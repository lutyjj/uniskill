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

#[derive(Debug, Clone)]
#[derive(PartialEq)]
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
pub fn ensure_skill_symlink(source: &Path, target_dir: &Path) -> SyncResult {
    let target = PathBuf::from(target_dir);
    let skill_name = source.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    // Source must exist
    if !source.exists() {
        return SyncResult {
            skill_name: skill_name.clone(),
            target: target.to_string_lossy().to_string(),
            status: SyncStatus::Broken,
        };
    }

    // If target already exists and is a symlink...
    if let Ok(metadata) = fs::symlink_metadata(&target) {
        if metadata.file_type().is_symlink() {
            // Check if it points to the right place
            if let Ok(current_target) = fs::read_link(&target) {
                if current_target == source {
                    return SyncResult {
                        skill_name,
                        target: target.to_string_lossy().to_string(),
                        status: SyncStatus::Ok,
                    };
                }
                // Points somewhere else — replace it
                let _ = fs::remove_file(&target);
            } else {
                // Broken symlink or unreadable — remove and recreate
                let _ = fs::remove_file(&target);
            }
        } else if target.exists() {
            // Not a symlink, and it's not empty
            return SyncResult {
                skill_name: skill_name.clone(),
                target: target.to_string_lossy().to_string(),
                status: SyncStatus::Conflict(target.clone()),
            };
        }
    }

    // Create parent directories if needed
    if let Some(parent) = target.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).unwrap_or(());
        }
    }

    // Create the symlink (absolute source for portability)
    let abs_source = if source.is_absolute() {
        source.to_path_buf()
    } else {
        std::env::current_dir().map(|c| c.join(source)).unwrap_or_else(|_| source.to_path_buf())
    };

    match create_symlink(&abs_source, &target) {
        Ok(()) => SyncResult {
            skill_name,
            target: target.to_string_lossy().to_string(),
            status: SyncStatus::Created,
        },
        Err(_e) => SyncResult {
            skill_name,
            target: target.to_string_lossy().to_string(),
            status: SyncStatus::Conflict(target),
        },
    }
}

/// Create a symlink. Returns error if it fails.
fn create_symlink(source: &Path, target: &Path) -> Result<(), std::io::Error> {
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

/// Sync a full bundle: discover all skills and create symlinks for each.
pub fn sync_bundle(source: &Path, pattern: &str, _harness_name: &str) -> Vec<SyncResult> {
    let skills_dir = source.join("skills");
    if !skills_dir.exists() {
        return vec![];
    }

    let mut results = Vec::new();

    let entries = match fs::read_dir(&skills_dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let skill_path = entry.path();
        if !skill_path.is_dir() {
            continue; // skip non-directories (files like .gitkeep, etc.)
        }

        // Resolve the install path using the harness pattern
        let skill_name = skill_path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        let target = expand_pattern(pattern, &skill_name);

        let result = ensure_skill_symlink(&skill_path, Path::new(&target));
        results.push(result);
    }

    results
}

/// Expand the harness pattern: replace {name} and env vars.
fn expand_pattern(pattern: &str, skill_name: &str) -> String {
    let with_name = pattern.replace("{name}", skill_name);
    crate::config::expand_env_vars(&with_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
    fn test_update_broken_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let old_source = tmp.path().join("old_source");
        let new_source = tmp.path().join("new_source");
        let target = tmp.path().join("link");

        fs::create_dir_all(&old_source).unwrap();
        // Create symlink pointing to nonexistent old source (broken)
        std::os::unix::fs::symlink(&old_source, &target).unwrap();
        // Remove old source so it's truly broken
        fs::remove_dir_all(&old_source).unwrap();

        fs::create_dir_all(&new_source).unwrap();
        let result = ensure_skill_symlink(&new_source, &target);

        assert_eq!(result.status, SyncStatus::Created);
        assert!(target.is_symlink());
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
    fn test_sync_bundle_discovers_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle_dir = tmp.path().join("bundle");
        let skills_dir = bundle_dir.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();

        // Create two skill dirs
        fs::create_dir_all(skills_dir.join("caveman")).unwrap();
        fs::File::create(skills_dir.join("caveman").join("SKILL.md")).unwrap();
        fs::create_dir_all(skills_dir.join("code-design")).unwrap();
        fs::File::create(skills_dir.join("code-design").join("SKILL.md")).unwrap();

        let results = sync_bundle(
            &bundle_dir,
            "/tmp/harness/skills/{name}",
            "pi",
        );

        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_expand_pattern() {
        let expanded = expand_pattern("$HOME/.agents/skills/{name}", "caveman");
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expanded, format!("{}/.agents/skills/caveman", home));
    }
}
