use std::path::Path;

pub(crate) fn is_skill_dir(path: &Path) -> bool {
    path.is_dir() && path.join("SKILL.md").is_file()
}
