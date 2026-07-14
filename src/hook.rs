use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

/// Comment marker written into every hook uniskill owns, so a later install can
/// tell its own hook from one the user wrote by hand and never clobber theirs.
const MARKER: &str = "uniskill-managed-hook";

/// Client-side git hooks. Under a global `core.hooksPath`, git looks *only*
/// there, so any hook name we do not shim would silently stop running a repo's
/// own hook. Shimming the whole set and chaining preserves them.
const CLIENT_HOOKS: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
    "post-index-change",
    "push-to-checkout",
];

/// Install the `post-checkout` hook that keeps skills present in new worktrees.
///
/// Per-repo (`global == false`) writes the hook into the current repository's
/// shared hooks dir. `global == true` installs a machine-wide dispatcher via
/// git's global `core.hooksPath`.
pub fn install(global: bool, config: Option<&Path>) -> Result<()> {
    let bin = uniskill_bin();
    let cfg = match config {
        Some(path) => Some(canonical_config(path)?),
        None => None,
    };
    let sync_cmd = build_sync_command(&bin, cfg.as_deref());

    if global {
        install_global(&sync_cmd)
    } else {
        install_local(&sync_cmd)
    }
}

/// Install `post-checkout` into the current repo's shared hooks directory.
fn install_local(sync_cmd: &str) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let common = git_common_dir(&cwd)
        .context("not inside a git repository — run this from a repo, or use --global")?;
    let hooks_dir = common.join("hooks");
    fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("failed to create {}", hooks_dir.display()))?;
    install_local_at(&hooks_dir, sync_cmd)
}

/// Write (or refuse to overwrite) `post-checkout` in `hooks_dir`. Split from
/// [`install_local`] so the clobber guard is testable without a real cwd.
fn install_local_at(hooks_dir: &Path, sync_cmd: &str) -> Result<()> {
    let target = hooks_dir.join("post-checkout");
    if let Ok(existing) = fs::read_to_string(&target) {
        if !existing.contains(MARKER) {
            println!(
                "⚠ a post-checkout hook already exists at {} and was not written by uniskill.",
                target.display()
            );
            println!("  Leaving it untouched. Add this line to it to enable worktree sync:");
            println!("    {sync_cmd} >/dev/null 2>&1 || true");
            return Ok(());
        }
    }

    write_executable(&target, &local_hook(sync_cmd))?;
    println!("✓ installed post-checkout hook at {}", target.display());
    println!("  New worktrees of this repo now sync automatically.");
    Ok(())
}

/// Install a machine-wide dispatcher via global `core.hooksPath`.
fn install_global(sync_cmd: &str) -> Result<()> {
    let hooks_dir = dirs::config_dir()
        .map(|d| d.join("uniskill").join("hooks"))
        .context("could not resolve a config directory for the global hooks path")?;
    fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("failed to create {}", hooks_dir.display()))?;

    let dispatch = hooks_dir.join("dispatch");
    write_executable(&dispatch, &global_dispatch(sync_cmd))?;

    // Point every client hook name at the one dispatcher so repo-local hooks of
    // any type still run via chaining.
    for name in CLIENT_HOOKS {
        link_or_copy(&dispatch, &hooks_dir.join(name))?;
    }

    println!("✓ wrote global hook dispatcher to {}", hooks_dir.display());
    activate_global_hooks_path(&hooks_dir)
}

/// Set global `core.hooksPath` to `hooks_dir`, refusing to overwrite a
/// different value the user already set.
fn activate_global_hooks_path(hooks_dir: &Path) -> Result<()> {
    let want = hooks_dir.to_string_lossy();
    match global_hooks_path() {
        Some(current) if Path::new(&current) == hooks_dir => {
            println!("  global core.hooksPath already points here — active.");
        }
        Some(current) => {
            println!("⚠ global core.hooksPath is already set to {current} — not overwriting.");
            println!("  To activate uniskill, either move that dispatcher's logic in, or run:");
            println!("    git config --global core.hooksPath {want}");
        }
        None => {
            run_git_global(&["config", "--global", "core.hooksPath", &want])?;
            println!("  set global core.hooksPath — worktrees now sync in every repo.");
        }
    }
    Ok(())
}

/// The command the hook runs, with the uniskill binary and optional config
/// baked in as absolute, shell-quoted paths so it works from any cwd.
fn build_sync_command(bin: &str, config: Option<&Path>) -> String {
    match config {
        Some(cfg) => format!(
            "{} --config {} sync --worktree",
            shell_quote(bin),
            shell_quote(&cfg.to_string_lossy())
        ),
        None => format!("{} sync --worktree", shell_quote(bin)),
    }
}

/// Per-repo hook body. Runs only on the null previous-ref that `git worktree
/// add` and `git clone` produce, so ordinary branch switches stay untouched.
fn local_hook(sync_cmd: &str) -> String {
    format!(
        "#!/bin/sh\n\
         # {MARKER} v1 — keep skills present in new git worktrees (uniskill).\n\
         case \"$1\" in\n\
         \x20 \"\" | *[!0]* ) exit 0 ;;\n\
         esac\n\
         {sync_cmd} >/dev/null 2>&1 || true\n"
    )
}

