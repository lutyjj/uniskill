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
pub fn ensure_skill_symlink(source: &Path, target_dir: &Path) -> SyncResult {
    let target = PathBuf::from(target_dir);
    let skill_name = source
        .file_name()
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

    // Check if there is an existing symlink — if so, "Updated" not "Created"
    let existed = match fs::symlink_metadata(&target) {
        Ok(meta) => meta.file_type().is_symlink(),
        Err(_) => false,
    };

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
        if !parent.exists() && fs::create_dir_all(parent).is_err() {
            return SyncResult {
                skill_name: skill_name.clone(),
                target: target.to_string_lossy().to_string(),
                status: SyncStatus::Conflict(target.clone()),
            };
        }
    }

    // Create the symlink (absolute source for portability)
    let abs_source = if source.is_absolute() {
        source.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|c| c.join(source))
            .unwrap_or_else(|_| source.to_path_buf())
    };

    match create_symlink(&abs_source, &target) {
        Ok(()) => SyncResult {
            skill_name,
            target: target.to_string_lossy().to_string(),
            status: if existed {
                SyncStatus::Updated
            } else {
                SyncStatus::Created
            },
        },
        Err(_e) => SyncResult {
            skill_name,
            target: target.to_string_lossy().to_string(),
            status: SyncStatus::Conflict(target),
        },
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

/// Sync a full bundle: discover all skills and create symlinks for each.
pub fn sync_bundle(source: &Path, pattern: &str) -> Vec<SyncResult> {
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
        if !crate::skill::is_skill_dir(&skill_path) {
            // Skip files and incomplete directories.
            continue;
        }

        // Resolve the install path using the harness pattern
        let skill_name = skill_path
            .file_name()
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

    #[test]
    fn test_full_sync_flow_creates_real_links() {
        let tmp = tempfile::tempdir().unwrap();

        // Create source bundle structure
        let bundle_dir = tmp.path().join("bundle");
        let skills_dir = bundle_dir.join("skills");
        fs::create_dir_all(skills_dir.join("caveman")).unwrap();
        fs::File::create(skills_dir.join("caveman").join("SKILL.md")).unwrap();
        fs::create_dir_all(skills_dir.join("code-design")).unwrap();
        fs::File::create(skills_dir.join("code-design").join("SKILL.md")).unwrap();

        // Create target harness directory structure
        let harness_target = tmp.path().join("harness");
        let target_skills = harness_target.join("skills");
        fs::create_dir_all(&target_skills).unwrap();

        // Run sync with absolute pattern (no env vars to expand)
        let pattern = format!("{}", target_skills.to_string_lossy()) + "/{name}";
        let results = sync_bundle(&bundle_dir, &pattern);

        assert_eq!(results.len(), 2);
        for result in &results {
            assert_eq!(result.status, SyncStatus::Created);
            let link_path = PathBuf::from(&result.target);
            assert!(link_path.is_symlink());
            // Verify symlink points back to source
            let actual_target = fs::read_link(&link_path).unwrap();
            assert!(actual_target.starts_with(&skills_dir));
        }
    }

    #[test]
    fn test_sync_bundle_idempotent_on_second_run() {
        let tmp = tempfile::tempdir().unwrap();

        // Create source bundle
        let bundle_dir = tmp.path().join("bundle");
        let skills_dir = bundle_dir.join("skills");
        fs::create_dir_all(skills_dir.join("test-skill")).unwrap();
        fs::File::create(skills_dir.join("test-skill").join("SKILL.md")).unwrap();

        // First run: create links
        let target_skills = tmp.path().join("harness").join("skills");
        fs::create_dir_all(&target_skills).unwrap();
        let pattern = format!("{}", target_skills.to_string_lossy()) + "/{name}";
        let results1 = sync_bundle(&bundle_dir, &pattern);
        assert_eq!(results1[0].status, SyncStatus::Created);

        // Second run: should be Ok
        let results2 = sync_bundle(&bundle_dir, &pattern);
        assert_eq!(results2[0].status, SyncStatus::Ok);
    }

    #[test]
    fn test_sync_bundle_missing_skills_dir_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle_dir = tmp.path().join("bundle_no_skills");
        fs::create_dir_all(&bundle_dir).unwrap();

        let results = sync_bundle(&bundle_dir, "/any/target/{name}");
        assert!(results.is_empty());
    }

    #[test]
    fn test_sync_bundle_skips_non_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle_dir = tmp.path().join("bundle");
        let skills_dir = bundle_dir.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();

        // Create a skill directory (should be picked up)
        fs::create_dir_all(skills_dir.join("good-skill")).unwrap();
        fs::File::create(skills_dir.join("good-skill").join("SKILL.md")).unwrap();
        // Create a regular file (should be skipped)
        fs::write(skills_dir.join("not-a-skill.txt"), "ignore me").unwrap();

        let results = sync_bundle(&bundle_dir, "/any/target/{name}");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_sync_bundle_skips_directories_without_skill_md() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle_dir = tmp.path().join("bundle");
        let skills_dir = bundle_dir.join("skills");
        fs::create_dir_all(skills_dir.join("incomplete")).unwrap();

        let results = sync_bundle(&bundle_dir, "/any/target/{name}");

        assert!(results.is_empty());
    }

    #[test]
    fn test_expand_pattern() {
        let expanded = expand_pattern("$HOME/.agents/skills/{name}", "caveman");
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expanded, format!("{}/.agents/skills/caveman", home));
    }

    #[test]
    fn test_expand_pattern_no_placeholder() {
        let expanded = expand_pattern("/static/path/to/skills", "anything");
        assert_eq!(expanded, "/static/path/to/skills");
    }
}
