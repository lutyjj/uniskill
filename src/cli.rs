use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::config;
use crate::fetcher;
use crate::harnesses;
use crate::linker;
use crate::state;

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

    sync_with_registry(&config.bundles, &registry, &cache_dir, &config_dir)
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

    sync_with_registry(&project_config.bundles, &registry, &cache_dir, &config_dir)
}

/// Shared logic: iterate bundles and wire them into the registry.
///
/// Bundles are processed in sorted order so runs are reproducible, and one
/// bundle's failure is reported and skipped rather than aborting the whole sync.
/// Links installed on the previous sync but no longer declared are pruned.
fn sync_with_registry(
    bundles: &HashMap<String, config::Bundle>,
    registry: &HashMap<String, harnesses::HarnessDef>,
    cache_dir: &Path,
    source_base_dir: &Path,
) -> Result<()> {
    let previous = state::Manifest::load(cache_dir);

    let mut total_ok = 0;
    let mut total_created = 0;
    let mut total_updated = 0;
    let mut total_broken = 0;
    let mut total_conflict = 0;

    // Links successfully installed this run, and bundles that failed to build.
    let mut managed: Vec<state::ManagedLink> = Vec::new();
    let mut errored_bundles: BTreeSet<String> = BTreeSet::new();

    // Deterministic order: sort bundle names.
    let mut bundle_names: Vec<&String> = bundles.keys().collect();
    bundle_names.sort();

    for bundle_name in bundle_names {
        let bundle = &bundles[bundle_name];
        if bundle.source.is_empty() && bundle.skills.is_empty() {
            println!(
                "  ! bundle '{}' has no source or skills — skipping",
                bundle_name
            );
            errored_bundles.insert(bundle_name.clone());
            total_conflict += 1;
            continue;
        }

        // A single bundle's failure must not abort the rest of the sync.
        let source = match fetcher::assemble_bundle(bundle_name, bundle, cache_dir, source_base_dir)
        {
            Ok(source) => source,
            Err(err) => {
                println!("  ✗ bundle '{}': {}", bundle_name, err);
                errored_bundles.insert(bundle_name.clone());
                total_conflict += 1;
                continue;
            }
        };

        // Validate that all declared harnesses exist in the registry.
        for harness_name in &bundle.harnesses {
            let Some(harness) = registry.get(harness_name) else {
                println!(
                    "  ⚠ bundle '{}': unknown harness '{}' — skipping",
                    bundle_name, harness_name
                );
                total_conflict += 1;
                continue;
            };

            // Sort skills so output is stable regardless of filesystem order.
            let mut results = linker::sync_bundle(&source, &harness.pattern);
            results.sort_by(|a, b| a.skill_name.cmp(&b.skill_name));

            for result in results {
                let record = |managed: &mut Vec<state::ManagedLink>| {
                    managed.push(state::ManagedLink {
                        path: result.target.clone(),
                        skill: result.skill_name.clone(),
                        harness: harness.label.clone(),
                        bundle: bundle_name.clone(),
                    });
                };
                match &result.status {
                    linker::SyncStatus::Ok => {
                        println!(
                            "  ✓ {} [{}] → {}",
                            result.skill_name, harness.label, result.target
                        );
                        record(&mut managed);
                        total_ok += 1;
                    }
                    linker::SyncStatus::Created => {
                        println!(
                            "  → {} [{}] → {}",
                            result.skill_name, harness.label, result.target
                        );
                        record(&mut managed);
                        total_created += 1;
                    }
                    linker::SyncStatus::Updated => {
                        println!(
                            "  ~ {} [{}] → {}",
                            result.skill_name, harness.label, result.target
                        );
                        record(&mut managed);
                        total_updated += 1;
                    }
                    linker::SyncStatus::Broken => {
                        println!(
                            "  ✗ {} [{}] : source not found (target at {})",
                            result.skill_name, harness.label, result.target
                        );
                        total_broken += 1;
                    }
                    linker::SyncStatus::Conflict(path) => {
                        println!(
                            "  ! {} [{}] : conflict at {} — skipping",
                            result.skill_name,
                            harness.label,
                            path.display()
                        );
                        total_conflict += 1;
                    }
                }
            }
        }
    }

    let pruned = prune_orphans(&previous, &mut managed, &errored_bundles, cache_dir);

    if let Err(err) = (state::Manifest { links: managed }).save(cache_dir) {
        eprintln!("warning: failed to write uniskill state: {}", err);
    }

    println!();
    print_status(
        total_ok,
        total_created,
        total_updated,
        total_broken,
        total_conflict,
        pruned,
    );

    // Exit with error if there were conflicts (non-zero exit code).
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

/// Remove links installed last run but no longer declared. Links whose bundle
/// failed this run are kept (a build error is not a removal), as are any paths
/// that are not uniskill-managed symlinks. Retained links stay in the manifest.
fn prune_orphans(
    previous: &state::Manifest,
    managed: &mut Vec<state::ManagedLink>,
    errored_bundles: &BTreeSet<String>,
    cache_dir: &Path,
) -> usize {
    let current: HashSet<&str> = managed.iter().map(|link| link.path.as_str()).collect();
    let mut retained: Vec<state::ManagedLink> = Vec::new();
    let mut pruned = 0;

    for old in &previous.links {
        if current.contains(old.path.as_str()) {
            continue; // still installed this run
        }
        if errored_bundles.contains(&old.bundle) {
            retained.push(old.clone()); // bundle is broken, not removed — keep
            continue;
        }
        if state::remove_if_managed(Path::new(&old.path), cache_dir) {
            println!(
                "  - {} [{}] : removed (no longer in config)",
                old.skill, old.harness
            );
            pruned += 1;
        }
    }

    managed.extend(retained);
    pruned
}

fn print_status(
    ok: usize,
    created: usize,
    updated: usize,
    broken: usize,
    conflicts: usize,
    pruned: usize,
) {
    let total = ok + created + updated;
    println!(
        "synced {} skills ({} ok, {} new, {} changed, {} skipped, {} removed)",
        total,
        ok,
        created,
        updated,
        conflicts + broken,
        pruned
    );
}
