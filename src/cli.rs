use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::config;
use crate::harnesses;
use crate::hook;
use crate::linker;
use crate::mcp;
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
    /// Sync declared skill bundles and MCP servers.
    Sync {
        /// Retarget links into the current git worktree instead of the paths
        /// written in config. Run from inside a linked worktree; used by the
        /// post-checkout hook.
        #[arg(long)]
        worktree: bool,
        /// Check MCP definitions without fetching skills or writing files.
        #[arg(long)]
        check_mcp: bool,
        /// Take ownership of existing MCP entries only when definitions match.
        #[arg(long)]
        adopt_mcp: bool,
        /// Maximum simultaneous remote fetches.
        #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(1..=16))]
        jobs: u8,
        /// Sync only MCP definitions without fetching or linking skills.
        #[arg(long, conflicts_with = "check_mcp")]
        mcp_only: bool,
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
        Commands::Sync {
            worktree,
            check_mcp,
            adopt_mcp,
            jobs,
            mcp_only,
        } => {
            let options = McpOptions {
                check: *check_mcp,
                adopt: *adopt_mcp,
                jobs: usize::from(*jobs),
                only: *mcp_only,
            };
            if *worktree {
                sync_current_worktree(cli.config.as_deref(), options)
            } else {
                run_normal_sync(cli.config.clone(), options)
            }
        }
        Commands::Hook { action } => match action {
            HookAction::Install { global } => hook::install(*global, cli.config.as_deref()),
        },
    }
}

/// Sync every bundle into the paths declared in config (the default command).
#[derive(Clone, Copy)]
struct McpOptions {
    check: bool,
    adopt: bool,
    jobs: usize,
    only: bool,
}

fn run_normal_sync(explicit_config: Option<PathBuf>, options: McpOptions) -> Result<()> {
    if let Some(explicit_config) = explicit_config {
        sync_from_path(explicit_config, options)
    } else {
        match config::discover_project_config()? {
            Some(project_config) => {
                let config_dir = project_config
                    .path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("."));
                sync_project(&project_config.config, config_dir, options)
            }
            None => {
                let default_path = dirs::config_dir()
                    .map(|d| d.join("uniskill").join("config.toml"))
                    .unwrap_or_else(|| PathBuf::from("./config.toml"));
                sync_from_path(default_path, options)
            }
        }
    }
}

