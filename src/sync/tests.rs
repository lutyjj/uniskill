use super::*;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        Self(crate::test_support::tempdir().unwrap())
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.path().join(name)
    }
    fn write(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = self.path(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }
    fn skill(&self, name: &str, content: &str) {
        self.write(&format!("{name}/SKILL.md"), content);
    }
    fn alias_bundle_storage(&self) {
        fs::create_dir_all(self.path("cache/uniskill")).unwrap();
        fs::create_dir(self.path("store")).unwrap();
        std::os::unix::fs::symlink(self.path("store"), self.path("cache/uniskill/bundles"))
            .unwrap();
    }
    fn sync(&self, path: &Path) -> SyncReport {
        let config = config::parse_config(path).unwrap();
        let registry = harnesses::registry(&config).unwrap();
        let cache = self.path("cache/uniskill");
        sync_with_progress(
            &config.bundles,
            &registry,
            &cache,
            &cache,
            &crate::filesystem::Reservations::default(),
            1,
            &mut |_| {},
        )
    }
}

fn snapshot(path: &Path) -> (u64, i64, i64) {
    let m = fs::symlink_metadata(path).unwrap();
    (m.ino(), m.mtime(), m.mtime_nsec())
}

const LOCAL: &str = r#"
[harnesses.fixture]
pattern = "target/{name}"
[bundles.one]
harnesses = ["fixture"]
[bundles.one.skills.demo]
source = "source"
"#;

