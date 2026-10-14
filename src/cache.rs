use std::path::{Path, PathBuf};

use anyhow::Result;

/// Physical storage roots shared by publication and managed-link ownership.
pub struct Layout {
    root: PathBuf,
    bundles: PathBuf,
}

impl Layout {
    pub fn resolve(cache: &Path) -> Result<Self> {
        fn directory(path: &Path) -> Result<PathBuf> {
            let entry = crate::config::destination_path(&path.join(".uniskill-path"))?;
            Ok(entry
                .parent()
                .expect("directory probe has parent")
                .to_path_buf())
        }
        let root = directory(cache)?;
        let bundles = directory(&root.join("bundles"))?;
        Ok(Self { root, bundles })
    }

    pub fn bundle(&self, name: &str) -> Result<PathBuf> {
        crate::config::validate_component(name)?;
        Ok(self.bundles.join(name))
    }

    pub fn owns(&self, target: &Path) -> Result<bool> {
        let target = crate::config::destination_path(target)?;
        Ok(target.starts_with(&self.root) || target.starts_with(&self.bundles))
    }
}
