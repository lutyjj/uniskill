use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::config;
use crate::harnesses;
use crate::hook;
use crate::linker;
use crate::sync::{self, SyncEvent, SyncReport};
use crate::worktree;

#[derive(Parser)]
#[command(name = "uniskill", version, about, long_about = None)]
struct Cli {
    /// Path to config file (defaults to ~/.config/uniskill/config.toml)
    #[arg(short, long)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create or update symlinks for all declared bundles.
    Sync {
        /// Retarget links into the current git worktree instead of the paths
        /// written in config. Run from inside a linked worktree; used by the
        /// post-checkout hook.
        #[arg(long)]
        worktree: bool,
    },
    /// Manage the git hook that keeps worktrees in sync.
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },
}

#[derive(Subcommand)]
enum HookAction {
    /// Install a post-checkout hook that syncs each new worktree.
    Install {
        /// Install machine-wide via git's global core.hooksPath instead of into
        /// the current repository only.
        #[arg(long)]
        global: bool,
    },
}

/// Default cache directory for assembled bundles (relative to XDG_CACHE_HOME).
const DEFAULT_CACHE_DIR: &str = "uniskill";

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Sync { worktree } => {
            if *worktree {
                sync_current_worktree(cli.config.as_deref())
            } else {
                run_normal_sync(cli.config.clone())
            }
        }
        Commands::Hook { action } => match action {
            HookAction::Install { global } => hook::install(*global, cli.config.as_deref()),
        },
    }
}

/// Sync every bundle into the paths declared in config (the default command).
fn run_normal_sync(explicit_config: Option<PathBuf>) -> Result<()> {
    if let Some(explicit_config) = explicit_config {
        sync_from_path(explicit_config)
    } else {
        match config::discover_project_config()? {
            Some(project_config) => {
                let config_dir = project_config
                    .path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("."));
                sync_project(&project_config.config, config_dir)
            }
            None => {
                let default_path = dirs::config_dir()
                    .map(|d| d.join("uniskill").join("config.toml"))
                    .unwrap_or_else(|| PathBuf::from("./config.toml"));
                sync_from_path(default_path)
            }
        }
    }
}

/// Retarget the assembled cache into the current git worktree.
///
/// Resolves the same bundles and harnesses a normal sync would, but rewrites
/// each repo-scoped harness path onto this worktree and links from the cache the
/// main sync already assembled. A no-op outside a linked worktree.
fn sync_current_worktree(explicit_config: Option<&Path>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let Some(ctx) = worktree::WorktreeContext::detect(&cwd) else {
        println!("not inside a linked git worktree — nothing to sync");
        return Ok(());
    };

    let inputs = worktree_inputs(explicit_config, &ctx)?;
    // Keep the worktree's manifest in the worktree itself: scoped so a main sync
    // never prunes these links, and self-cleaning when the worktree is removed.
    let state_dir = ctx.worktree_root.join(".uniskill-cache");

    let report = sync::sync_worktree(
        &inputs.bundles,
        &inputs.registry,
        &inputs.cache_dir,
        &state_dir,
        &ctx,
    );
    print_report(&report);

    if report.summary.has_failures() {
        Err(anyhow::anyhow!(
            "worktree sync completed with {} conflict(s) and {} broken link(s)",
            report.summary.conflicts,
            report.summary.broken
        ))
    } else {
        Ok(())
    }
}

/// Bundles, harness registry, and assembled-cache location for a worktree sync.
struct WorktreeInputs {
    bundles: HashMap<String, config::Bundle>,
    registry: HashMap<String, harnesses::HarnessDef>,
    cache_dir: PathBuf,
}

/// Resolve worktree-sync inputs from an explicit global config, a project
/// `uniskill.toml` at the repo's main worktree, or the default global config —
/// always anchoring harness patterns and the cache at the main worktree so the
/// retarget in [`sync::sync_worktree`] can rewrite them onto this worktree.
fn worktree_inputs(
    explicit_config: Option<&Path>,
    ctx: &worktree::WorktreeContext,
) -> Result<WorktreeInputs> {
    if let Some(path) = explicit_config {
        let config = config::parse_config(path)?;
        return Ok(WorktreeInputs {
            registry: merge_global_registry(&config),
            bundles: config.bundles,
            cache_dir: shared_cache_dir(),
        });
    }

    let project_config = ctx.main_root.join("uniskill.toml");
    if project_config.exists() {
        let config = config::parse_project_config(&project_config)?;
        return Ok(WorktreeInputs {
            registry: merge_project_registry(&config, &ctx.main_root),
            bundles: config.bundles,
            cache_dir: ctx.main_root.join(".uniskill-cache"),
        });
    }

    let default_path = dirs::config_dir()
        .map(|d| d.join("uniskill").join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("./config.toml"));
    let config = config::parse_config(&default_path)?;
    Ok(WorktreeInputs {
        registry: merge_global_registry(&config),
        bundles: config.bundles,
        cache_dir: shared_cache_dir(),
    })
}

/// Shared cache directory for globally-configured bundles.
fn shared_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .map(|d| d.join(DEFAULT_CACHE_DIR))
        .unwrap_or_else(|| PathBuf::from("./.uniskill-cache"))
}

/// Merge user-defined global harnesses over the built-in defaults.
fn merge_global_registry(config: &config::Config) -> HashMap<String, harnesses::HarnessDef> {
    let mut registry = harnesses::default_harnesses();
    for (name, harness) in &config.harnesses {
        let label = harness.label.clone().unwrap_or_else(|| name.clone());
        registry.insert(
            name.clone(),
            harnesses::HarnessDef {
                label,
                pattern: harness.pattern.clone(),
            },
        );
    }
    registry
}

