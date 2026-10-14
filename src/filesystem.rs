use std::env;
use std::path::{Path, PathBuf};

/// Resolve destination aliases through existing parents without following the
/// final entry, which may be a managed symlink or a foreign file.
pub(crate) fn destination_path(path: &Path) -> anyhow::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    let name = absolute
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("destination requires a file name"))?;
    let parent = directory_path(absolute.parent().expect("absolute path has parent"))?;
    Ok(parent.path.join(name))
}

/// A directory's physical location and every symlink entry used to reach it.
pub(crate) struct DirectoryPath {
    pub path: PathBuf,
    pub aliases: std::collections::BTreeSet<PathBuf>,
}

/// Filesystem resources another reconciler needs to keep reachable.
#[derive(Debug, Default)]
pub(crate) struct Reservations {
    pub directories: Vec<PathBuf>,
    pub entries: Vec<PathBuf>,
}

pub(crate) fn directory_path(path: &Path) -> anyhow::Result<DirectoryPath> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    let mut parent = PathBuf::new();
    let mut aliases = std::collections::BTreeSet::new();
    for component in absolute.components() {
        match component {
            std::path::Component::ParentDir => {
                parent.pop();
            }
            std::path::Component::CurDir => {}
            component => {
                parent.push(component.as_os_str());
                match std::fs::symlink_metadata(&parent) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        // Validate the complete chain before recursively retaining its entries.
                        // This also rejects cycles and dangling directory aliases.
                        parent.canonicalize()?;
                        aliases.insert(parent.clone());
                        let target = std::fs::read_link(&parent)?;
                        let target = if target.is_absolute() {
                            target
                        } else {
                            parent.parent().expect("symlink has parent").join(target)
                        };
                        let resolved = directory_path(&target)?;
                        parent = resolved.path;
                        aliases.extend(resolved.aliases);
                    }
                    Ok(metadata) if !metadata.is_dir() => anyhow::bail!(
                        "destination parent is not a directory: {}",
                        parent.display()
                    ),
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
    Ok(DirectoryPath {
        path: parent,
        aliases,
    })
}

/// Resolved resources and the aliases required to keep them reachable.
#[derive(Default)]
pub(crate) struct Protection {
    directories: std::collections::BTreeSet<PathBuf>,
    entries: std::collections::BTreeSet<PathBuf>,
    aliases: std::collections::BTreeSet<PathBuf>,
}

impl Protection {
    pub fn reserve_directory(&mut self, path: &Path) -> anyhow::Result<PathBuf> {
        let directory = directory_path(path)?;
        self.directories.insert(directory.path.clone());
        self.aliases.extend(directory.aliases);
        Ok(directory.path)
    }

    pub fn reserve(&mut self, resources: &Reservations) -> anyhow::Result<()> {
        for directory in &resources.directories {
            self.reserve_directory(directory)?;
        }
        for entry in &resources.entries {
            let parent = directory_path(
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

    pub fn conflicts(&self, entry: &Path) -> bool {
        self.directories
            .iter()
            .chain(&self.entries)
            .chain(&self.aliases)
            .any(|root| entry.starts_with(root) || root.starts_with(entry))
    }

    pub fn invalidates_alias(&self, entry: &Path) -> bool {
        self.aliases.iter().any(|alias| alias.starts_with(entry))
    }

    /// A file write must neither mutate storage nor depend on an alias inside it.
    pub fn validate_write(&self, entry: &Path) -> anyhow::Result<()> {
        let path = destination_path(entry)?;
        let parent = directory_path(
            entry
                .parent()
                .ok_or_else(|| anyhow::anyhow!("file requires a parent"))?,
        )?;
        if self.conflicts(&path)
            || parent.aliases.iter().any(|alias| {
                self.directories
                    .iter()
                    .any(|directory| alias.starts_with(directory))
            })
        {
            anyhow::bail!(
                "file target overlaps reserved filesystem storage: {}",
                entry.display()
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn destination_identity_resolves_symlink_parents_before_parent_components() {
        let temp = crate::test_support::tempdir().unwrap();
        let physical = temp.path().join("physical");
        std::fs::create_dir_all(physical.join("child")).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(physical.join("child"), &alias).unwrap();
        assert_eq!(
            destination_path(&alias.join("../target/demo")).unwrap(),
            physical.join("target/demo")
        );
    }
}
