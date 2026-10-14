use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One symlink uniskill created, and therefore owns and may prune.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct Manifest {
    #[serde(default)]
    pub links: Vec<ManagedLink>,
}

impl Manifest {
    /// Load the manifest for a cache directory, or an empty one if absent or
    /// unreadable (a missing/corrupt manifest must not break a sync).
    pub fn load(cache_dir: &Path) -> Self {
        fs::read_to_string(Self::path(cache_dir))
            .ok()
            .and_then(|body| toml::from_str(&body).ok())
            .unwrap_or_default()
    }

    /// Persist the manifest next to the assembled bundles.
    pub fn save(&self, cache_dir: &Path) -> Result<()> {
        let path = Self::path(cache_dir);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let body = toml::to_string(self).context("failed to serialize uniskill state")?;
        fs::write(&path, body).with_context(|| format!("failed to write {}", path.display()))
    }

    fn path(cache_dir: &Path) -> PathBuf {
        cache_dir.join("state.toml")
    }
}

/// True when `path` is a symlink uniskill owns — one pointing into `cache_dir`.
/// Anything that is not a symlink, or points elsewhere, is left alone.
pub fn is_managed_symlink(path: &Path, cache_dir: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => match fs::read_link(path) {
            Ok(target) => target.starts_with(cache_dir),
            Err(_) => false,
        },
        _ => false,
    }
}

/// Remove `path` only if it is a uniskill-managed symlink. Returns whether it
/// was removed, so callers can report and count prunes.
pub fn remove_if_managed(path: &Path, cache_dir: &Path) -> bool {
    is_managed_symlink(path, cache_dir) && fs::remove_file(path).is_ok()
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
        let loaded = Manifest::load(cache);
        assert_eq!(loaded.links, manifest.links);
    }

    #[test]
    fn test_missing_manifest_loads_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(Manifest::load(tmp.path()).links.is_empty());
    }

    #[test]
    fn test_is_managed_symlink_only_true_for_cache_links() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        fs::create_dir_all(cache.join("bundles")).unwrap();

        // A symlink into the cache is managed.
        let managed = tmp.path().join("managed");
        symlink(cache.join("bundles").join("x"), &managed).unwrap();
        assert!(is_managed_symlink(&managed, &cache));

        // A symlink pointing elsewhere is not.
        let foreign = tmp.path().join("foreign");
        symlink(tmp.path().join("somewhere-else"), &foreign).unwrap();
        assert!(!is_managed_symlink(&foreign, &cache));

        // A real directory is not.
        let real = tmp.path().join("real");
        fs::create_dir_all(&real).unwrap();
        assert!(!is_managed_symlink(&real, &cache));

        // A missing path is not.
        assert!(!is_managed_symlink(&tmp.path().join("gone"), &cache));
    }

    #[test]
    fn test_remove_if_managed_leaves_foreign_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        fs::create_dir_all(&cache).unwrap();

        // Foreign real dir (e.g. a hand-placed skill) is never removed.
        let foreign = tmp.path().join("pi-extensions");
        fs::create_dir_all(&foreign).unwrap();
        assert!(!remove_if_managed(&foreign, &cache));
        assert!(foreign.exists());

        // A managed symlink is removed.
        let managed = tmp.path().join("managed");
        symlink(cache.join("skill"), &managed).unwrap();
        assert!(remove_if_managed(&managed, &cache));
        assert!(fs::symlink_metadata(&managed).is_err());
    }
}