/// Merge project-local harnesses (relative patterns resolved against
/// `config_dir`) over the built-in defaults.
fn merge_project_registry(
    project_config: &config::ProjectConfig,
    config_dir: &Path,
) -> HashMap<String, harnesses::HarnessDef> {
    let mut registry = harnesses::default_harnesses();
    for (name, local_harness) in &project_config.project_harnesses {
        let resolved = config_dir.join(&local_harness.pattern);
        let label = local_harness.label.clone().unwrap_or_else(|| name.clone());
        registry.insert(
            name.clone(),
            harnesses::HarnessDef {
                label,
                pattern: resolved.to_string_lossy().to_string(),
            },
        );
    }
    registry
}

/// Sync from an explicit global config path.
fn sync_from_path(config_path: PathBuf) -> Result<()> {
    let config = config::parse_config(&config_path)?;
    let config_dir = config_base_dir(&config_path);

    let registry = merge_global_registry(&config);
    let cache_dir = shared_cache_dir();

    run_sync(&config.bundles, &registry, &cache_dir, &config_dir)
}

/// Directory that a global config's relative sources resolve against.
///
/// The config path is canonicalized first, so a config symlinked into
/// `~/.config/uniskill/config.toml` still resolves `../bundles/...` against the
/// real file's directory instead of the symlink's location.
fn config_base_dir(config_path: &Path) -> PathBuf {
    let resolved = config_path
        .canonicalize()
        .unwrap_or_else(|_| config_path.to_path_buf());
    resolved
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Sync using a project-local config with relative paths.
fn sync_project(project_config: &config::ProjectConfig, config_dir: PathBuf) -> Result<()> {
    let registry = merge_project_registry(project_config, &config_dir);

    // Use project-local cache directory for assembled bundles.
    let cache_dir = config_dir.join(".uniskill-cache");

    run_sync(&project_config.bundles, &registry, &cache_dir, &config_dir)
}

fn run_sync(
    bundles: &HashMap<String, config::Bundle>,
    registry: &HashMap<String, harnesses::HarnessDef>,
    cache_dir: &Path,
    source_base_dir: &Path,
) -> Result<()> {
    // A normal sync keeps its manifest in the cache dir.
    let report = sync::sync_with_registry(bundles, registry, cache_dir, cache_dir, source_base_dir);
    print_report(&report);

    if report.summary.has_failures() {
        Err(anyhow::anyhow!(
            "sync completed with {} conflict(s) and {} broken link(s)",
            report.summary.conflicts,
            report.summary.broken
        ))
    } else {
        Ok(())
    }
}

fn print_report(report: &SyncReport) {
    for event in &report.events {
        print_event(event);
    }
    println!();
    print_status(report);
}

fn print_event(event: &SyncEvent) {
    match event {
        SyncEvent::BundleSkippedNoSources { bundle } => {
            println!("  ! bundle '{}' has no source or skills — skipping", bundle);
        }
        SyncEvent::BundleFailed { bundle, error } => {
            println!("  ✗ bundle '{}': {}", bundle, error);
        }
        SyncEvent::UnknownHarness { bundle, harness } => {
            println!(
                "  ⚠ bundle '{}': unknown harness '{}' — skipping",
                bundle, harness
            );
        }
        SyncEvent::BundleNotAssembled { bundle } => {
            println!(
                "  ⚠ bundle '{}': not in cache — run `uniskill sync` first",
                bundle
            );
        }
        SyncEvent::SkillSynced {
            skill_name,
            harness_label,
            target,
            status,
        } => match status {
            linker::SyncStatus::Ok => {
                println!("  ✓ {} [{}] → {}", skill_name, harness_label, target);
            }
            linker::SyncStatus::Created => {
                println!("  → {} [{}] → {}", skill_name, harness_label, target);
            }
            linker::SyncStatus::Updated => {
                println!("  ~ {} [{}] → {}", skill_name, harness_label, target);
            }
            linker::SyncStatus::Broken => {
                println!(
                    "  ✗ {} [{}] : source not found (target at {})",
                    skill_name, harness_label, target
                );
            }
            linker::SyncStatus::Conflict(path) => {
                println!(
                    "  ! {} [{}] : conflict at {} — skipping",
                    skill_name,
                    harness_label,
                    path.display()
                );
            }
        },
        SyncEvent::Pruned { skill, harness } => {
            println!(
                "  - {} [{}] : removed (no longer in config)",
                skill, harness
            );
        }
        SyncEvent::StateWriteFailed { error } => {
            eprintln!("warning: failed to write uniskill state: {}", error);
        }
    }
}

fn print_status(report: &SyncReport) {
    println!(
        "synced {} skills ({} ok, {} new, {} changed, {} skipped, {} removed)",
        report.summary.total_synced(),
        report.summary.ok,
        report.summary.created,
        report.summary.updated,
        report.summary.skipped(),
        report.summary.pruned
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_base_dir_follows_a_symlinked_config() {
        let tmp = tempfile::tempdir().unwrap();
        // Real config lives in one directory...
        let real_dir = tmp.path().join("agent-skills").join("configs");
        std::fs::create_dir_all(&real_dir).unwrap();
        let real_config = real_dir.join("global.toml");
        std::fs::write(&real_config, "").unwrap();

        // ...and is symlinked into the default config location.
        let link_dir = tmp.path().join(".config").join("uniskill");
        std::fs::create_dir_all(&link_dir).unwrap();
        let link = link_dir.join("config.toml");
        std::os::unix::fs::symlink(&real_config, &link).unwrap();

        // Relative sources must resolve against the real directory, not the link.
        assert_eq!(config_base_dir(&link), real_dir.canonicalize().unwrap());
    }

    #[test]
    fn config_base_dir_of_plain_file_is_its_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        assert_eq!(config_base_dir(&config), tmp.path().canonicalize().unwrap());
    }
}
