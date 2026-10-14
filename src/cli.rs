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
            let options = SyncOptions {
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
struct SyncOptions {
    check: bool,
    adopt: bool,
    jobs: usize,
    only: bool,
}

fn run_normal_sync(explicit_config: Option<PathBuf>, options: SyncOptions) -> Result<()> {
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
fn sync_current_worktree(explicit_config: Option<&Path>, options: SyncOptions) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let Some(ctx) = worktree::WorktreeContext::detect(&cwd) else {
        println!("not inside a linked git worktree — nothing to sync");
        return Ok(());
    };

    let inputs = worktree_inputs(explicit_config, &ctx)?;
    // Keep the worktree's manifest in the worktree itself: scoped so a main sync
    // never prunes these links, and self-cleaning when the worktree is removed.
    let state_path = ctx.destination(&ctx.worktree_root.join(".uniskill-cache/state.toml"))?;
    let state_dir = state_path
        .parent()
        .expect("state file has parent")
        .to_path_buf();

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
            "worktree sync completed with {} conflict(s), {} broken link(s), and {} state error(s)",
            report.summary.conflicts,
            report.summary.broken,
            report.summary.errors
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
            registry: harnesses::registry(&config)?,
            mcp: config.mcp,
            bundles: config.bundles,
            cache_dir: shared_cache_dir(),
        });
    }

    let project_config = ctx.main_root.join("uniskill.toml");
    if project_config.exists() {
        let config = config::parse_config(&project_config)?;
        return Ok(WorktreeInputs {
            registry: harnesses::registry(&config)?,
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
        registry: harnesses::registry(&config)?,
        mcp: config.mcp,
        bundles: config.bundles,
        cache_dir: shared_cache_dir(),
    })
}

/// Shared cache directory for globally-configured bundles.
fn shared_cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(dirs::cache_dir)
        .map(|d| d.join(DEFAULT_CACHE_DIR))
        .unwrap_or_else(|| PathBuf::from("./.uniskill-cache"))
}

/// Sync from an explicit global config path.
fn sync_from_path(config_path: PathBuf, options: SyncOptions) -> Result<()> {
    let config = config::parse_config(&config_path)?;

    let registry = harnesses::registry(&config)?;
    let cache_dir = shared_cache_dir();

    run_sync(&config, &registry, &cache_dir, options)
}

/// Sync using a project-local config with relative paths.
fn sync_project(
    project_config: &config::Config,
    config_dir: PathBuf,
    options: SyncOptions,
) -> Result<()> {
    let registry = harnesses::registry(project_config)?;

    // Use project-local cache directory for assembled bundles.
    let cache_dir = config_dir.join(".uniskill-cache");

    run_sync(project_config, &registry, &cache_dir, options)
}

fn run_sync(
    config: &config::Config,
    registry: &HashMap<String, harnesses::HarnessDef>,
    cache_dir: &Path,
    options: SyncOptions,
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
        options.jobs,
        &mut print_fetch_progress,
    );
    print_report(&report);

    if report.summary.has_failures() {
        Err(anyhow::anyhow!(
            "sync completed with {} conflict(s), {} broken link(s), and {} state error(s)",
            report.summary.conflicts,
            report.summary.broken,
            report.summary.errors
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
        SyncEvent::BundleFailed { bundle, error } => {
            println!("  ✗ bundle '{}': {}", bundle, error);
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
        SyncEvent::PruneFailed { path, error } => {
            eprintln!("error: cannot remove managed link {path}: {error}");
        }
        SyncEvent::StateFailed { error } => {
            eprintln!("error: uniskill state: {}", error);
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
