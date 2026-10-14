use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use anyhow::{anyhow, bail, Result};

/// Publish complete bytes in one rename, then make the directory entry durable.
pub fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("file requires a parent directory"))?;
    fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged.persist(path).map_err(|error| error.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn read(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                bail!(
                    "configuration path is not a regular file: {}",
                    path.display()
                );
            }
            if metadata.len() > limit {
                bail!("configuration exceeds its size limit: {}", path.display());
            }
            let mut bytes = Vec::new();
            File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > limit {
                bail!("configuration exceeds its size limit: {}", path.display());
            }
            Ok(Some(bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
