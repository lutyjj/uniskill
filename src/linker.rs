use std::fs;
use std::path::{Path, PathBuf};

/// Represents what action the linker took for a single skill.
#[derive(Debug)]
pub struct SyncResult {
    pub skill_name: String,
    pub target: String,
    /// What happened: already correct, created, or updated
    pub status: SyncStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncStatus {
    /// Symlink already exists and points to the correct target
    Ok,
    /// Created a new symlink
    Created,
    /// Replaced an existing (wrong) symlink with a new one
    Updated,
    /// Could not create/update: target path has a conflict
    Conflict(PathBuf),
    /// Source no longer exists — broken symlink left in place
    Broken,
}

/// Ensure a single skill is symlinked from `source` to `target`.
pub fn ensure_skill_symlink(source: &Path, target: &Path) -> SyncResult {
    let skill_name = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let status = if !source.exists() {
        SyncStatus::Broken
    } else {
        apply_link(source, target).unwrap_or_else(|_| SyncStatus::Conflict(target.to_path_buf()))
    };
    SyncResult {
        skill_name,
        target: target.to_string_lossy().into_owned(),
        status,
    }
}

fn apply_link(source: &Path, target: &Path) -> std::io::Result<SyncStatus> {
    let source = if source.is_absolute() {
        source.to_path_buf()
    } else {
        std::env::current_dir()?.join(source)
    };
    let previous = match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() => Some(fs::read_link(target)?),
        Ok(_) => return Ok(SyncStatus::Conflict(target.to_path_buf())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if previous.as_ref() == Some(&source) {
        return Ok(SyncStatus::Ok);
    }
    let parent = target
        .parent()
        .ok_or_else(|| std::io::Error::other("link requires a parent directory"))?;
    fs::create_dir_all(parent)?;
    if let Some(previous) = previous {
        let staged = tempfile::tempdir_in(parent)?;
        let link = staged.path().join("link");
        create_symlink(&source, &link)?;
        if fs::read_link(target)? != previous {
            return Ok(SyncStatus::Conflict(target.to_path_buf()));
        }
        fs::rename(&link, target)?;
        Ok(SyncStatus::Updated)
    } else {
        create_symlink(&source, target)?;
        Ok(SyncStatus::Created)
    }
}

/// Create a symlink. Returns error if it fails.
pub(crate) fn create_symlink(source: &Path, target: &Path) -> Result<(), std::io::Error> {
    #[cfg(windows)]
    {
        // Windows needs admin or developer mode for symlinks
        std::os::windows::fs::symlink_dir(source, target)
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn failed_replacement_retains_the_existing_link() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let parent = temp.path().join("target");
        fs::create_dir(&parent).unwrap();
        let target = parent.join("demo");
        let previous = temp.path().join("previous");
        create_symlink(&previous, &target).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
        let result = ensure_skill_symlink(&source, &target);
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(result.status, SyncStatus::Conflict(_)));
        assert_eq!(fs::read_link(target).unwrap(), previous);
    }

    #[test]
    fn test_create_and_verify_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let target = tmp.path().join("link");

        fs::create_dir_all(&source).unwrap();
        fs::File::create(source.join("SKILL.md")).unwrap();

        let result = ensure_skill_symlink(&source, &target);

        assert_eq!(result.status, SyncStatus::Created);
        assert!(target.is_symlink());

        // Running again should be Idempotent (Ok)
        let result2 = ensure_skill_symlink(&source, &target);
        assert_eq!(result2.status, SyncStatus::Ok);
    }

    #[test]
    fn test_update_wrong_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let correct_source = tmp.path().join("correct");
        let wrong_target_path = tmp.path().join("wrong_source");
        let target_link = tmp.path().join("link");

        fs::create_dir_all(&correct_source).unwrap();
        fs::create_dir_all(&wrong_target_path).unwrap();

        // Create a symlink pointing to the wrong place
        std::os::unix::fs::symlink(&wrong_target_path, &target_link).unwrap();

        let result = ensure_skill_symlink(&correct_source, &target_link);

        assert_eq!(result.status, SyncStatus::Updated);
        assert!(target_link.is_symlink());
        // Verify it now points to the correct source
        let read_target = fs::read_link(&target_link).unwrap();
        assert_eq!(read_target, correct_source);
    }

    #[test]
    fn test_broken_symlink_replacement_is_updated() {
        let tmp = tempfile::tempdir().unwrap();
        let new_source = tmp.path().join("new_source");
        let target_link = tmp.path().join("link");

        fs::create_dir_all(&new_source).unwrap();

        // Create a broken symlink (target doesn't exist)
        let nonexistent = tmp.path().join("nonexistent");
        std::os::unix::fs::symlink(&nonexistent, &target_link).unwrap();

        let result = ensure_skill_symlink(&new_source, &target_link);

        assert_eq!(result.status, SyncStatus::Updated);
    }

    #[test]
    fn test_conflict_with_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let target = tmp.path().join("link");

        fs::create_dir_all(&source).unwrap();
        fs::write(&target, "not a symlink").unwrap();

        let result = ensure_skill_symlink(&source, &target);
        assert!(matches!(result.status, SyncStatus::Conflict(_)));
    }

    #[test]
    fn test_broken_source_returns_broken() {
        let tmp = tempfile::tempdir().unwrap();
        let non_existent_source = tmp.path().join("gone");
        let target = tmp.path().join("link");

        let result = ensure_skill_symlink(&non_existent_source, &target);

        assert_eq!(result.status, SyncStatus::Broken);
    }
}
