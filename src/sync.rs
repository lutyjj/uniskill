use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use crate::config;
use crate::fetcher;
use crate::harnesses;
use crate::linker;
use crate::state;
use crate::worktree;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncSummary {
    pub ok: usize,
    pub created: usize,
    pub updated: usize,
    pub broken: usize,
    pub conflicts: usize,
    pub pruned: usize,
}

impl SyncSummary {
    pub fn total_synced(&self) -> usize {
        self.ok + self.created + self.updated
    }

    pub fn skipped(&self) -> usize {
        self.conflicts + self.broken
    }

    pub fn has_failures(&self) -> bool {
        self.conflicts > 0 || self.broken > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncEvent {
    BundleSkippedNoSources {
        bundle: String,
    },
    BundleFailed {
        bundle: String,
        error: String,
    },
    UnknownHarness {
        bundle: String,
        harness: String,
    },
    BundleNotAssembled {
        bundle: String,
    },
    SkillSynced {
        skill_name: String,
        harness_label: String,
        target: String,
        status: linker::SyncStatus,
    },
    Pruned {
        skill: String,
        harness: String,
    },
    StateWriteFailed {
        error: String,
    },
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub events: Vec<SyncEvent>,
    pub summary: SyncSummary,
}

fn lock_failure(error: anyhow::Error) -> SyncReport {
    SyncReport {
        events: vec![SyncEvent::StateWriteFailed {
            error: error.to_string(),
        }],
        summary: SyncSummary {
            conflicts: 1,
            ..SyncSummary::default()
        },
    }
}

/// Iterate bundles and wire them into the registry.
///
/// Bundles are processed in sorted order so runs are reproducible, and one
/// bundle's failure is reported and skipped rather than aborting the whole sync.
/// Links installed on the previous sync but no longer declared are pruned.
///
/// `cache_dir` is where bundles are assembled and the ownership root for
/// pruning; `state_dir` is where this sync's manifest lives. They are the same
/// directory for a normal sync, and differ for a worktree sync so that main and
/// worktree runs never prune each other's links.
#[cfg(test)]
pub fn sync_with_registry(
    bundles: &HashMap<String, config::Bundle>,
    registry: &HashMap<String, harnesses::HarnessDef>,
    cache_dir: &Path,
    state_dir: &Path,
    source_base_dir: &Path,
) -> SyncReport {
    sync_with_progress(
        bundles,
        registry,
        cache_dir,
        state_dir,
        source_base_dir,
        4,
        &mut |_| {},
    )
}

#[allow(clippy::too_many_arguments)]
pub fn sync_with_progress(
    bundles: &HashMap<String, config::Bundle>,
    registry: &HashMap<String, harnesses::HarnessDef>,
    cache_dir: &Path,
    state_dir: &Path,
    source_base_dir: &Path,
    jobs: usize,
    progress: &mut impl FnMut(crate::fetch_plan::Progress),
) -> SyncReport {
    let _lock = match state::lock(cache_dir) {
        Ok(lock) => lock,
        Err(error) => return lock_failure(error),
    };
    let previous = state::Manifest::load(state_dir);
    let mut report = SyncReport::default();

    let mut managed: Vec<state::ManagedLink> = Vec::new();
    let mut errored_bundles: BTreeSet<String> = BTreeSet::new();

    let mut bundle_names: Vec<&String> = bundles.keys().collect();
    bundle_names.sort();

    let eligible: HashMap<String, config::Bundle> = bundles
        .iter()
        .filter(|(_, bundle)| {
            !bundle.harnesses.is_empty()
                && bundle
                    .harnesses
                    .iter()
                    .all(|name| registry.get(name).is_some_and(|h| h.pattern.is_some()))
                && fetcher::validate_bundle(bundle).is_ok()
        })
        .map(|(name, bundle)| (name.clone(), bundle.clone()))
        .collect();
    let prepared =
        crate::fetch_plan::prepare(&eligible, cache_dir, source_base_dir, jobs, progress);

    for bundle_name in bundle_names {
        let bundle = &bundles[bundle_name];
        if bundle.source.is_empty() && bundle.skills.is_empty() {
            report.events.push(SyncEvent::BundleSkippedNoSources {
                bundle: bundle_name.clone(),
            });
            errored_bundles.insert(bundle_name.clone());
            report.summary.conflicts += 1;
            continue;
        }

        let mut resolved_harnesses = Vec::new();
        let mut missing_harness = false;
        for harness_name in &bundle.harnesses {
            match registry.get(harness_name) {
                Some(harness) if harness.pattern.is_some() => {
                    resolved_harnesses.push((harness_name, harness))
                }
                Some(_) | None => {
                    report.events.push(SyncEvent::UnknownHarness {
                        bundle: bundle_name.clone(),
                        harness: harness_name.clone(),
                    });
                    missing_harness = true;
                    report.summary.conflicts += 1;
                }
            }
        }
        if missing_harness {
            errored_bundles.insert(bundle_name.clone());
            continue;
        }

        progress(crate::fetch_plan::Progress::Assembling {
            bundle: bundle_name.clone(),
        });
        let source = match fetcher::assemble_bundle_prepared(
            bundle_name,
            bundle,
            cache_dir,
            source_base_dir,
            Some(&prepared),
        ) {
            Ok(source) => source,
            Err(err) => {
                report.events.push(SyncEvent::BundleFailed {
                    bundle: bundle_name.clone(),
                    error: err.to_string(),
                });
                errored_bundles.insert(bundle_name.clone());
                report.summary.conflicts += 1;
                continue;
            }
        };

        progress(crate::fetch_plan::Progress::Linking {
            bundle: bundle_name.clone(),
        });
        for (harness_name, harness) in resolved_harnesses {
            let mut results = linker::sync_bundle(
                &source,
                harness
                    .pattern
                    .as_deref()
                    .expect("skill target was validated"),
            );
            results.sort_by(|a, b| a.skill_name.cmp(&b.skill_name));

            for result in results {
                record_result(
                    &mut report,
                    &mut managed,
                    result,
                    &harness.label,
                    harness_name,
                    bundle_name,
                );
            }
        }
    }

    finalize(
        report,
        &previous,
        managed,
        &errored_bundles,
        cache_dir,
        state_dir,
    )
}

/// Sync the already-assembled cache into a linked git worktree.
///
/// Worktrees are fresh checkouts of tracked files only, so uniskill's generated
/// skill symlinks never appear in them. This links the bundles a previous main
/// `sync` already assembled into `cache_dir` onto the worktree's retargeted
/// paths. It never fetches — that keeps a `post-checkout` hook fast and
/// offline-safe — and it records its links in a worktree-scoped `state_dir` so
/// a later main sync never prunes them and vice versa.
///
/// Harnesses whose target is outside the repository's main worktree (a
/// machine-global path, or another repo) are left untouched.
pub fn sync_worktree(
    bundles: &HashMap<String, config::Bundle>,
    registry: &HashMap<String, harnesses::HarnessDef>,
    cache_dir: &Path,
    state_dir: &Path,
    ctx: &worktree::WorktreeContext,
) -> SyncReport {
    let _lock = match state::lock(cache_dir) {
        Ok(lock) => lock,
        Err(error) => return lock_failure(error),
    };
    let previous = state::Manifest::load(state_dir);
    let mut report = SyncReport::default();
    let mut managed: Vec<state::ManagedLink> = Vec::new();
    // A worktree sync assembles nothing, so no bundle can fail to build; an
    // empty errored set means orphan pruning is never held back.
    let errored_bundles: BTreeSet<String> = BTreeSet::new();

    let mut bundle_names: Vec<&String> = bundles.keys().collect();
    bundle_names.sort();

    for bundle_name in bundle_names {
        let bundle = &bundles[bundle_name];
        let assembled = cache_dir.join("bundles").join(bundle_name);
        if !assembled.join("skills").is_dir() {
            // The main sync that assembles this bundle has not run yet.
            report.events.push(SyncEvent::BundleNotAssembled {
                bundle: bundle_name.clone(),
            });
            continue;
        }

        for harness_name in &bundle.harnesses {
            let Some(harness) = registry.get(harness_name) else {
                continue;
            };
            let Some(pattern) = &harness.pattern else {
                continue;
            };
            let expanded = config::expand_env_vars(pattern);
            let Some(retargeted) = ctx.retarget(&expanded) else {
                // Global or other-repo harness — not owned by this worktree.
                continue;
            };

            let mut results = linker::sync_bundle(&assembled, &retargeted);
            results.sort_by(|a, b| a.skill_name.cmp(&b.skill_name));
            for result in results {
                record_result(
                    &mut report,
                    &mut managed,
                    result,
                    &harness.label,
                    harness_name,
                    bundle_name,
                );
            }
        }
    }

    finalize(
        report,
        &previous,
        managed,
        &errored_bundles,
        cache_dir,
        state_dir,
    )
}

/// Prune links no longer declared, then persist the manifest. The single commit
/// point every sync path ends on.
fn finalize(
    mut report: SyncReport,
    previous: &state::Manifest,
    mut managed: Vec<state::ManagedLink>,
    errored_bundles: &BTreeSet<String>,
    cache_dir: &Path,
    state_dir: &Path,
) -> SyncReport {
    report.summary.pruned = prune_orphans(
        previous,
        &mut managed,
        errored_bundles,
        cache_dir,
        &mut report.events,
    );

    if let Err(err) = (state::Manifest { links: managed }).save(state_dir) {
        report.events.push(SyncEvent::StateWriteFailed {
            error: err.to_string(),
        });
    }

    report
}

/// Record one linker result into the report and, when the link is live, the
/// managed set. Shared by the main and worktree sync paths so both classify and
/// count outcomes identically.
fn record_result(
    report: &mut SyncReport,
    managed: &mut Vec<state::ManagedLink>,
    result: linker::SyncResult,
    harness_label: &str,
    harness_key: &str,
    bundle_name: &str,
) {
    let status = result.status.clone();
    if matches!(
        status,
        linker::SyncStatus::Ok | linker::SyncStatus::Created | linker::SyncStatus::Updated
    ) {
        managed.push(state::ManagedLink {
            path: result.target.clone(),
            skill: result.skill_name.clone(),
            harness: harness_key.to_string(),
            bundle: bundle_name.to_string(),
        });
    }

    match &status {
        linker::SyncStatus::Ok => report.summary.ok += 1,
        linker::SyncStatus::Created => report.summary.created += 1,
        linker::SyncStatus::Updated => report.summary.updated += 1,
        linker::SyncStatus::Broken => report.summary.broken += 1,
        linker::SyncStatus::Conflict(_) => report.summary.conflicts += 1,
    }

    report.events.push(SyncEvent::SkillSynced {
        skill_name: result.skill_name,
        harness_label: harness_label.to_string(),
        target: result.target,
        status,
    });
}

fn prune_orphans(
    previous: &state::Manifest,
    managed: &mut Vec<state::ManagedLink>,
    errored_bundles: &BTreeSet<String>,
    cache_dir: &Path,
    events: &mut Vec<SyncEvent>,
) -> usize {
    let current: HashSet<&str> = managed.iter().map(|link| link.path.as_str()).collect();
    let mut retained: Vec<state::ManagedLink> = Vec::new();
    let mut pruned = 0;

    for old in &previous.links {
        if current.contains(old.path.as_str()) {
            continue;
        }
        if errored_bundles.contains(&old.bundle) {
            retained.push(old.clone());
            continue;
        }
        if state::remove_if_managed(Path::new(&old.path), cache_dir) {
            events.push(SyncEvent::Pruned {
                skill: old.skill.clone(),
                harness: old.harness.clone(),
            });
            pruned += 1;
        }
    }

    managed.extend(retained);
    pruned
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn worktree_waits_for_cache_publication_before_reading_or_pruning() {
        use std::sync::mpsc;
        use std::time::Duration;

        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let source = temp.path().join("source");
        let main = temp.path().join("main");
        let linked = temp.path().join("linked");
        let state_dir = linked.join(".uniskill-cache");
        let ctx = worktree::WorktreeContext {
            main_root: main.clone(),
            worktree_root: linked.clone(),
        };
        write_skill_dir(&source.join("skills/docs"), "docs");
        let bundles =
            HashMap::from([("local".into(), bundle_with_source(&source, vec!["fixture"]))]);
        let registry = HashMap::from([(
            "fixture".into(),
            harnesses::HarnessDef {
                label: "Fixture".into(),
                pattern: Some(main.join("skills/{name}").to_string_lossy().into_owned()),
                mcp: None,
            },
        )]);
        assert!(
            !sync_with_registry(&bundles, &registry, &cache, &cache, temp.path())
                .summary
                .has_failures()
        );
        assert!(
            !sync_worktree(&bundles, &registry, &cache, &state_dir, &ctx)
                .summary
                .has_failures()
        );
        let manifest = fs::read(state_dir.join("state.toml")).unwrap();
        let lock = state::lock(&cache).unwrap();
        let published = cache.join("bundles/local");
        let displaced = cache.join("displaced");
        fs::rename(&published, &displaced).unwrap();
        let (started, ready) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                started.send(()).unwrap();
                finished
                    .send(sync_worktree(&bundles, &registry, &cache, &state_dir, &ctx))
                    .unwrap();
            });
            ready.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(matches!(
                done.recv_timeout(Duration::from_millis(200)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            assert!(fs::symlink_metadata(linked.join("skills/docs"))
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(fs::read(state_dir.join("state.toml")).unwrap(), manifest);
            fs::rename(&displaced, &published).unwrap();
            drop(lock);
            let report = done.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(!report.summary.has_failures());
            assert_eq!(report.summary.pruned, 0);
        });
        assert!(linked.join("skills/docs/SKILL.md").exists());
        assert_eq!(fs::read(state_dir.join("state.toml")).unwrap(), manifest);
    }

    fn write_skill_dir(dir: &Path, name: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\n---\n")).unwrap();
    }

    fn bundle_with_source(source: &Path, harnesses: Vec<&str>) -> config::Bundle {
        config::Bundle {
            harnesses: harnesses.into_iter().map(str::to_string).collect(),
            source: config::SourceSpec {
                source: Some(source.to_string_lossy().to_string()),
                ..config::SourceSpec::default()
            },
            skills: HashMap::new(),
            link: false,
        }
    }

    #[test]
    fn test_manifest_records_stable_harness_key() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let source = tmp.path().join("source");
        write_skill_dir(&source.join("skills").join("code-design"), "code-design");

        let mut bundles = HashMap::new();
        bundles.insert(
            "generic".to_string(),
            bundle_with_source(&source, vec!["agents"]),
        );

        let mut registry = HashMap::new();
        registry.insert(
            "agents".to_string(),
            harnesses::HarnessDef {
                label: "Local Agents".to_string(),
                mcp: None,
                pattern: Some(
                    tmp.path()
                        .join("harness")
                        .join("{name}")
                        .to_string_lossy()
                        .to_string(),
                ),
            },
        );

        let report = sync_with_registry(&bundles, &registry, &cache, &cache, tmp.path());

        assert!(!report.summary.has_failures());
        let manifest = state::Manifest::load(&cache);
        assert_eq!(manifest.links.len(), 1);
        assert_eq!(manifest.links[0].harness, "agents");
    }

    #[test]
    fn test_unknown_harness_retains_previous_manifest_link() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let old_skill = cache
            .join("bundles")
            .join("generic")
            .join("skills")
            .join("code-design");
        write_skill_dir(&old_skill, "code-design");

        let target = tmp.path().join("harness").join("code-design");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        crate::linker::create_symlink(&old_skill, &target).unwrap();

        state::Manifest {
            links: vec![state::ManagedLink {
                path: target.to_string_lossy().to_string(),
                skill: "code-design".to_string(),
                harness: "agents".to_string(),
                bundle: "generic".to_string(),
            }],
        }
        .save(&cache)
        .unwrap();

        let source = tmp.path().join("source");
        write_skill_dir(&source.join("skills").join("other-skill"), "other-skill");
        let mut bundles = HashMap::new();
        bundles.insert(
            "generic".to_string(),
            bundle_with_source(&source, vec!["missing"]),
        );

        let registry = HashMap::new();
        let report = sync_with_registry(&bundles, &registry, &cache, &cache, tmp.path());

        assert_eq!(report.summary.conflicts, 1);
        assert!(target.is_symlink());
        assert!(target.exists());
        assert!(old_skill.join("SKILL.md").exists());
        let manifest = state::Manifest::load(&cache);
        assert_eq!(manifest.links.len(), 1);
        assert_eq!(manifest.links[0].path, target.to_string_lossy());
        assert!(report.events.iter().any(|event| {
            matches!(
                event,
                SyncEvent::UnknownHarness { bundle, harness }
                    if bundle == "generic" && harness == "missing"
            )
        }));
    }
}
