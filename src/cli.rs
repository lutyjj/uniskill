use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::config;

use crate::harnesses;
use crate::linker;

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

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Sync {} => {
            if let Some(ref explicit_config) = cli.config {
                // Explicit --config overrides everything
                sync_from_path(explicit_config.clone())
            } else if let Some(proj_config) = config::discover_project_config() {
                // Project-local uniskill.toml found in CWD
                eprintln!("[debug] Found project config, {} bundles", proj_config.bundles.len());
                sync_project(&proj_config, std::env::current_dir().unwrap_or_default())
            } else {
                // Fall back to global config
                eprintln!("[debug] No project config, falling back to global");
                let default_path = dirs::config_dir()
                    .map(|d| d.join("uniskill").join("config.toml"))
                    .unwrap_or_else(|| PathBuf::from("./config.toml"));
                sync_from_path(default_path)
            }
        }
    }
}

/// Sync from an explicit global config path.
fn sync_from_path(config_path: PathBuf) -> Result<()> {
    let config = config::parse_config(&config_path)?;

    // Merge registry: user-defined harnesses override built-in defaults
    let mut registry = harnesses::default_harnesses();
    for (name, user_harness) in &config.harnesses {
        let label = user_harness.label.clone().unwrap_or_else(|| name.clone());
        registry.insert(
            name.clone(),
            harnesses::HarnessDef { label, pattern: user_harness.pattern.clone() },
        );
    }

    sync_with_registry(&config.bundles, &registry)
}

/// Sync using a project-local config with relative paths.
fn sync_project(project_config: &config::ProjectConfig, config_dir: PathBuf) -> Result<()> {
    // For `uniskill.toml` in CWD, the project root IS CWD itself.

    // Build registry from project-local harnesses
    let mut registry = harnesses::default_harnesses();
    for (name, local_harness) in &project_config.project_harnesses {
        let resolved = config_dir.join(&local_harness.pattern);
        let label = local_harness.label.clone().unwrap_or_else(|| name.clone());
        registry.insert(
            name.clone(),
            harnesses::HarnessDef { label, pattern: resolved.to_string_lossy().to_string() },
        );
    }

    // Resolve bundle sources relative to the config directory
    let bundles: Vec<config::Bundle> = project_config.bundles.iter().map(|b| {
        let source = if PathBuf::from(&b.source).is_absolute() {
            b.source.clone()
        } else {
            let joined = config_dir.join(&b.source);
            // Normalise — strip leading "./" components so paths read cleanly
            let norm = joined.components().filter(|c| *c != std::path::Component::CurDir).collect::<PathBuf>();
            norm.to_string_lossy().to_string()
        };
        config::Bundle { source, harnesses: b.harnesses.clone() }
    }).collect();

    sync_with_registry(&bundles, &registry)
}

/// Shared logic: iterate bundles and wire them into the registry.
fn sync_with_registry(
    bundles: &[config::Bundle],
    registry: &HashMap<String, harnesses::HarnessDef>,
) -> Result<()> {
    let mut total_ok = 0;
    let mut total_created = 0;
    let mut total_updated = 0;
    let mut total_broken = 0;
    let mut total_conflict = 0;

    for bundle in bundles {
        let source = config::resolve_source(&bundle.source);
        eprintln!("[debug] Bundle source: {:?}, exists={}", source, source.exists());

        // Validate that all declared harnesses exist in the registry
        for harness_name in &bundle.harnesses {
            if !registry.contains_key(harness_name) {
                eprintln!("[debug] Unknown harness: {}", harness_name);
                println!(
                    "  ⚠ bundle '{}': unknown harness '{}' — skipping",
                    bundle.source, harness_name
                );
                total_conflict += 1;
                continue;
            }

            let harness = registry.get(harness_name).unwrap();
            eprintln!("[debug] Syncing {} into {} ({})", bundle.source, harness.label, harness.pattern);
            let results = linker::sync_bundle(&source, &harness.pattern);
            eprintln!("[debug] Got {} results", results.len());

            for result in results {
                match &result.status {
                    linker::SyncStatus::Ok => {
                        println!("  ✓ {} → {}", result.skill_name, result.target);
                        total_ok += 1;
                    }
                    linker::SyncStatus::Created => {
                        println!("  → {} → {}", result.skill_name, result.target);
                        total_created += 1;
                    }
                    linker::SyncStatus::Updated => {
                        println!("  ~ {} → {}", result.skill_name, result.target);
                        total_updated += 1;
                    }
                    linker::SyncStatus::Broken => {
                        println!(
                            "  ✗ {} : source not found (target at {})",
                            result.skill_name, result.target
                        );
                        total_broken += 1;
                    }
                    linker::SyncStatus::Conflict(path) => {
                        println!(
                            "  ! {} : conflict at {} — skipping",
                            result.skill_name,
                            path.display()
                        );
                        total_conflict += 1;
                    }
                }
            }
        }
    }

    println!();
    print_status(total_ok, total_created, total_updated, total_broken, total_conflict);

    // Exit with error if there were conflicts (non-zero exit code)
    if total_conflict > 0 || total_broken > 0 {
        Err(anyhow::anyhow!(
            "sync completed with {} conflict(s) and {} broken link(s)",
            total_conflict,
            total_broken
        ))
    } else {
        Ok(())
    }
}

fn print_status(ok: usize, created: usize, updated: usize, broken: usize, conflicts: usize) {
    let total = ok + created + updated;
    println!(
        "synced {} skills ({} ok, {} new, {} changed, {} skipped)",
        total, ok, created, updated, conflicts + broken
    );
}
