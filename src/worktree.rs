use std::path::{Path, PathBuf};
use std::process::Command;

/// Git worktree topology for a sync.
///
/// Harness patterns are written against a repository's *main* worktree (for
/// example `$HOME/workplace/proj/.claude/skills/{name}`). A linked worktree —
/// the kind Claude Code, Codex, or `git worktree add` create — is a fresh
/// checkout of tracked files only, so uniskill's generated skill symlinks never
/// appear in it. This type carries the two roots needed to retarget those
/// patterns onto the linked worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeContext {
    /// Absolute path of the linked worktree being synced into.
    pub worktree_root: PathBuf,
    /// Absolute path of the repository's main worktree.
    pub main_root: PathBuf,
}

impl WorktreeContext {
    /// Detect the worktree topology for `dir`.
    ///
    /// Returns `None` when `dir` is not inside a git worktree, or when it is the
    /// repository's *main* worktree — in that case a normal sync already targets
    /// the right paths and there is nothing to retarget.
    pub fn detect(dir: &Path) -> Option<Self> {
        let worktree_root = git_toplevel(dir)?;
        let main_root = main_worktree(dir)?;
        if worktree_root == main_root {
            return None;
        }
        Some(Self {
            worktree_root,
            main_root,
        })
    }

    /// Retarget a harness install pattern from the main worktree onto this
    /// linked worktree.
    ///
    /// `expanded_pattern` must already have its environment variables expanded
    /// (so it is an absolute path with a trailing `{name}` placeholder). Returns
    /// `None` when the pattern does not point inside the repository's main
    /// worktree — a machine-global harness (`$HOME/.claude/skills`) or a harness
    /// belonging to a different repository is not owned by this worktree and is
    /// left untouched.
    pub fn retarget(&self, expanded_pattern: &str) -> Option<String> {
        let pattern = Path::new(expanded_pattern);
        // Already inside the linked worktree — nothing to rewrite.
        if pattern.starts_with(&self.worktree_root) {
            return None;
        }
        let rel = pattern.strip_prefix(&self.main_root).ok()?;
        Some(self.worktree_root.join(rel).to_string_lossy().into_owned())
    }
}

/// Absolute root of the worktree containing `dir`, or `None` if `dir` is not in
/// a git worktree.
fn git_toplevel(dir: &Path) -> Option<PathBuf> {
    let out = git_stdout(dir, &["rev-parse", "--show-toplevel"])?;
    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

/// Absolute root of the repository's main worktree. `git worktree list` always
/// reports the main worktree first.
fn main_worktree(dir: &Path) -> Option<PathBuf> {
    let out = git_stdout(dir, &["worktree", "list", "--porcelain"])?;
    for line in out.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            let path = path.trim();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    None
}

/// Run `git -C dir <args>` and return trimmed stdout on success, `None` on any
/// failure (not a repo, git missing, non-zero exit).
fn git_stdout(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(main: &str, worktree: &str) -> WorktreeContext {
        WorktreeContext {
            worktree_root: PathBuf::from(worktree),
            main_root: PathBuf::from(main),
        }
    }

    #[test]
    fn retargets_pattern_under_main_root() {
        let c = ctx("/home/u/proj", "/home/u/proj/.git/worktrees-checkout/feat");
        let out = c.retarget("/home/u/proj/.agents/skills/{name}");
        assert_eq!(
            out.as_deref(),
            Some("/home/u/proj/.git/worktrees-checkout/feat/.agents/skills/{name}")
        );
    }

    #[test]
    fn preserves_name_placeholder_and_nested_subpath() {
        let c = ctx("/repo", "/wt");
        assert_eq!(
            c.retarget("/repo/.claude/skills/{name}").as_deref(),
            Some("/wt/.claude/skills/{name}")
        );
    }

    #[test]
    fn ignores_global_harness_outside_repo() {
        let c = ctx("/home/u/proj", "/home/u/proj/wt/feat");
        // A machine-global harness lives outside the repo and is not retargeted.
        assert_eq!(c.retarget("/home/u/.claude/skills/{name}"), None);
    }

    #[test]
    fn ignores_pattern_already_in_worktree() {
        let c = ctx("/repo", "/repo/wt/feat");
        assert_eq!(c.retarget("/repo/wt/feat/.agents/skills/{name}"), None);
    }

    #[test]
    fn ignores_other_repo() {
        let c = ctx("/home/u/proj", "/home/u/proj/wt/feat");
        assert_eq!(c.retarget("/home/u/other/.agents/skills/{name}"), None);
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {:?} failed", args);
    }

    #[test]
    fn detect_distinguishes_main_and_linked_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("repo");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q"]);
        git(&main, &["config", "user.email", "t@t"]);
        git(&main, &["config", "user.name", "t"]);
        git(&main, &["commit", "--allow-empty", "-qm", "init"]);

        let wt = tmp.path().join("linked");
        git(
            &main,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feat"],
        );

        // Detection resolves canonical paths, so compare canonically.
        let main_c = std::fs::canonicalize(&main).unwrap();
        let wt_c = std::fs::canonicalize(&wt).unwrap();

        // The main worktree has nothing to retarget.
        assert_eq!(WorktreeContext::detect(&main), None);

        // The linked worktree resolves both roots.
        let ctx = WorktreeContext::detect(&wt).expect("linked worktree detected");
        assert_eq!(ctx.worktree_root, wt_c);
        assert_eq!(ctx.main_root, main_c);
    }

    #[test]
    fn detect_returns_none_outside_git() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(WorktreeContext::detect(tmp.path()), None);
    }
}
