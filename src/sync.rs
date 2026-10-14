use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::{config, fetch_plan, fetcher, harnesses, linker, state, worktree};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncSummary {
    pub ok: usize,
    pub created: usize,
    pub updated: usize,
    pub conflicts: usize,
    pub broken: usize,
    pub pruned: usize,
    pub errors: usize,
}

impl SyncSummary {
    pub fn total_synced(&self) -> usize {
        self.ok + self.created + self.updated
    }
    pub fn skipped(&self) -> usize {
        self.conflicts + self.broken
    }
    pub fn has_failures(&self) -> bool {
        self.conflicts > 0 || self.broken > 0 || self.errors > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncEvent {
    BundleFailed {
        bundle: String,
        error: String,
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
    PruneFailed {
        path: String,
        error: String,
    },
    StateFailed {
        error: String,
    },
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub events: Vec<SyncEvent>,
    pub summary: SyncSummary,
}

fn state_failure(error: anyhow::Error) -> SyncReport {
    SyncReport {
        events: vec![SyncEvent::StateFailed {
            error: format!("{error:#}"),
        }],
        summary: SyncSummary {
            errors: 1,
            ..SyncSummary::default()
        },
    }
}

struct PriorLink {
    owner: state::ManagedLink,
    ancestors: BTreeSet<PathBuf>,
}

fn ancestor_destinations(path: &Path) -> Result<BTreeSet<PathBuf>> {
    path.ancestors()
        .skip(1)
        .filter(|parent| parent.file_name().is_some())
        .map(config::destination_path)
        .collect()
}

fn begin(
    cache: &Path,
    state_dir: &Path,
    worktree: Option<&worktree::WorktreeContext>,
) -> Result<(std::fs::File, PathBuf, Vec<PriorLink>)> {
    let lock = state::lock(cache)?;
    let cache = cache.canonicalize()?;
    if let Some(ctx) = worktree {
        ctx.destination(&state_dir.join("state.toml"))?;
    }
    let previous = state::Manifest::load(state_dir)?
        .links
        .into_iter()
        .map(|mut owner| {
            let declared = PathBuf::from(&owner.path);
            let path = match worktree {
                Some(ctx) => ctx.destination(&declared)?,
                None => config::destination_path(&declared)?,
            };
            owner.path = path.to_string_lossy().into_owned();
            Ok(PriorLink {
                owner,
                ancestors: ancestor_destinations(&declared)?,
            })
        })
        .collect::<Result<_>>()?;
    Ok((lock, cache, previous))
}

fn bundle_failure(
    report: &mut SyncReport,
    failed: &mut BTreeSet<String>,
    bundle: &str,
    error: anyhow::Error,
) {
    failed.insert(bundle.into());
    report.summary.conflicts += 1;
    report.events.push(SyncEvent::BundleFailed {
        bundle: bundle.into(),
        error: format!("{error:#}"),
    });
}

struct Destination {
    harness: String,
    label: String,
    pattern: String,
}

/// Both sync modes validate the complete routing policy before selecting scope.
fn destinations(
    bundle: &config::Bundle,
    registry: &HashMap<String, harnesses::HarnessDef>,
    ctx: Option<&worktree::WorktreeContext>,
) -> Result<Vec<Destination>> {
    if bundle.harnesses.is_empty() {
        bail!("bundle requires at least one harness");
    }
    fetcher::validate_bundle(bundle)?;
    let mut result = Vec::new();
    for name in bundle.harnesses.iter().collect::<BTreeSet<_>>() {
        let harness = registry
            .get(name)
            .with_context(|| format!("unknown harness '{name}'"))?;
        let pattern = harness
            .pattern
            .as_ref()
            .with_context(|| format!("harness '{name}' has no skill target"))?;
        config::validate_pattern(pattern)?;
        let pattern = match ctx {
            Some(ctx) => match ctx.retarget(pattern) {
                Some(pattern) => pattern,
                None => continue,
            },
            None => pattern.clone(),
        };
        result.push(Destination {
            harness: name.clone(),
            label: harness.label.clone(),
            pattern,
        });
    }
    Ok(result)
}

struct Route {
    source: PathBuf,
    owner: state::ManagedLink,
    label: String,
    ancestors: BTreeSet<PathBuf>,
}

type Routes = BTreeMap<PathBuf, Vec<Route>>;

fn plan_bundle(
    routes: &mut Routes,
    name: &str,
    skills: &Path,
    published: &Path,
    destinations: &[Destination],
    worktree: Option<&worktree::WorktreeContext>,
) -> Result<()> {
    let mut planned = Vec::new();
    for entry in
        std::fs::read_dir(skills).context("bundle cache is unavailable; run normal sync first")?
    {
        let entry = entry?;
        if !crate::skill::is_skill_dir(&entry.path()) {
            continue;
        }
        let skill = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("skill names must be UTF-8"))?;
        config::validate_component(&skill)?;
        for destination in destinations {
            let declared = PathBuf::from(destination.pattern.replace("{name}", &skill));
            let path = match worktree {
                Some(ctx) => ctx.destination(&declared)?,
                None => config::destination_path(&declared)?,
            };
            let owner = state::ManagedLink {
                path: path.to_string_lossy().into_owned(),
                skill: skill.clone(),
                harness: destination.harness.clone(),
                bundle: name.into(),
            };
            planned.push((
                path,
                Route {
                    source: published.join("skills").join(&skill),
                    owner,
                    label: destination.label.clone(),
                    ancestors: ancestor_destinations(&declared)?,
                },
            ));
        }
    }
    for (path, route) in planned {
        routes.entry(path).or_default().push(route);
    }
    Ok(())
}

fn select_routes(
    routes: Routes,
    previous: &[PriorLink],
    configured: &HashMap<String, config::Bundle>,
    failed: &mut BTreeSet<String>,
    report: &mut SyncReport,
) -> BTreeMap<PathBuf, Route> {
    let mut descendants = BTreeMap::<PathBuf, BTreeSet<String>>::new();
    for candidates in routes.values() {
        for route in candidates {
            for parent in &route.ancestors {
                descendants
                    .entry(parent.clone())
                    .or_default()
                    .insert(route.owner.bundle.clone());
            }
        }
    }
    for (path, candidates) in &routes {
        let different = candidates
            .iter()
            .any(|route| route.source != candidates[0].source);
        let nested = descendants.get(path);
        if different || nested.is_some() {
            let bundles: BTreeSet<_> = candidates
                .iter()
                .map(|r| r.owner.bundle.clone())
                .chain(nested.into_iter().flatten().cloned())
                .collect();
            for bundle in bundles {
                if !failed.contains(&bundle) {
                    bundle_failure(
                        report,
                        failed,
                        &bundle,
                        anyhow::anyhow!(
                            "overlapping or conflicting skill routes at {}",
                            path.display()
                        ),
                    );
                }
            }
        }
    }
    for old in previous {
        let root = PathBuf::from(&old.owner.path);
        let mut blocked = descendants.get(&root).cloned().unwrap_or_default();
        for path in &old.ancestors {
            if let Some(candidates) = routes.get(path) {
                blocked.extend(candidates.iter().map(|r| r.owner.bundle.clone()));
            }
        }
        if configured.contains_key(&old.owner.bundle) {
            if let Some(candidates) = routes.get(&root) {
                blocked.extend(
                    candidates
                        .iter()
                        .filter(|r| r.owner.bundle != old.owner.bundle)
                        .map(|r| r.owner.bundle.clone()),
                );
            }
        }
        for bundle in blocked {
            if !failed.contains(&bundle) {
                bundle_failure(
                    report,
                    failed,
                    &bundle,
                    anyhow::anyhow!(
                        "skill destination overlaps existing ownership at {}",
                        root.display()
                    ),
                );
            }
        }
    }
    routes
        .into_iter()
        .filter_map(|(path, candidates)| {
            candidates
                .into_iter()
                .find(|r| !failed.contains(&r.owner.bundle))
                .map(|r| (path, r))
        })
        .collect()
}

/// Retain a durable ownership superset until link application and pruning finish.
/// Every intermediate state can be reconciled by the next ordinary sync.
fn save_intent(
    previous: &[PriorLink],
    routes: &BTreeMap<PathBuf, Route>,
    state_dir: &Path,
) -> Result<()> {
    let mut owned = BTreeMap::new();
    for link in previous
        .iter()
        .map(|prior| prior.owner.clone())
        .chain(routes.values().map(|r| r.owner.clone()))
    {
        owned.insert((link.path.clone(), link.bundle.clone()), link);
    }
    state::Manifest {
        links: owned.into_values().collect(),
    }
    .save(state_dir)
}

#[cfg(test)]
pub fn sync_with_registry(
    bundles: &HashMap<String, config::Bundle>,
    registry: &HashMap<String, harnesses::HarnessDef>,
    cache_dir: &Path,
    state_dir: &Path,
    source_base_dir: &Path,
) -> SyncReport {
    let mut bundles = bundles.clone();
    for bundle in bundles.values_mut() {
        config::anchor_bundle_paths(bundle, source_base_dir);
    }
    sync_with_progress(&bundles, registry, cache_dir, state_dir, 4, &mut |_| {})
}

pub fn sync_with_progress(
    bundles: &HashMap<String, config::Bundle>,
    registry: &HashMap<String, harnesses::HarnessDef>,
    cache_dir: &Path,
    state_dir: &Path,
    jobs: usize,
    progress: &mut impl FnMut(fetch_plan::Progress),
) -> SyncReport {
    let (_lock, cache, previous) = match begin(cache_dir, state_dir, None) {
        Ok(inputs) => inputs,
        Err(error) => return state_failure(error),
    };
    let mut report = SyncReport::default();
    let mut failed = BTreeSet::new();
    let mut eligible = HashMap::new();
    let mut targets = BTreeMap::new();
    for (name, bundle) in bundles.iter().collect::<BTreeMap<_, _>>() {
        let checked = config::validate_component(name)
            .and_then(|_| fetcher::recover_bundle_cache(&cache.join("bundles").join(name)))
            .and_then(|_| destinations(bundle, registry, None));
        match checked {
            Ok(destinations) => {
                targets.insert(name.clone(), destinations);
                eligible.insert(name.clone(), bundle.clone());
            }
            Err(error) => bundle_failure(&mut report, &mut failed, name, error),
        }
    }
    let prepared = fetch_plan::prepare(&eligible, &cache, jobs, progress);
    let mut staged = BTreeMap::new();
    let mut routes = Routes::new();
    for (name, destinations) in targets {
        progress(fetch_plan::Progress::Assembling {
            bundle: name.clone(),
        });
        let result =
            fetcher::stage_bundle(&name, &bundles[&name], &cache, &prepared).and_then(|bundle| {
                plan_bundle(
                    &mut routes,
                    &name,
                    &bundle.staged.join("skills"),
                    &bundle.published,
                    &destinations,
                    None,
                )?;
                Ok(bundle)
            });
        match result {
            Ok(bundle) => {
                staged.insert(name, bundle);
            }
            Err(error) => bundle_failure(&mut report, &mut failed, &name, error),
        }
    }
    let routes = select_routes(routes, &previous, bundles, &mut failed, &mut report);
    if let Err(error) = save_intent(&previous, &routes, state_dir) {
        return state_failure(error);
    }
    for (name, bundle) in staged {
        if failed.contains(&name) {
            continue;
        }
        if let Err(error) = bundle.publish() {
            bundle_failure(&mut report, &mut failed, &name, error);
        }
    }
    for name in bundles.keys().collect::<BTreeSet<_>>() {
        if !failed.contains(name) {
            progress(fetch_plan::Progress::Linking {
                bundle: name.clone(),
            });
        }
    }
    apply_routes(report, &previous, routes, failed, &cache, state_dir)
}

/// Worktrees consume published caches without downloading or assembling sources.
pub fn sync_worktree(
    bundles: &HashMap<String, config::Bundle>,
    registry: &HashMap<String, harnesses::HarnessDef>,
    cache_dir: &Path,
    state_dir: &Path,
    ctx: &worktree::WorktreeContext,
) -> SyncReport {
    let (_lock, cache, previous) = match begin(cache_dir, state_dir, Some(ctx)) {
        Ok(inputs) => inputs,
        Err(error) => return state_failure(error),
    };
    let mut report = SyncReport::default();
    let mut failed = BTreeSet::new();
    let mut routes = Routes::new();
    for (name, bundle) in bundles.iter().collect::<BTreeMap<_, _>>() {
        let published = cache.join("bundles").join(name);
        let result = config::validate_component(name)
            .and_then(|_| destinations(bundle, registry, Some(ctx)))
            .and_then(|destinations| {
                if destinations.is_empty() {
                    return Ok(());
                }
                fetcher::recover_bundle_cache(&published)?;
                plan_bundle(
                    &mut routes,
                    name,
                    &published.join("skills"),
                    &published,
                    &destinations,
                    Some(ctx),
                )
            });
        if let Err(error) = result {
            bundle_failure(&mut report, &mut failed, name, error);
        }
    }
    let routes = select_routes(routes, &previous, bundles, &mut failed, &mut report);
    if let Err(error) = save_intent(&previous, &routes, state_dir) {
        return state_failure(error);
    }
    apply_routes(report, &previous, routes, failed, &cache, state_dir)
}

fn apply_routes(
    mut report: SyncReport,
    previous: &[PriorLink],
    routes: BTreeMap<PathBuf, Route>,
    mut failed: BTreeSet<String>,
    cache: &Path,
    state_dir: &Path,
) -> SyncReport {
    let mut managed = Vec::new();
    let mut failed_paths = HashSet::new();
    for (path, route) in routes {
        if failed.contains(&route.owner.bundle) {
            failed_paths.insert(route.owner.path);
            continue;
        }
        let result = linker::ensure_skill_symlink(&route.source, &path);
        match &result.status {
            linker::SyncStatus::Ok => report.summary.ok += 1,
            linker::SyncStatus::Created => report.summary.created += 1,
            linker::SyncStatus::Updated => report.summary.updated += 1,
            linker::SyncStatus::Broken => {
                report.summary.broken += 1;
                failed.insert(route.owner.bundle.clone());
                failed_paths.insert(route.owner.path.clone());
            }
            linker::SyncStatus::Conflict(_) => {
                report.summary.conflicts += 1;
                failed.insert(route.owner.bundle.clone());
                failed_paths.insert(route.owner.path.clone());
            }
        }
        if matches!(
            result.status,
            linker::SyncStatus::Ok | linker::SyncStatus::Created | linker::SyncStatus::Updated
        ) {
            managed.push(route.owner);
        }
        report.events.push(SyncEvent::SkillSynced {
            skill_name: result.skill_name,
            harness_label: route.label,
            target: result.target,
            status: result.status,
        });
    }
    let current: HashSet<_> = managed.iter().map(|link| link.path.clone()).collect();
    let retained: HashSet<_> = previous
        .iter()
        .filter(|prior| failed.contains(&prior.owner.bundle))
        .map(|prior| &prior.owner.path)
        .collect();
    for prior in previous {
        let old = &prior.owner;
        if current.contains(&old.path) {
            continue;
        }
        if failed.contains(&old.bundle) || failed_paths.contains(&old.path) {
            managed.push(old.clone());
            continue;
        }
        if retained.contains(&old.path) {
            continue;
        }
        match state::remove_if_managed(Path::new(&old.path), cache) {
            Ok(true) => {
                report.summary.pruned += 1;
                report.events.push(SyncEvent::Pruned {
                    skill: old.skill.clone(),
                    harness: old.harness.clone(),
                });
            }
            Ok(false) => {}
            Err(error) => {
                managed.push(old.clone());
                report.summary.errors += 1;
                report.events.push(SyncEvent::PruneFailed {
                    path: old.path.clone(),
                    error: error.to_string(),
                });
            }
        }
    }
    let managed: BTreeMap<_, _> = managed
        .into_iter()
        .map(|link| ((link.path.clone(), link.bundle.clone()), link))
        .collect();
    if let Err(error) = (state::Manifest {
        links: managed.into_values().collect(),
    })
    .save(state_dir)
    {
        report.summary.errors += 1;
        report.events.push(SyncEvent::StateFailed {
            error: format!("{error:#}"),
        });
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn interrupted_apply_keeps_intended_ownership_for_the_next_sync() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        write_skill_dir(&source.join("skills/demo"), "demo");
        let cache = temp.path().join("cache");
        let target = temp.path().join("target/demo");
        let bundles = HashMap::from([(
            "fixture".into(),
            bundle_with_source(&source, vec!["fixture"]),
        )]);
        let registry = HashMap::from([(
            "fixture".into(),
            harnesses::HarnessDef {
                label: "Fixture".into(),
                pattern: Some(
                    temp.path()
                        .join("target/{name}")
                        .to_string_lossy()
                        .into_owned(),
                ),
                mcp: None,
            },
        )]);
        let interrupted = std::panic::catch_unwind(|| {
            sync_with_progress(&bundles, &registry, &cache, &cache, 1, &mut |event| {
                if matches!(event, fetch_plan::Progress::Linking { .. }) {
                    let owned = state::Manifest::load(&cache).unwrap();
                    assert_eq!(owned.links.len(), 1);
                    assert_eq!(owned.links[0].path, target.to_string_lossy());
                    assert!(!target.exists());
                    assert_eq!(
                        linker::ensure_skill_symlink(
                            &cache.join("bundles/fixture/skills/demo"),
                            &target
                        )
                        .status,
                        linker::SyncStatus::Created
                    );
                    panic!("fixture interruption after one link");
                }
            })
        });
        assert!(interrupted.is_err());
        assert!(target.is_symlink());
        let retry = sync_with_registry(&HashMap::new(), &registry, &cache, &cache, temp.path());
        assert!(!retry.summary.has_failures());
        assert_eq!(retry.summary.pruned, 1);
        assert!(!target.is_symlink());
        assert!(state::Manifest::load(&cache).unwrap().links.is_empty());
    }

    #[test]
    fn worktree_waits_for_cache_publication_before_reading_or_pruning() {
        use std::sync::mpsc;
        use std::time::Duration;

        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let source = temp.path().join("source");
        let main = temp.path().join("main");
        let linked = temp.path().join("linked");
        fs::create_dir_all(&linked).unwrap();
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
        let manifest = state::Manifest::load(&cache).unwrap();
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
        let manifest = state::Manifest::load(&cache).unwrap();
        assert_eq!(manifest.links.len(), 1);
        assert_eq!(manifest.links[0].path, target.to_string_lossy());
        assert!(report.events.iter().any(|event| {
            matches!(
                event,
                SyncEvent::BundleFailed { bundle, error }
                    if bundle == "generic" && error.contains("missing")
            )
        }));
    }
}
