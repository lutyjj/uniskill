use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;

pub fn lock(cache_dir: &Path) -> Result<File> {
    fs::create_dir_all(cache_dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(cache_dir.join("sync.lock"))?;
    file.lock()?;
    Ok(file)
}

/// One symlink uniskill created, and therefore owns and may prune.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedLink {
    /// Absolute path of the harness symlink.
    pub path: String,
    pub skill: String,
    /// Stable harness key the link belongs to.
    pub harness: String,
    /// Bundle the link came from.
    pub bundle: String,
}

/// Record of every link uniskill installed on the last sync. Lets a later sync
/// prune links whose skill or bundle has since left the config.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub links: Vec<ManagedLink>,
}

impl Manifest {
    /// Only an absent manifest means no ownership. Unreadable state fails closed.
    pub fn load(cache_dir: &Path) -> Result<Self> {
        let path = Self::path(cache_dir);
        match crate::persistence::read(&path, MAX_MANIFEST_BYTES)? {
            Some(bytes) => {
                toml::from_str(std::str::from_utf8(&bytes).context("skill ownership is not UTF-8")?)
                    .context("invalid skill ownership manifest")
            }
            None => Ok(Self::default()),
        }
    }

    /// Persist the manifest next to the assembled bundles.
    pub fn save(&self, cache_dir: &Path) -> Result<()> {
        let path = Self::path(cache_dir);
        let body = toml::to_string(self).context("failed to serialize uniskill state")?;
        if body.len() as u64 > MAX_MANIFEST_BYTES {
            anyhow::bail!("skill ownership manifest exceeds its size limit");
        }
        let previous = crate::persistence::read(&path, MAX_MANIFEST_BYTES)?;
        if (self.links.is_empty() && previous.is_none())
            || previous.as_deref() == Some(body.as_bytes())
        {
            return Ok(());
        }
        crate::persistence::atomic_write(&path, body.as_bytes(), 0o600)
            .with_context(|| format!("failed to write {}", path.display()))
    }

    fn path(cache_dir: &Path) -> PathBuf {
        cache_dir.join("state.toml")
    }
}

/// True when `path` is a symlink uniskill owns — one pointing into `cache_dir`.
/// Anything that is not a symlink, or points elsewhere, is left alone.
pub fn is_managed_symlink(path: &Path, cache_dir: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            match fs::read_link(path) {
                Ok(target) => {
                    let target = if target.is_absolute() {
                        target
                    } else {
                        path.parent()
                            .context("managed link requires a parent")?
                            .join(target)
                    };
                    Ok(crate::config::destination_path(&target)?
                        .starts_with(cache_dir.canonicalize()?))
                }
                Err(error) => Err(error.into()),
            }
        }
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Remove `path` only if it is a uniskill-managed symlink. Returns whether it
/// was removed, so callers can report and count prunes.
pub fn remove_if_managed(path: &Path, cache_dir: &Path) -> Result<bool> {
    if !is_managed_symlink(path, cache_dir)? {
        return Ok(false);
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn test_manifest_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path();
        let manifest = Manifest {
            links: vec![ManagedLink {
                path: "/home/u/.agents/skills/code-design".to_string(),
                skill: "code-design".to_string(),
                harness: "pi".to_string(),
                bundle: "generic".to_string(),
            }],
        };
        manifest.save(cache).unwrap();
        let loaded = Manifest::load(cache).unwrap();
        assert_eq!(loaded.links, manifest.links);
    }

    #[test]
    fn test_missing_manifest_loads_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(Manifest::load(tmp.path()).unwrap().links.is_empty());
    }

    #[test]
    fn test_is_managed_symlink_only_true_for_cache_links() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        fs::create_dir_all(cache.join("bundles")).unwrap();

        // A symlink into the cache is managed.
        let managed = tmp.path().join("managed");
        symlink(cache.join("bundles").join("x"), &managed).unwrap();
        assert!(is_managed_symlink(&managed, &cache).unwrap());

        // A symlink pointing elsewhere is not.
        let foreign = tmp.path().join("foreign");
        symlink(tmp.path().join("somewhere-else"), &foreign).unwrap();
        assert!(!is_managed_symlink(&foreign, &cache).unwrap());

        // A real directory is not.
        let real = tmp.path().join("real");
        fs::create_dir_all(&real).unwrap();
        assert!(!is_managed_symlink(&real, &cache).unwrap());

        // A missing path is not.
        assert!(!is_managed_symlink(&tmp.path().join("gone"), &cache).unwrap());
    }

    #[test]
    fn test_remove_if_managed_leaves_foreign_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        fs::create_dir_all(&cache).unwrap();

        // Foreign real dir (e.g. a hand-placed skill) is never removed.
        let foreign = tmp.path().join("pi-extensions");
        fs::create_dir_all(&foreign).unwrap();
        assert!(!remove_if_managed(&foreign, &cache).unwrap());
        assert!(foreign.exists());

        // A managed symlink is removed.
        let managed = tmp.path().join("managed");
        symlink(cache.join("skill"), &managed).unwrap();
        assert!(remove_if_managed(&managed, &cache).unwrap());
        assert!(fs::symlink_metadata(&managed).is_err());
    }

    #[test]
    fn ownership_resolves_cache_aliases_and_relative_targets_without_following_live_skills() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        fs::create_dir_all(cache.join("skills")).unwrap();
        let alias = temp.path().join("alias");
        symlink(&cache, &alias).unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        symlink(&source, cache.join("skills/demo")).unwrap();
        let link = temp.path().join("installed");
        symlink("alias/skills/demo", &link).unwrap();
        assert!(remove_if_managed(&link, &cache).unwrap());
        assert!(source.is_dir());
        let foreign = temp.path().join("foreign");
        symlink(cache.join("../source"), &foreign).unwrap();
        assert!(!remove_if_managed(&foreign, &cache).unwrap());
        assert!(foreign.is_symlink());
    }
}