/// Global dispatcher body. Runs uniskill only for `post-checkout` on a worktree
/// add / clone, then chains to any repo-local hook of the same name.
fn global_dispatch(sync_cmd: &str) -> String {
    format!(
        "#!/bin/sh\n\
         # {MARKER} v1 — global dispatcher (uniskill).\n\
         _hook=$(basename \"$0\")\n\
         if [ \"$_hook\" = \"post-checkout\" ]; then\n\
         \x20 case \"$1\" in\n\
         \x20   \"\" | *[!0]* ) : ;;\n\
         \x20   * ) {sync_cmd} >/dev/null 2>&1 || true ;;\n\
         \x20 esac\n\
         fi\n\
         _local=\"$(git rev-parse --git-common-dir 2>/dev/null)/hooks/$_hook\"\n\
         if [ -x \"$_local\" ] && [ \"$_local\" != \"$0\" ]; then\n\
         \x20 exec \"$_local\" \"$@\"\n\
         fi\n\
         exit 0\n"
    )
}

fn uniskill_bin() -> String {
    std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "uniskill".to_string())
}

fn canonical_config(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("config file not found: {}", path.display()))
}

fn git_common_dir(dir: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn global_hooks_path() -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--global", "--get", "core.hooksPath"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn run_git_global(args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .args(args)
        .status()
        .context("failed to run git")?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("git {:?} failed", args))
    }
}

/// Write `content` to `path`, replacing an existing file, and mark it
/// executable on unix.
fn write_executable(path: &Path, content: &str) -> Result<()> {
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))?;
    set_executable(path)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).with_context(|| format!("failed to chmod {}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Point `link` at `dispatch`: a symlink on unix, a copy elsewhere. Replaces an
/// existing entry so reinstalls are idempotent.
#[cfg(unix)]
fn link_or_copy(dispatch: &Path, link: &Path) -> Result<()> {
    if fs::symlink_metadata(link).is_ok() {
        let _ = fs::remove_file(link);
    }
    std::os::unix::fs::symlink(dispatch, link)
        .with_context(|| format!("failed to link {}", link.display()))
}

#[cfg(not(unix))]
fn link_or_copy(dispatch: &Path, link: &Path) -> Result<()> {
    fs::copy(dispatch, link)
        .map(|_| ())
        .with_context(|| format!("failed to copy hook to {}", link.display()))?;
    set_executable(link)
}

/// Wrap `s` in single quotes for POSIX sh, escaping any embedded quotes.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_command_bakes_config_absolutely() {
        let cmd = build_sync_command("/usr/bin/uniskill", Some(Path::new("/home/u/g.toml")));
        assert_eq!(
            cmd,
            "'/usr/bin/uniskill' --config '/home/u/g.toml' sync --worktree"
        );
    }

    #[test]
    fn sync_command_without_config() {
        let cmd = build_sync_command("/usr/bin/uniskill", None);
        assert_eq!(cmd, "'/usr/bin/uniskill' sync --worktree");
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn local_hook_only_runs_on_null_prev_ref() {
        let body = local_hook("'uniskill' sync --worktree");
        assert!(body.contains(MARKER));
        // Non-zero previous ref (an ordinary checkout) exits before syncing.
        assert!(body.contains("*[!0]* ) exit 0"));
        assert!(body.contains("sync --worktree"));
    }

    #[test]
    fn global_dispatch_chains_to_local_hook() {
        let body = global_dispatch("'uniskill' sync --worktree");
        assert!(body.contains(MARKER));
        // Only post-checkout triggers uniskill.
        assert!(body.contains("if [ \"$_hook\" = \"post-checkout\" ]"));
        // Repo-local hooks of any name are still chained.
        assert!(body.contains("exec \"$_local\" \"$@\""));
        // And never re-execs itself.
        assert!(body.contains("\"$_local\" != \"$0\""));
    }

    #[test]
    fn install_local_refuses_to_clobber_foreign_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path();
        let target = hooks.join("post-checkout");
        let foreign = "#!/bin/sh\necho mine\n";
        fs::write(&target, foreign).unwrap();

        install_local_at(hooks, "'uniskill' sync --worktree").unwrap();

        // The user's hook is left byte-for-byte intact.
        assert_eq!(fs::read_to_string(&target).unwrap(), foreign);
    }

    #[test]
    fn install_local_writes_managed_hook_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path();

        install_local_at(hooks, "'uniskill' sync --worktree").unwrap();

        let body = fs::read_to_string(hooks.join("post-checkout")).unwrap();
        assert!(body.contains(MARKER));
        assert!(body.contains("sync --worktree"));
    }

    #[test]
    fn install_local_refreshes_its_own_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path();

        install_local_at(hooks, "'uniskill' sync --worktree").unwrap();
        // A second install with a new command replaces the managed hook.
        install_local_at(hooks, "'uniskill' --config '/g.toml' sync --worktree").unwrap();

        let body = fs::read_to_string(hooks.join("post-checkout")).unwrap();
        assert!(body.contains("--config '/g.toml'"));
    }
}
