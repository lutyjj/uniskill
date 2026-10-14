use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

/// Physical storage roots shared by publication and managed-link ownership.
pub struct Layout {
    root: PathBuf,
    bundles: PathBuf,
    directories: BTreeSet<PathBuf>,
    reserved_directories: BTreeSet<PathBuf>,
    entries: BTreeSet<PathBuf>,
    aliases: BTreeSet<PathBuf>,
}

impl Layout {
    pub fn resolve(cache: &Path) -> Result<Self> {
        let root = crate::config::directory_path(cache)?;
        let bundles = crate::config::directory_path(&root.path.join("bundles"))?;
        let mut aliases = root.aliases;
        aliases.extend(bundles.aliases);
        Ok(Self {
            directories: BTreeSet::from([root.path.clone(), bundles.path.clone()]),
            reserved_directories: BTreeSet::new(),
            entries: BTreeSet::new(),
            root: root.path,
            bundles: bundles.path,
            aliases,
        })
    }

    pub fn reserve_directory(&mut self, path: &Path) -> Result<PathBuf> {
        let directory = crate::config::directory_path(path)?;
        self.directories.insert(directory.path.clone());
        self.aliases.extend(directory.aliases);
        Ok(directory.path)
    }

    pub fn reserve(&mut self, reservations: &crate::config::Reservations) -> Result<()> {
        for directory in &reservations.directories {
            let path = self.reserve_directory(directory)?;
            self.reserved_directories.insert(path);
        }
        for entry in &reservations.entries {
            let parent = crate::config::directory_path(
                entry
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("reserved entry requires a parent"))?,
            )?;
            let name = entry
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("reserved entry requires a file name"))?;
            self.entries.insert(parent.path.join(name));
            self.aliases.extend(parent.aliases);
        }
        Ok(())
    }

    pub fn bundle(&self, name: &str) -> Result<BundlePaths> {
        crate::config::validate_component(name)?;
        let published = self.bundles.join(name);
        Ok(BundlePaths {
            staging: sibling_path(&published, "staging")?,
            previous: sibling_path(&published, "previous")?,
            published,
        })
    }

    pub fn owns(&self, target: &Path) -> Result<bool> {
        let target = crate::config::destination_path(target)?;
        Ok(target.starts_with(&self.root) || target.starts_with(&self.bundles))
    }

    pub fn conflicts(&self, entry: &Path) -> bool {
        self.directories
            .iter()
            .chain(self.entries.iter())
            .chain(self.aliases.iter())
            .any(|root| entry.starts_with(root) || root.starts_with(entry))
    }

    pub fn invalidates_storage(&self, entry: &Path) -> bool {
        self.aliases
            .iter()
            .chain(self.entries.iter())
            .any(|root| root.starts_with(entry))
            || self
                .reserved_directories
                .iter()
                .any(|root| root.starts_with(entry) || entry.starts_with(root))
    }
}

/// Every directory mutated while assembling, publishing, or recovering a bundle.
pub struct BundlePaths {
    pub published: PathBuf,
    pub staging: PathBuf,
    pub previous: PathBuf,
}

impl BundlePaths {
    pub fn roots(&self) -> impl Iterator<Item = &Path> {
        [&self.published, &self.staging, &self.previous]
            .into_iter()
            .map(PathBuf::as_path)
    }

    pub fn overlaps(&self, entry: &Path) -> bool {
        self.roots()
            .any(|root| entry.starts_with(root) || root.starts_with(entry))
    }
}

fn sibling_path(path: &Path, suffix: &str) -> Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("cache path has no file name: {}", path.display()))?
        .to_string_lossy();
    let component = format!(".{name}.{suffix}");
    if component.len() <= 255 {
        return Ok(path.with_file_name(component));
    }
    use sha2::{Digest, Sha256};
    // Double-dot names cannot collide with the non-hidden public bundle keys.
    Ok(path.with_file_name(format!("..{:x}.{suffix}", Sha256::digest(name.as_bytes()))))
}