#[test]
fn interrupted_apply_keeps_intended_ownership_for_the_next_sync() {
    let temp = crate::test_support::tempdir().unwrap();
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
        sync_with_progress(
            &bundles,
            &registry,
            &cache,
            &cache,
            &crate::filesystem::Reservations::default(),
            1,
            &mut |event| {
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
            },
        )
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

    let temp = crate::test_support::tempdir().unwrap();
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
    let bundles = HashMap::from([("local".into(), bundle_with_source(&source, vec!["fixture"]))]);
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
    assert!(!sync_worktree(
        &bundles,
        &registry,
        &cache,
        &state_dir,
        &ctx,
        &crate::filesystem::Reservations::default()
    )
    .summary
    .has_failures());
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
                .send(sync_worktree(
                    &bundles,
                    &registry,
                    &cache,
                    &state_dir,
                    &ctx,
                    &crate::filesystem::Reservations::default(),
                ))
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
fn cache_storage_aliases_cannot_be_replaced_by_skill_routes() {
    for (kind, installed) in [
        ("bundles", false),
        ("cache-root", true),
        ("cache-parent", false),
        ("alias-chain", true),
    ] {
        let f = Fixture::new();
        f.skill("source", "one");
        f.skill("other", "two");
        f.write("other/one/manual.txt", "preserve user data");
        fs::create_dir_all(f.path("store")).unwrap();
        let (alias, pattern, name) = match kind {
            "bundles" => {
                fs::create_dir_all(f.path("cache/uniskill")).unwrap();
                ("cache/uniskill/bundles", "cache/uniskill/{name}", "bundles")
            }
            "cache-root" => {
                fs::create_dir(f.path("cache")).unwrap();
                ("cache/uniskill", "cache/{name}", "uniskill")
            }
            "cache-parent" => ("cache", "{name}", "cache"),
            "alias-chain" => {
                fs::create_dir_all(f.path("cache/uniskill")).unwrap();
                std::os::unix::fs::symlink("../../alias", f.path("cache/uniskill/bundles"))
                    .unwrap();
                ("alias", "{name}", "alias")
            }
            _ => unreachable!(),
        };
        let alias = f.path(alias);
        std::os::unix::fs::symlink(f.path("store"), &alias).unwrap();
        let config = f.write("config.toml", LOCAL);
        if installed {
            assert!(!f.sync(&config).summary.has_failures());
        }
        let before = (snapshot(&alias), fs::read_link(&alias).unwrap());
        f.write(
                "config.toml",
                format!("{LOCAL}\n[harnesses.storage]\npattern='{pattern}'\n[bundles.two]\nharnesses=['storage']\n[bundles.two.skills.{name}]\nsource='other'\n"),
            );
        for _ in 0..2 {
            let output = f.sync(&config);
            assert!(
                output.summary.has_failures(),
                "{kind}, installed={installed}"
            );
            assert!(output
                .events
                .iter()
                .any(|event| matches!(event, SyncEvent::BundleFailed { .. })));
            assert_eq!((snapshot(&alias), fs::read_link(&alias).unwrap()), before);
            assert_eq!(
                fs::read_to_string(f.path("other/one/manual.txt")).unwrap(),
                "preserve user data"
            );
            assert!(!f.path("cache/uniskill/bundles/two").exists());
        }
        f.write("config.toml", LOCAL);
        assert!(!f.sync(&config).summary.has_failures());
        assert_eq!(fs::read_link(&alias).unwrap(), f.path("store"));
        assert_eq!(
            fs::read_to_string(f.path("other/one/manual.txt")).unwrap(),
            "preserve user data"
        );
    }
}

#[test]
fn loaded_ownership_cannot_prune_cache_storage_aliases() {
    let f = Fixture::new();
    f.skill("source", "one");
    f.alias_bundle_storage();
    let config = f.write("config.toml", LOCAL);
    assert!(!f.sync(&config).summary.has_failures());
    let alias = f.path("cache/uniskill/bundles");
    let state = f.path("cache/uniskill/state.toml");
    let original = fs::read_to_string(&state).unwrap();
    fs::write(&state, format!("{original}\n[[links]]\npath={:?}\nskill='bundles'\nharness='storage'\nbundle='removed'\n", alias.to_string_lossy())).unwrap();
    let before = (
        snapshot(&alias),
        fs::read(&state).unwrap(),
        snapshot(&f.path("target/demo")),
    );
    for _ in 0..2 {
        assert!(f.sync(&config).summary.has_failures());
        assert_eq!(
            (
                snapshot(&alias),
                fs::read(&state).unwrap(),
                snapshot(&f.path("target/demo"))
            ),
            before
        );
        assert_eq!(fs::read_link(&alias).unwrap(), f.path("store"));
    }
    fs::write(&state, original).unwrap();
    assert!(!f.sync(&config).summary.has_failures());
}

#[test]
fn aliases_into_installed_skills_cannot_write_below_their_exposed_roots() {
    for kind in ["live", "copied", "failed-owner"] {
        let f = Fixture::new();
        f.skill("source", "one");
        f.skill("other", "two");
        let initial = if kind == "copied" {
            LOCAL.replace("[bundles.one]", "[bundles.one]\nlink=false")
        } else {
            LOCAL.to_owned()
        };
        let config = f.write("config.toml", &initial);
        assert!(!f.sync(&config).summary.has_failures());
        std::os::unix::fs::symlink(f.path("target/demo"), f.path("alias")).unwrap();
        let state = f.path("cache/uniskill/state.toml");
        let cache = f.path("cache/uniskill/bundles/one/skills/demo");
        let before = (
            snapshot(&f.path("target/demo")),
            snapshot(&cache),
            fs::read(&state).unwrap(),
        );
        let changed = if kind == "failed-owner" {
            initial.replace("harnesses = [\"fixture\"]", "harnesses=['missing']")
        } else {
            initial
        };
        f.write("config.toml", changed + "\n[harnesses.nested]\npattern='alias/{name}'\n[bundles.two]\nharnesses=['nested']\n[bundles.two.skills.extra]\nsource='other'\n");
        for _ in 0..2 {
            let output = f.sync(&config);
            assert!(output.summary.has_failures(), "{kind}");
            assert!(output
                .events
                .iter()
                .any(|event| matches!(event, SyncEvent::BundleFailed { .. })));
            assert_eq!(
                (
                    snapshot(&f.path("target/demo")),
                    snapshot(&cache),
                    fs::read(&state).unwrap()
                ),
                before,
                "{kind}"
            );
            assert_eq!(fs::read_to_string(cache.join("SKILL.md")).unwrap(), "one");
            assert!(!cache.join("extra").exists());
            assert!(!f.path("source/extra").exists());
            assert!(!f.path("cache/uniskill/bundles/two").exists());
        }
    }
}

#[test]
fn planned_skill_footprints_protect_sources_and_all_future_cache_entries() {
    for aliased in [false, true] {
        for kind in ["live-source", "live-cache", "copied-cache"] {
            let f = Fixture::new();
            f.skill("source", "one");
            f.skill("other", "two");
            std::os::unix::fs::symlink(f.path("source"), f.path("alias")).unwrap();
            if aliased {
                f.alias_bundle_storage();
            }
            let initial = if kind == "copied-cache" {
                LOCAL.replace("[bundles.one]", "[bundles.one]\nlink=false")
            } else {
                LOCAL.to_owned()
            };
            let pattern = if kind == "live-source" {
                "alias/{name}"
            } else {
                "cache/uniskill/bundles/one/skills/demo/{name}"
            };
            let config = f.write("config.toml", format!("{initial}\n[harnesses.nested]\npattern='{pattern}'\n[bundles.two]\nharnesses=['nested']\n[bundles.two.skills.extra]\nsource='other'\n"));
            for _ in 0..2 {
                let output = f.sync(&config);
                assert!(output.summary.has_failures(), "{kind}, alias={aliased}");
                assert!(output
                    .events
                    .iter()
                    .any(|event| matches!(event, SyncEvent::BundleFailed { .. })));
                assert!(!f.path("source/extra").exists());
                assert!(!f.path("target/demo").exists());
                assert!(!f.path("cache/uniskill/bundles/one").exists());
                assert!(!f.path("cache/uniskill/bundles/two").exists());
                assert!(!f.path("cache/uniskill/state.toml").exists());
            }
        }
    }
}

#[test]
fn aliased_bundle_storage_supports_install_reapply_and_owned_pruning() {
    for copied in [false, true] {
        let f = Fixture::new();
        f.skill("source", "one");
        f.alias_bundle_storage();
        f.write("store/manual", "preserve");
        let config = f.write(
            "config.toml",
            if copied {
                LOCAL.replace("[bundles.one]", "[bundles.one]\nlink=false")
            } else {
                LOCAL.to_owned()
            },
        );
        assert!(!f.sync(&config).summary.has_failures());
        let paths = [
            "target/demo",
            "cache/uniskill/state.toml",
            "store/one",
            "store/one/skills/demo",
            "cache/uniskill/bundles",
        ];
        let before = paths.map(|p| snapshot(&f.path(p)));
        assert!(!f.sync(&config).summary.has_failures());
        assert_eq!(paths.map(|p| snapshot(&f.path(p))), before);
        f.write("config.toml", "");
        assert!(!f.sync(&config).summary.has_failures());
        assert!(!f.path("target/demo").is_symlink());
        assert_eq!(
            fs::read_to_string(f.path("source/SKILL.md")).unwrap(),
            "one"
        );
        assert_eq!(
            fs::read_to_string(f.path("store/manual")).unwrap(),
            "preserve"
        );
        assert!(f.path("cache/uniskill/bundles").is_symlink());
    }
}

#[test]
fn published_cache_entries_cannot_be_replaced_by_other_skill_routes() {
    for kind in ["fresh", "installed", "failed-owner"] {
        for aliased in [false, true] {
            let f = Fixture::new();
            f.skill("source", "one");
            f.skill("other", "two");
            if aliased {
                f.alias_bundle_storage();
            }
            let config = f.write("config.toml", LOCAL);
            let state = f.path("cache/uniskill/state.toml");
            if kind != "fresh" {
                assert!(!f.sync(&config).summary.has_failures());
            }
            let original = fs::read(&state).ok();
            let link = f.path("target/demo");
            let before = fs::symlink_metadata(&link).ok().map(|_| snapshot(&link));
            let owner = if kind == "failed-owner" {
                LOCAL.replace("harnesses = [\"fixture\"]", "harnesses=['missing']")
            } else {
                LOCAL.to_owned()
            };
            f.write("config.toml", owner + "\n[harnesses.cache-entry]\npattern='cache/uniskill/bundles/one/skills/{name}'\n[bundles.two]\nharnesses=['cache-entry']\n[bundles.two.skills.demo]\nsource='other'\n");
            for _ in 0..2 {
                let output = f.sync(&config);
                assert!(output.summary.has_failures(), "{kind}, alias={aliased}");
                assert!(output
                    .events
                    .iter()
                    .any(|event| matches!(event, SyncEvent::BundleFailed { .. })));
                assert_eq!(fs::read(&state).ok(), original);
                assert_eq!(
                    fs::symlink_metadata(&link).ok().map(|_| snapshot(&link)),
                    before
                );
                assert!(!f.path("cache/uniskill/bundles/two").exists());
                if kind == "fresh" {
                    assert!(!f.path("cache/uniskill/bundles/one").exists());
                } else {
                    assert_eq!(fs::read_to_string(link.join("SKILL.md")).unwrap(), "one");
                }
            }
        }
    }
}

#[test]
fn publication_preserves_legacy_harness_claims_inside_bundle_storage() {
    for (aliased, location, retained) in [
        (false, "root", false),
        (true, "descendant", true),
        (false, "staging", true),
        (true, "previous", false),
    ] {
        let f = Fixture::new();
        f.skill("source", "one");
        f.skill("other", "two");
        if aliased {
            f.alias_bundle_storage();
        }
        let two =
            "[bundles.two]\nharnesses=['fixture']\n[bundles.two.skills.demo]\nsource='other'\n";
        let config = f.write(
            "config.toml",
            format!("[harnesses.fixture]\npattern='retired/{{name}}'\n{two}"),
        );
        assert!(!f.sync(&config).summary.has_failures());
        let old = f.path(match location {
            "root" => "cache/uniskill/bundles/one",
            "descendant" => "cache/uniskill/bundles/one/skills/retired",
            "staging" => "cache/uniskill/bundles/.one.staging/demo",
            "previous" => "cache/uniskill/bundles/.one.previous/demo",
            _ => unreachable!(),
        });
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::rename(f.path("retired/demo"), &old).unwrap();
        let state = f.path("cache/uniskill/state.toml");
        let body = fs::read_to_string(&state).unwrap().replace(
            &format!("{:?}", f.path("retired/demo").to_string_lossy()),
            &format!(
                "{:?}",
                old.parent()
                    .unwrap()
                    .canonicalize()
                    .unwrap()
                    .join(old.file_name().unwrap())
                    .to_string_lossy()
            ),
        );
        fs::write(&state, body).unwrap();
        let before = (
            snapshot(&old),
            fs::read_link(&old).unwrap(),
            fs::read(&state).unwrap(),
        );
        let owner = if retained {
            two.replace("harnesses=['fixture']", "harnesses=['missing']")
        } else {
            String::new()
        };
        f.write("config.toml", format!("{LOCAL}\n{owner}"));
        for _ in 0..2 {
            assert!(f.sync(&config).summary.has_failures());
            assert!(!f.path("target/demo").exists());
            if retained {
                assert_eq!(
                    (
                        snapshot(&old),
                        fs::read_link(&old).unwrap(),
                        fs::read(&state).unwrap()
                    ),
                    before
                );
                assert_eq!(fs::read_to_string(old.join("SKILL.md")).unwrap(), "two");
            } else {
                break;
            }
        }
        if !retained {
            assert!(!old.is_symlink());
            assert!(!f.sync(&config).summary.has_failures());
            assert_eq!(
                fs::read_to_string(f.path("target/demo/SKILL.md")).unwrap(),
                "one"
            );
        }
    }
}

#[test]
fn live_sources_cannot_depend_on_mutable_cache_storage() {
    for aliased in [false, true] {
        for dependency in ["self", "other"] {
            let f = Fixture::new();
            f.skill("source", "one");
            if aliased {
                f.alias_bundle_storage();
            }
            let initial = LOCAL.replace("[bundles.one]", "[bundles.one]\nlink=false");
            let config = f.write("config.toml", &initial);
            assert!(!f.sync(&config).summary.has_failures());
            let paths = [
                "target/demo",
                "cache/uniskill/state.toml",
                "cache/uniskill/bundles/one/skills/demo",
            ];
            let before = paths.map(|p| (snapshot(&f.path(p)), fs::read(f.path(p)).ok()));
            let updated = if dependency == "self" {
                LOCAL.replace(
                    "source = \"source\"",
                    "source='cache/uniskill/bundles/one/skills/demo'",
                )
            } else {
                format!("{initial}\n[harnesses.second]\npattern='second/{{name}}'\n[bundles.two]\nharnesses=['second']\n[bundles.two.skills.extra]\nsource='cache/uniskill/bundles/one/skills/demo'\n")
            };
            f.write("config.toml", &updated);
            for _ in 0..2 {
                assert!(f.sync(&config).summary.has_failures());
                assert_eq!(
                    paths.map(|p| (snapshot(&f.path(p)), fs::read(f.path(p)).ok())),
                    before
                );
                assert_eq!(
                    fs::read_to_string(f.path("target/demo/SKILL.md")).unwrap(),
                    "one"
                );
                assert!(!f.path("second/extra").exists());
            }
            let copy = if dependency == "self" {
                updated.replace("[bundles.one]", "[bundles.one]\nlink=false")
            } else {
                updated.replace("[bundles.two]", "[bundles.two]\nlink=false")
            };
            f.write("config.toml", copy);
            assert!(!f.sync(&config).summary.has_failures());
            let installed = if dependency == "self" {
                "target/demo"
            } else {
                "second/extra"
            };
            let before = snapshot(&f.path(installed));
            assert!(!f.sync(&config).summary.has_failures());
            assert_eq!(snapshot(&f.path(installed)), before);
            assert_eq!(
                fs::read_to_string(f.path(installed).join("SKILL.md")).unwrap(),
                "one"
            );
        }
    }
}

#[test]
fn missing_skill_roots_keep_their_recorded_target_directories_reserved() {
    for kind in ["copied-cache", "live-source"] {
        let f = Fixture::new();
        f.skill("source", "one");
        f.skill("other", "two");
        let initial = if kind == "copied-cache" {
            LOCAL.replace("[bundles.one]", "[bundles.one]\nlink=false")
        } else {
            LOCAL.to_owned()
        };
        let config = f.write("config.toml", &initial);
        assert!(!f.sync(&config).summary.has_failures());
        let (owner, pattern, missing) = if kind == "copied-cache" {
            fs::rename(f.path("cache/uniskill/bundles/one"), f.path("displaced")).unwrap();
            (
                initial.replace("harnesses = [\"fixture\"]", "harnesses=['missing']"),
                "cache/uniskill/bundles/one/skills/demo/{name}",
                f.path("cache/uniskill/bundles/one"),
            )
        } else {
            fs::remove_dir_all(f.path("source")).unwrap();
            (initial, "source/{name}", f.path("source"))
        };
        let before = (
            snapshot(&f.path("target/demo")),
            fs::read(f.path("cache/uniskill/state.toml")).unwrap(),
        );
        f.write("config.toml", format!("{owner}\n[harnesses.nested]\npattern='{pattern}'\n[bundles.two]\nharnesses=['nested']\n[bundles.two.skills.extra]\nsource='other'\n"));
        for _ in 0..2 {
            let output = f.sync(&config);
            assert!(output.summary.has_failures());
            assert!(output
                .events
                .iter()
                .any(|event| matches!(event, SyncEvent::BundleFailed { .. })));
            assert_eq!(
                (
                    snapshot(&f.path("target/demo")),
                    fs::read(f.path("cache/uniskill/state.toml")).unwrap()
                ),
                before
            );
            assert!(!missing.exists());
            assert!(!f.path("cache/uniskill/bundles/two").exists());
        }
    }
}

#[test]
fn multiple_entries_can_expose_one_live_directory_without_false_overlaps() {
    let f = Fixture::new();
    f.skill("source", "one");
    let config = f.write(
        "config.toml",
        LOCAL
            .replace(
                "harnesses = [\"fixture\"]",
                "harnesses=['fixture','mirror']",
            )
            .replace("target/{name}", "source/../target/{name}")
            + "\n[harnesses.mirror]\npattern='mirror/{name}'\n",
    );
    assert!(!f.sync(&config).summary.has_failures());
    let paths = ["target/demo", "mirror/demo", "cache/uniskill/state.toml"];
    let before = paths.map(|p| snapshot(&f.path(p)));
    assert!(!f.sync(&config).summary.has_failures());
    assert_eq!(paths.map(|p| snapshot(&f.path(p))), before);
    for path in ["target/demo", "mirror/demo"] {
        assert_eq!(fs::canonicalize(f.path(path)).unwrap(), f.path("source"));
    }
}

#[test]
fn legacy_destination_aliases_upgrade_without_pruning_the_live_link() {
    let f = Fixture::new();
    f.skill("source", "fixture");
    fs::create_dir(f.path("target")).unwrap();
    std::os::unix::fs::symlink(f.path("target"), f.path("alias")).unwrap();
    let config = f.write(
        "config.toml",
        LOCAL.to_owned() + "\n[harnesses.alias]\npattern='alias/{name}'\n",
    );
    assert!(!f.sync(&config).summary.has_failures());
    let state = f.path("cache/uniskill/state.toml");
    let mut value: toml::Value = toml::from_str(&fs::read_to_string(&state).unwrap()).unwrap();
    let mut alias = value["links"][0].clone();
    alias["path"] = toml::Value::String(f.path("alias/demo").to_string_lossy().into_owned());
    alias["harness"] = toml::Value::String("alias".into());
    value["links"].as_array_mut().unwrap().push(alias);
    fs::write(&state, toml::to_string(&value).unwrap()).unwrap();
    let before = snapshot(&f.path("target/demo"));
    assert!(!f.sync(&config).summary.has_failures());
    assert_eq!(snapshot(&f.path("target/demo")), before);
    let upgraded = snapshot(&state);
    assert!(!f.sync(&config).summary.has_failures());
    assert_eq!(snapshot(&f.path("target/demo")), before);
    assert_eq!(snapshot(&state), upgraded);
}

#[test]
fn failed_bundles_reserve_installed_destinations_until_they_are_removed() {
    let f = Fixture::new();
    f.skill("source", "one");
    f.skill("other", "two");
    let one = LOCAL.replace("[bundles.one]", "[bundles.one]\nlink=false");
    let config = f.write("config.toml", &one);
    assert!(!f.sync(&config).summary.has_failures());
    let before = snapshot(&f.path("target/demo"));
    fs::remove_dir_all(f.path("source")).unwrap();
    let two = "[bundles.two]\nharnesses=['fixture']\n[bundles.two.skills.demo]\nsource='other'\n";
    f.write("config.toml", format!("{one}\n{two}"));
    for _ in 0..2 {
        assert!(f.sync(&config).summary.has_failures());
        assert_eq!(snapshot(&f.path("target/demo")), before);
        assert_eq!(
            fs::read_to_string(f.path("target/demo/SKILL.md")).unwrap(),
            "one"
        );
        assert!(!f.path("cache/uniskill/bundles/two").exists());
    }
    f.write(
        "config.toml",
        format!("[harnesses.fixture]\npattern='target/{{name}}'\n{two}"),
    );
    assert!(!f.sync(&config).summary.has_failures());
    assert_eq!(
        fs::read_to_string(f.path("target/demo/SKILL.md")).unwrap(),
        "two"
    );
}

#[test]
fn legacy_alias_claims_cannot_prune_a_failed_owners_physical_link() {
    let f = Fixture::new();
    f.skill("source", "one");
    fs::create_dir(f.path("target")).unwrap();
    std::os::unix::fs::symlink(f.path("target"), f.path("alias")).unwrap();
    let config = f.write("config.toml", LOCAL);
    assert!(!f.sync(&config).summary.has_failures());
    let state = f.path("cache/uniskill/state.toml");
    let mut value: toml::Value = toml::from_str(&fs::read_to_string(&state).unwrap()).unwrap();
    let mut old = value["links"][0].clone();
    old["path"] = toml::Value::String(f.path("alias/demo").to_string_lossy().into_owned());
    old["bundle"] = toml::Value::String("retired".into());
    value["links"].as_array_mut().unwrap().push(old);
    fs::write(&state, toml::to_string(&value).unwrap()).unwrap();
    f.write(
        "config.toml",
        LOCAL.replace("harnesses = [\"fixture\"]", "harnesses=['missing']"),
    );
    let before = snapshot(&f.path("target/demo"));
    for _ in 0..2 {
        assert!(f.sync(&config).summary.has_failures());
        assert_eq!(snapshot(&f.path("target/demo")), before);
        assert_eq!(
            fs::read_to_string(f.path("target/demo/SKILL.md")).unwrap(),
            "one"
        );
        let value: toml::Value = toml::from_str(&fs::read_to_string(&state).unwrap()).unwrap();
        assert_eq!(value["links"].as_array().unwrap().len(), 1);
        assert_eq!(value["links"][0]["bundle"].as_str(), Some("one"));
    }
}

#[test]
fn failed_ownership_reservations_propagate_across_bundle_handoffs() {
    let f = Fixture::new();
    f.skill("source", "one");
    f.skill("other", "two");
    f.skill("third", "three");
    let initial = format!("{LOCAL}\n[harnesses.second]\npattern='second/{{name}}'\n[bundles.two]\nharnesses=['second']\n[bundles.two.skills.demo]\nsource='other'\n");
    let config = f.write("config.toml", &initial);
    assert!(!f.sync(&config).summary.has_failures());
    let paths = ["target/demo", "second/demo", "cache/uniskill/state.toml"];
    let before = paths.map(|path| {
        (
            snapshot(&f.path(path)),
            fs::read_link(f.path(path)).ok(),
            fs::read(f.path(path)).ok(),
        )
    });
    f.write("config.toml", initial.replace("harnesses = [\"fixture\"]", "harnesses = ['missing']").replace("harnesses=['second']", "harnesses=['fixture']") + "\n[bundles.three]\nharnesses=['second']\n[bundles.three.skills.demo]\nsource='third'\n");
    for _ in 0..2 {
        assert!(f.sync(&config).summary.has_failures());
        assert_eq!(
            paths.map(|path| (
                snapshot(&f.path(path)),
                fs::read_link(f.path(path)).ok(),
                fs::read(f.path(path)).ok()
            )),
            before
        );
        assert_eq!(
            fs::read_to_string(f.path("target/demo/SKILL.md")).unwrap(),
            "one"
        );
        assert_eq!(
            fs::read_to_string(f.path("second/demo/SKILL.md")).unwrap(),
            "two"
        );
        assert!(!f.path("cache/uniskill/bundles/three").exists());
    }
}

#[test]
fn a_bundle_cannot_move_its_destination_below_its_existing_skill_root() {
    let f = Fixture::new();
    f.skill("source", "one");
    let config = f.write("config.toml", LOCAL);
    assert!(!f.sync(&config).summary.has_failures());
    let before = (
        snapshot(&f.path("target/demo")),
        fs::read(f.path("cache/uniskill/state.toml")).unwrap(),
    );
    f.write(
        "config.toml",
        LOCAL.replace("target/{name}", "target/demo/{name}"),
    );
    assert!(f.sync(&config).summary.has_failures());
    assert_eq!(
        (
            snapshot(&f.path("target/demo")),
            fs::read(f.path("cache/uniskill/state.toml")).unwrap()
        ),
        before
    );
    assert!(!f.path("source/demo").exists());
}

#[test]
fn destination_handoffs_wait_for_the_configured_owner_to_retire_its_link() {
    let f = Fixture::new();
    f.skill("source", "one");
    f.skill("other", "two");
    let config = f.write("config.toml", LOCAL);
    assert!(!f.sync(&config).summary.has_failures());
    f.write(
        "config.toml",
        LOCAL.replace("skills.demo", "skills.moved")
            + "\n[bundles.two]\nharnesses=['fixture']\n[bundles.two.skills.demo]\nsource='other'\n",
    );
    assert!(f.sync(&config).summary.has_failures());
    assert!(!f.path("target/demo").is_symlink());
    assert_eq!(
        fs::read_to_string(f.path("target/moved/SKILL.md")).unwrap(),
        "one"
    );
    assert!(!f.path("cache/uniskill/bundles/two").exists());
    assert!(!f.sync(&config).summary.has_failures());
    assert_eq!(
        fs::read_to_string(f.path("target/demo/SKILL.md")).unwrap(),
        "two"
    );
    let before = (
        snapshot(&f.path("target/demo")),
        snapshot(&f.path("target/moved")),
        snapshot(&f.path("cache/uniskill/state.toml")),
    );
    assert!(!f.sync(&config).summary.has_failures());
    assert_eq!(
        (
            snapshot(&f.path("target/demo")),
            snapshot(&f.path("target/moved")),
            snapshot(&f.path("cache/uniskill/state.toml"))
        ),
        before
    );
}

#[test]
fn symlinked_and_oversized_skill_ownership_fail_before_application() {
    for kind in ["symlink", "oversized"] {
        let f = Fixture::new();
        f.skill("source", "fixture");
        let config = f.write("config.toml", LOCAL);
        let state = f.path("cache/uniskill/state.toml");
        fs::create_dir_all(state.parent().unwrap()).unwrap();
        if kind == "symlink" {
            let outside = f.write("outside.toml", "links=[]\n");
            std::os::unix::fs::symlink(outside, &state).unwrap();
        } else {
            fs::File::create(&state)
                .unwrap()
                .set_len(64 * 1024 * 1024 + 1)
                .unwrap();
        }
        let before = snapshot(&state);
        assert!(f.sync(&config).summary.has_failures(), "{kind}");
        assert_eq!(snapshot(&state), before, "{kind}");
        assert!(!f.path("target/demo").exists());
        assert!(!f.path("cache/uniskill/bundles").exists());
        if kind == "symlink" {
            assert_eq!(
                fs::read_to_string(f.path("outside.toml")).unwrap(),
                "links=[]\n"
            );
            assert!(state.is_symlink());
        }
    }
}

#[test]
fn competing_skill_routes_fail_without_replacing_the_installed_link() {
    let f = Fixture::new();
    f.skill("source", "one");
    f.skill("other", "two");
    let local = LOCAL.replace("[bundles.one]", "[bundles.one]\nlink = false");
    let config = f.write("config.toml", &local);
    assert!(!f.sync(&config).summary.has_failures());
    let link = f.path("target/demo");
    let state = f.path("cache/uniskill/state.toml");
    let before = (
        snapshot(&link),
        fs::read_link(&link).unwrap(),
        fs::read(&state).unwrap(),
    );
    f.skill("source", "updated input");
    f.write("config.toml", format!("{local}\n[bundles.two]\nharnesses = [\"fixture\"]\n[bundles.two.skills.demo]\nsource = \"other\"\n"));
    for _ in 0..2 {
        assert!(f.sync(&config).summary.has_failures());
        assert_eq!(
            (
                snapshot(&link),
                fs::read_link(&link).unwrap(),
                fs::read(&state).unwrap()
            ),
            before
        );
        assert_eq!(fs::read_to_string(link.join("SKILL.md")).unwrap(), "one");
        assert!(!f.path("cache/uniskill/bundles/two").exists());
    }
}

#[test]
fn equivalent_harness_routes_apply_and_record_one_destination() {
    let f = Fixture::new();
    f.skill("source", "one");
    let config = f.write(
        "config.toml",
        LOCAL.replace(
            "harnesses = [\"fixture\"]",
            "harnesses = [\"fixture\", \"alias\"]",
        ) + "\n[harnesses.alias]\npattern = \"target/{name}\"\n",
    );
    assert!(!f.sync(&config).summary.has_failures());
    let state = f.path("cache/uniskill/state.toml");
    let value: toml::Value = toml::from_str(&fs::read_to_string(&state).unwrap()).unwrap();
    assert_eq!(value["links"].as_array().unwrap().len(), 1);
    let before = (snapshot(&f.path("target/demo")), snapshot(&state));
    assert!(!f.sync(&config).summary.has_failures());
    assert_eq!((snapshot(&f.path("target/demo")), snapshot(&state)), before);
}

#[test]
fn unavailable_ownership_state_fails_and_retry_can_install_then_prune() {
    let f = Fixture::new();
    f.skill("source", "one");
    let config = f.write("config.toml", LOCAL);
    let state = f.path("cache/uniskill/state.toml");
    fs::create_dir_all(&state).unwrap();
    assert!(f.sync(&config).summary.has_failures());
    fs::remove_dir(&state).unwrap();
    assert!(!f.sync(&config).summary.has_failures());
    f.write("config.toml", "");
    assert!(!f.sync(&config).summary.has_failures());
    assert!(fs::symlink_metadata(f.path("target/demo")).is_err());
}

#[test]
fn corrupt_ownership_state_is_preserved_and_fails_before_mutation() {
    let f = Fixture::new();
    f.skill("source", "one");
    let config = f.write("config.toml", LOCAL);
    assert!(!f.sync(&config).summary.has_failures());
    let link = f.path("target/demo");
    let before = snapshot(&link);
    let state = f.write("cache/uniskill/state.toml", "invalid [[");
    assert!(f.sync(&config).summary.has_failures());
    assert_eq!(snapshot(&link), before);
    assert_eq!(fs::read_to_string(state).unwrap(), "invalid [[");
}

#[test]
fn failed_prune_retains_ownership_and_retries_after_permissions_recover() {
    let f = Fixture::new();
    f.skill("source", "one");
    let config = f.write("config.toml", LOCAL);
    assert!(!f.sync(&config).summary.has_failures());
    f.write("config.toml", "");
    let parent = f.path("target");
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
    let output = f.sync(&config);
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(output.summary.has_failures());
    assert!(f.path("target/demo").is_symlink());
    let state = fs::read_to_string(f.path("cache/uniskill/state.toml")).unwrap();
    assert!(state.contains("demo"));
    assert!(!f.sync(&config).summary.has_failures());
    assert!(!f.path("target/demo").is_symlink());
}

#[test]
fn interrupted_cache_publication_recovers_before_a_failed_rebuild() {
    let f = Fixture::new();
    f.skill("source", "one");
    let config = f.write(
        "config.toml",
        LOCAL.replace(
            "harnesses = [\"fixture\"]",
            "harnesses = [\"fixture\"]\nlink = false",
        ),
    );
    assert!(!f.sync(&config).summary.has_failures());
    let cache = f.path("cache/uniskill/bundles");
    fs::rename(cache.join("one"), cache.join(".one.previous")).unwrap();
    fs::remove_dir_all(f.path("source")).unwrap();
    assert!(f.sync(&config).summary.has_failures());
    assert_eq!(
        fs::read_to_string(f.path("target/demo/SKILL.md")).unwrap(),
        "one"
    );
    assert!(!cache.join(".one.previous").exists());
}
