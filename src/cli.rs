use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::config;
use crate::harnesses;
use crate::linker;
use crate::sync::{self, SyncEvent, SyncReport};

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
    Sync {},
}

/// Default cache directory for assembled bundles (relative to XDG_CACHE_HOME).
const DEFAULT_CACHE_DIR: &str = "uniskill";

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Sync {} => {
            if let Some(ref explicit_config) = cli.config {
                sync_from_path(explicit_config.clone())
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
    }
}

/// Sync from an explicit global config path.
fn sync_from_path(config_path: PathBuf) -> Result<()> {
    let config = config::parse_config(&config_path)?;
    let config_dir = config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    // Merge registry: user-defined harnesses override built-in defaults
    let mut registry = harnesses::default_harnesses();
    for (name, user_harness) in &config.harnesses {
        let label = user_harness.label.clone().unwrap_or_else(|| name.clone());
        registry.insert(
            name.clone(),
            harnesses::HarnessDef {
                label,
                pattern: user_harness.pattern.clone(),
            },
        );
    }

    // Resolve cache directory for assembled bundles.
    let cache_dir = dirs::cache_dir()
        .map(|d| d.join(DEFAULT_CACHE_DIR))
        .unwrap_or_else(|| PathBuf::from("./.uniskill-cache"));

    run_sync(&config.bundles, &registry, &cache_dir, &config_dir)
}

/// Sync using a project-local config with relative paths.
fn sync_project(project_config: &config::ProjectConfig, config_dir: PathBuf) -> Result<()> {
    // Build registry from project-local harnesses
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
    let report = sync::sync_with_registry(bundles, registry, cache_dir, source_base_dir);
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
