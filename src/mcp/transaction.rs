use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::persistence::atomic_write;

const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Update {
    pub path: PathBuf,
    pub before: Option<String>,
    pub after: String,
    pub mode: u32,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    version: u32,
    updates: Vec<Update>,
}

pub fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    crate::persistence::read(path, MAX_BYTES)
}

pub fn mode(path: &Path) -> Result<u32> {
    match fs::metadata(path) {
        Ok(metadata) => {
            if metadata.permissions().readonly() {
                bail!("configuration is read-only: {}", path.display());
            }
            Ok(metadata.permissions().mode() & 0o777)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0o600),
        Err(error) => Err(error.into()),
    }
}

pub fn lock(state_dir: &Path) -> Result<File> {
    fs::create_dir_all(state_dir)?;
    let metadata = fs::symlink_metadata(state_dir)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("MCP state directory is not a regular directory");
    }
    if metadata.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(state_dir, fs::Permissions::from_mode(0o700))?;
    }
    lock_file(&state_dir.join("lock"))
}

fn lock_file(path: &Path) -> Result<File> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            bail!("MCP lock is not a regular file");
        }
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.lock()?;
    Ok(file)
}

/// Persistent sidecars keep the lock identity stable across native file replacements.
pub fn lock_destinations(paths: &std::collections::BTreeSet<PathBuf>) -> Result<Vec<File>> {
    paths
        .iter()
        .map(|path| {
            let parent = path.parent().expect("absolute file has a parent");
            fs::create_dir_all(parent)?;
            let digest = Sha256::digest(path.as_os_str().as_encoded_bytes());
            lock_file(&parent.join(format!(".uniskill-mcp-{digest:x}.lock")))
        })
        .collect()
}

pub fn pending(state_dir: &Path) -> bool {
    state_dir.join("journal.json").exists()
}

fn read_journal(state_dir: &Path) -> Result<Option<Journal>> {
    let Some(bytes) = crate::persistence::read(&state_dir.join("journal.json"), MAX_JOURNAL_BYTES)?
    else {
        return Ok(None);
    };
    let journal: Journal = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow!("invalid MCP transaction journal; recovery requires inspection"))?;
    if journal.version != 1 {
        bail!("unsupported MCP transaction journal version");
    }
    if journal
        .updates
        .iter()
        .any(|update| !update.path.is_absolute())
    {
        bail!("MCP transaction journal contains a relative path");
    }
    Ok(Some(journal))
}

pub fn journal_updates(state_dir: &Path) -> Result<Vec<Update>> {
    Ok(read_journal(state_dir)?
        .map(|journal| journal.updates)
        .unwrap_or_default())
}

/// Roll back only bytes from this journal. A subsequent external edit is never replaced.
pub fn recover(state_dir: &Path) -> Result<Vec<PathBuf>> {
    let Some(journal) = read_journal(state_dir)? else {
        return Ok(Vec::new());
    };
    let path = state_dir.join("journal.json");
    let mut preserved = Vec::new();
    for update in journal.updates.iter().rev() {
        let current = read(&update.path)?;
        if current.as_deref() == update.before.as_deref().map(str::as_bytes) {
            continue;
        }
        if current.as_deref() != Some(update.after.as_bytes()) {
            preserved.push(update.path.clone());
            continue;
        }
        if let Some(before) = &update.before {
            atomic_write(&update.path, before.as_bytes(), update.mode)?;
        } else {
            fs::remove_file(&update.path)?;
            File::open(update.path.parent().expect("update has a parent"))?.sync_all()?;
        }
    }
    fs::remove_file(&path)?;
    File::open(state_dir)?.sync_all()?;
    Ok(preserved)
}

pub fn commit(state_dir: &Path, updates: Vec<Update>) -> Result<()> {
    if updates.is_empty() {
        return Ok(());
    }
    if updates
        .iter()
        .any(|update| update.after.len() as u64 > MAX_BYTES)
    {
        bail!("MCP output exceeds its size limit");
    }
    let path = state_dir.join("journal.json");
    let journal = Journal {
        version: 1,
        updates,
    };
    let bytes = serde_json::to_vec(&journal)?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        bail!("MCP transaction exceeds 64 MiB");
    }
    atomic_write(&path, &bytes, 0o600)?;
    let result = (|| {
        for update in &journal.updates {
            if read(&update.path)?.as_deref() != update.before.as_deref().map(str::as_bytes) {
                bail!(
                    "MCP configuration changed during sync: {}",
                    update.path.display()
                );
            }
            atomic_write(&update.path, update.after.as_bytes(), update.mode)?;
        }
        fs::remove_file(&path)?;
        File::open(state_dir)?.sync_all()?;
        Ok(())
    })();
    if let Err(error) = result {
        recover(state_dir).context("MCP rollback needs recovery")?;
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_edit_rolls_back_prior_writes_and_preserves_the_editor() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        fs::create_dir_all(&state).unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        fs::write(&a, b"original-a").unwrap();
        fs::write(&b, b"external-b").unwrap();
        let updates = vec![
            Update {
                path: a.clone(),
                before: Some("original-a".into()),
                after: "new-a".into(),
                mode: 0o600,
            },
            Update {
                path: b.clone(),
                before: Some("original-b".into()),
                after: "new-b".into(),
                mode: 0o600,
            },
        ];
        assert!(commit(&state, updates).is_err());
        assert_eq!(fs::read(&a).unwrap(), b"original-a");
        assert_eq!(fs::read(&b).unwrap(), b"external-b");
    }

    #[test]
    fn interrupted_commit_restores_native_file_and_ownership_state() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path();
        let target = state.join("native");
        let manifest = state.join("state.json");
        let updates = vec![
            Update {
                path: target.clone(),
                before: Some("old".into()),
                after: "new".into(),
                mode: 0o600,
            },
            Update {
                path: manifest.clone(),
                before: None,
                after: "owned".into(),
                mode: 0o600,
            },
        ];
        fs::write(&target, b"new").unwrap();
        fs::write(&manifest, b"owned").unwrap();
        atomic_write(
            &state.join("journal.json"),
            &serde_json::to_vec(&Journal {
                version: 1,
                updates,
            })
            .unwrap(),
            0o600,
        )
        .unwrap();
        recover(state).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert!(!manifest.exists());
        assert!(!pending(state));
    }
}
