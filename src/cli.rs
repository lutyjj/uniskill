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
        Commands::Sync {} => sync(cli.config.as_ref().map(|p| p.to_string_lossy().into_owned())),
    }
}

fn sync(config_path: Option<String>) -> Result<()> {
    // Determine config path
    let cfg_path = config_path.map(PathBuf::from).unwrap_or_else(|| {
        dirs::config_dir()
            .map(|d| d.join("uniskill").join("config.toml"))
            .unwrap_or_else(|| PathBuf::from("./config.toml"))
    });

    let config = config::parse_config(&cfg_path)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    // Merge registry: user-defined harnesses override built-in defaults
    let mut registry = harnesses::default_harnesses();
    for (name, user_harness) in &config.harnesses {
        registry.insert(
            name.clone(),
            harnesses::HarnessDef {
                label: name.clone(),
                pattern: user_harness.pattern.clone(),
            },
        );
    }

    let mut total_ok = 0;
    let mut total_created = 0;
    let mut total_updated = 0;
    let mut total_broken = 0;
    let mut total_conflict = 0;

    for bundle in &config.bundles {
        let source = config::resolve_source(&bundle.source);

        // Validate that all declared harnesses exist in the registry
        for harness_name in &bundle.harnesses {
            if !registry.contains_key(harness_name) {
                println!(
                    "  ⚠ bundle '{}': unknown harness '{}' — skipping",
                    bundle.source, harness_name
                );
                total_conflict += 1;
                continue;
            }

            let harness = registry.get(harness_name).unwrap();
            let results = linker::sync_bundle(&source, &harness.pattern, harness_name);

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