/// Retarget the assembled cache into the current git worktree.
///
/// Resolves the same bundles and harnesses a normal sync would, but rewrites
/// each repo-scoped harness path onto this worktree and links from the cache the
/// main sync already assembled. A no-op outside a linked worktree.
fn sync_current_worktree(explicit_config: Option<&Path>, options: McpOptions) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let Some(ctx) = worktree::WorktreeContext::detect(&cwd) else {
        println!("not inside a linked git worktree — nothing to sync");
        return Ok(());
    };

    let inputs = worktree_inputs(explicit_config, &ctx)?;
    // Keep the worktree's manifest in the worktree itself: scoped so a main sync
    // never prunes these links, and self-cleaning when the worktree is removed.
    let state_dir = ctx.worktree_root.join(".uniskill-cache");

    let mcp_report = if options.check {
        mcp::check(
            &inputs.mcp,
            &inputs.registry,
            &state_dir,
            Some(&ctx),
            options.adopt,
        )?
    } else {
        mcp::sync(
            &inputs.mcp,
            &inputs.registry,
            &state_dir,
            Some(&ctx),
            options.adopt,
        )?
    };
    print_mcp_report(&mcp_report);
    if options.check {
        return finish_mcp_check(&mcp_report);
    }
    if options.only {
        return Ok(());
    }

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
    mcp: std::collections::BTreeMap<String, mcp::Server>,
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
            registry: merge_registry(&config),
            mcp: config.mcp,
            bundles: config.bundles,
            cache_dir: shared_cache_dir(),
        });
    }

    let project_config = ctx.main_root.join("uniskill.toml");
    if project_config.exists() {
        let config = config::parse_config(&project_config)?;
        return Ok(WorktreeInputs {
            registry: merge_registry(&config),
            mcp: config.mcp,
            bundles: config.bundles,
            cache_dir: ctx.main_root.join(".uniskill-cache"),
        });
    }

    let default_path = dirs::config_dir()
        .map(|d| d.join("uniskill").join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("./config.toml"));
    let config = config::parse_config(&default_path)?;
    Ok(WorktreeInputs {
        registry: merge_registry(&config),
        mcp: config.mcp,
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
fn merge_registry(config: &config::Config) -> HashMap<String, harnesses::HarnessDef> {
    let mut registry = harnesses::default_harnesses();
    for (name, harness) in &config.harnesses {
        let target = registry
            .entry(name.clone())
            .or_insert_with(|| harnesses::HarnessDef {
                label: name.clone(),
                pattern: None,
                mcp: None,
            });
        if let Some(label) = &harness.label {
            target.label = label.clone();
        }
        if let Some(pattern) = &harness.pattern {
            target.pattern = Some(pattern.clone());
        }
        if let Some(mcp) = &harness.mcp {
            target.mcp = Some(mcp.clone());
        }
    }
    registry
}

/// Sync from an explicit global config path.
fn sync_from_path(config_path: PathBuf, options: McpOptions) -> Result<()> {
    let config = config::parse_config(&config_path)?;
    let config_dir = config_base_dir(&config_path);

    let registry = merge_registry(&config);
    let cache_dir = shared_cache_dir();

    run_sync(&config, &registry, &cache_dir, &config_dir, options)
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
fn sync_project(
    project_config: &config::Config,
    config_dir: PathBuf,
    options: McpOptions,
) -> Result<()> {
    let registry = merge_registry(project_config);

    // Use project-local cache directory for assembled bundles.
    let cache_dir = config_dir.join(".uniskill-cache");

    run_sync(project_config, &registry, &cache_dir, &config_dir, options)
}

fn run_sync(
    config: &config::Config,
    registry: &HashMap<String, harnesses::HarnessDef>,
    cache_dir: &Path,
    source_base_dir: &Path,
    options: McpOptions,
) -> Result<()> {
    let mcp_report = if options.check {
        mcp::check(&config.mcp, registry, cache_dir, None, options.adopt)?
    } else {
        mcp::sync(&config.mcp, registry, cache_dir, None, options.adopt)?
    };
    print_mcp_report(&mcp_report);
    if options.check {
        return finish_mcp_check(&mcp_report);
    }
    if options.only {
        return Ok(());
    }
    // A normal sync keeps its manifest in the cache dir.
    let report = sync::sync_with_progress(
        &config.bundles,
        registry,
        cache_dir,
        cache_dir,
        source_base_dir,
        options.jobs,
        &mut print_fetch_progress,
    );
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

fn print_fetch_progress(progress: crate::fetch_plan::Progress) {
    use crate::fetch_plan::Progress;
    match progress {
        Progress::Assembling { bundle } => println!("  assemble {bundle}"),
        Progress::Linking { bundle } => println!("  link {bundle}"),
        Progress::Plan { bundles, remote_sources, workers } => println!("plan: {bundles} bundles, {remote_sources} distinct remote sources, {workers} fetch workers"),
        Progress::Started { label, kind } => println!("  fetch {label} [{kind}]"),
        Progress::Finished { label, seconds, success } => println!("  fetch {label}: {} ({seconds:.2}s)", if success { "ready" } else { "failed" }),
    }
}

fn print_mcp_report(report: &mcp::Report) {
    for path in &report.preserved {
        eprintln!(
            "MCP recovery preserved an external edit: {}",
            path.display()
        );
    }
    for event in &report.events {
        println!(
            "  MCP {} [{}] -> {}",
            event.server,
            event.status,
            event.target.display()
        );
    }
}

fn finish_mcp_check(report: &mcp::Report) -> Result<()> {
    if report.changed {
        anyhow::bail!("MCP definitions need syncing");
    }
    Ok(())
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
