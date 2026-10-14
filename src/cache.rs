use std::path::{Path, PathBuf};

use anyhow::Result;

/// Physical storage roots shared by publication and managed-link ownership.
pub struct Layout {
    root: PathBuf,
    bundles: PathBuf,
    storage: crate::filesystem::Protection,
    dependencies: crate::filesystem::Protection,
}

/// Namespaces that native config writers must not mutate or depend on.
pub fn storage(cache: &Path, state: &Path) -> Result<crate::filesystem::Protection> {
    let mut layout = Layout::resolve(cache)?;
    layout.reserve_directory(state)?;
    Ok(layout.storage)
}

impl Layout {
    pub fn resolve(cache: &Path) -> Result<Self> {
        let mut storage = crate::filesystem::Protection::default();
        let root = storage.reserve_directory(cache)?;
        let bundles = storage.reserve_directory(&root.join("bundles"))?;
        Ok(Self {
            root,
            bundles,
            storage,
            dependencies: Default::default(),
        })
    }

    pub fn reserve_directory(&mut self, path: &Path) -> Result<PathBuf> {
        self.storage.reserve_directory(path)
    }

    pub fn reserve(&mut self, reservations: &crate::filesystem::Reservations) -> Result<()> {
        self.dependencies.reserve(reservations)
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
        let target = crate::filesystem::destination_path(target)?;
        Ok(target.starts_with(&self.root) || target.starts_with(&self.bundles))
    }

    pub fn conflicts(&self, entry: &Path) -> bool {
        self.storage.conflicts(entry) || self.dependencies.conflicts(entry)
    }

    pub fn invalidates_storage(&self, entry: &Path) -> bool {
        self.storage.invalidates_alias(entry) || self.dependencies.conflicts(entry)
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
