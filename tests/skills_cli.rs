mod support;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        let f = Self(support::tempdir().unwrap());
        fs::create_dir_all(f.path("home")).unwrap();
        f
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.path().join(name)
    }

    fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.path("gitconfig"))
            .env("UNISKILL_FIXTURE_TOKEN", "expanded")
            .env(
                "UNISKILL_FIXTURE_DIR",
                self.path("literal-$UNISKILL_FIXTURE_TOKEN"),
            )
            .current_dir(self.0.path());
        cmd
    }

    fn run(&self, config: &Path, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_uniskill"))
            .arg("--config")
            .arg(config)
            .arg("sync")
            .args(args)
            .output()
            .unwrap()
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

    fn git(&self, dir: &Path, args: &[&str]) -> Output {
        self.command("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
    }

    fn repo(&self, name: &str) -> PathBuf {
        let dir = self.path(name);
        fs::create_dir_all(&dir).unwrap();
        success(self.git(&dir, &["init", "-q", "-b", "main"]));
        success(self.git(&dir, &["config", "user.name", "Fixture"]));
        success(self.git(&dir, &["config", "user.email", "fixture@example.invalid"]));
        dir
    }
}

fn success(output: Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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

const NATIVE_MCP: &str = "[harnesses.native.mcp]\npath='native.json'\nformat='json'\ntable=['mcpServers']\n[mcp.docs]\nharnesses=['native']\ncommand='echo'\nargs=['docs']\n";

#[test]
fn mcp_storage_aliases_remain_reachable_across_skill_reconciliation() {
    for worktree in [false, true] {
        for loaded in [false, true] {
            let f = Fixture::new();
            let initial = format!("{NATIVE_MCP}\n{LOCAL}\n[harnesses.second]\npattern='safe/{{name}}'\n[bundles.two]\nharnesses=['second']\n[bundles.two.skills.state]\nsource='other'\n");
            let (base, config, cache) = if worktree {
                f.skill("main/other", "two");
                let linked = linked_project(&f, &initial);
                (
                    linked.clone(),
                    f.path("main/uniskill.toml"),
                    linked.join(".uniskill-cache"),
                )
            } else {
                f.skill("source", "one");
                f.skill("other", "two");
                (
                    f.0.path().to_path_buf(),
                    f.write("config.toml", &initial),
                    f.path("cache/uniskill"),
                )
            };
            fs::create_dir_all(&cache).unwrap();
            fs::create_dir(base.join("mcp-store")).unwrap();
            std::os::unix::fs::symlink("mcp-store", base.join("state")).unwrap();
            std::os::unix::fs::symlink(base.join("state"), cache.join("mcp")).unwrap();
            let run = || {
                if worktree {
                    f.command(env!("CARGO_BIN_EXE_uniskill"))
                        .args(["sync", "--worktree"])
                        .current_dir(&base)
                        .output()
                        .unwrap()
                } else {
                    f.run(&config, &[])
                }
            };
            success(run());
            let state = cache.join("state.toml");
            let original = fs::read_to_string(&state).unwrap();
            if loaded {
                fs::write(&state, format!("{original}\n[[links]]\npath={:?}\nskill='state'\nharness='retired'\nbundle='removed'\n", base.join("state").to_string_lossy())).unwrap();
            } else {
                fs::write(&config, initial.replace("safe/{name}", "{name}")).unwrap();
            }
            let paths = [
                base.join("state"),
                cache.join("mcp"),
                base.join("mcp-store/state.json"),
                base.join("native.json"),
                state.clone(),
                base.join("target/demo"),
                base.join("safe/state"),
            ];
            let before = paths.each_ref().map(|p| (snapshot(p), fs::read(p).ok()));
            for _ in 0..2 {
                assert!(
                    !run().status.success(),
                    "worktree={worktree}, loaded={loaded}"
                );
                assert_eq!(
                    paths.each_ref().map(|p| (snapshot(p), fs::read(p).ok())),
                    before
                );
                assert_eq!(
                    fs::read_link(base.join("state")).unwrap(),
                    Path::new("mcp-store")
                );
            }
            fs::write(&config, &initial).unwrap();
            fs::write(&state, original).unwrap();
            success(run());
            success(run());
            assert_eq!(
                fs::read_link(base.join("state")).unwrap(),
                Path::new("mcp-store")
            );
            assert!(base.join("mcp-store/state.json").is_file());
        }
    }
}

#[test]
fn cache_lifecycle_preserves_mcp_files_and_storage_dependencies() {
    for location in ["one/skills/demo", ".one.staging", ".one.previous"] {
        for storage in [false, true] {
            let f = Fixture::new();
            f.skill("source", "one");
            let local = LOCAL.replace("[bundles.one]", "[bundles.one]\nlink=false");
            let config = f.write("config.toml", &local);
            success(f.run(&config, &[]));
            let directory = f.path(&format!("cache/uniskill/bundles/{location}"));
            fs::create_dir_all(&directory).unwrap();
            let mcp = if storage {
                let store = directory.join("mcp-store");
                fs::create_dir(&store).unwrap();
                std::os::unix::fs::symlink(store, f.path("cache/uniskill/mcp")).unwrap();
                NATIVE_MCP.to_owned()
            } else {
                NATIVE_MCP.replace(
                    "native.json",
                    &format!("cache/uniskill/bundles/{location}/native.json"),
                )
            };
            f.write("config.toml", format!("{mcp}\n{local}"));
            assert!(
                !f.run(&config, &[]).status.success(),
                "{location}, storage={storage}"
            );
            let mut paths = vec![
                f.path("cache/uniskill/state.toml"),
                f.path("target/demo"),
                f.path("cache/uniskill/bundles/one/skills/demo/SKILL.md"),
            ];
            if storage {
                paths.push(f.path("cache/uniskill/mcp/state.json"));
            }
            let before = paths
                .iter()
                .map(|p| (snapshot(p), fs::read(p).ok()))
                .collect::<Vec<_>>();
            for _ in 0..2 {
                assert!(!f.run(&config, &[]).status.success());
                assert_eq!(
                    paths
                        .iter()
                        .map(|p| (snapshot(p), fs::read(p).ok()))
                        .collect::<Vec<_>>(),
                    before
                );
                assert_eq!(
                    fs::read_to_string(f.path("target/demo/SKILL.md")).unwrap(),
                    "one"
                );
            }
        }
    }
}

#[test]
fn native_mcp_file_aliases_are_protected_without_reserving_sibling_skill_paths() {
    let f = Fixture::new();
    f.skill("source", "one");
    f.skill("other", "two");
    fs::create_dir(f.path("native-store")).unwrap();
    std::os::unix::fs::symlink("native-store", f.path("native")).unwrap();
    let initial = format!(
        "{}\n{LOCAL}",
        NATIVE_MCP.replace("native.json", "native/config.json")
    );
    let config = f.write("config.toml", &initial);
    success(f.run(&config, &[]));
    let paths = [
        f.path("native"),
        f.path("native-store/config.json"),
        f.path("cache/uniskill/mcp/state.json"),
        f.path("cache/uniskill/state.toml"),
        f.path("target/demo"),
    ];
    let before = paths.each_ref().map(|p| (snapshot(p), fs::read(p).ok()));
    f.write("config.toml", format!("{initial}\n[harnesses.second]\npattern='{{name}}'\n[bundles.two]\nharnesses=['second']\n[bundles.two.skills.native]\nsource='other'\n"));
    for _ in 0..2 {
        assert!(!f.run(&config, &[]).status.success());
        assert_eq!(
            paths.each_ref().map(|p| (snapshot(p), fs::read(p).ok())),
            before
        );
        assert_eq!(
            fs::read_link(f.path("native")).unwrap(),
            Path::new("native-store")
        );
    }
    f.write(
        "config.toml",
        initial.replace("target/{name}", "native/{name}"),
    );
    success(f.run(&config, &[]));
    let before = snapshot(&f.path("native-store/demo"));
    success(f.run(&config, &[]));
    assert_eq!(snapshot(&f.path("native-store/demo")), before);
    assert_eq!(
        fs::read_to_string(f.path("native-store/demo/SKILL.md")).unwrap(),
        "one"
    );
    assert!(f.path("native-store/config.json").is_file());
}

#[test]
fn cache_storage_aliases_cannot_be_replaced_by_skill_routes() {
    for kind in ["bundles", "cache-root", "cache-parent", "alias-chain"] {
        for installed in [false, true] {
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
                success(f.run(&config, &[]));
            }
            let before = (snapshot(&alias), fs::read_link(&alias).unwrap());
            f.write(
                "config.toml",
                format!("{LOCAL}\n[harnesses.storage]\npattern='{pattern}'\n[bundles.two]\nharnesses=['storage']\n[bundles.two.skills.{name}]\nsource='other'\n"),
            );
            for _ in 0..2 {
                let output = f.run(&config, &[]);
                assert!(!output.status.success(), "{kind}, installed={installed}");
                assert!(String::from_utf8_lossy(&output.stdout).contains("overlap"));
                assert_eq!((snapshot(&alias), fs::read_link(&alias).unwrap()), before);
                assert_eq!(
                    fs::read_to_string(f.path("other/one/manual.txt")).unwrap(),
                    "preserve user data"
                );
                assert!(!f.path("cache/uniskill/bundles/two").exists());
            }
            f.write("config.toml", LOCAL);
            success(f.run(&config, &[]));
            assert_eq!(fs::read_link(&alias).unwrap(), f.path("store"));
            assert_eq!(
                fs::read_to_string(f.path("other/one/manual.txt")).unwrap(),
                "preserve user data"
            );
        }
    }
}

#[test]
fn loaded_ownership_cannot_prune_cache_storage_aliases() {
    let f = Fixture::new();
    f.skill("source", "one");
    f.alias_bundle_storage();
    let config = f.write("config.toml", LOCAL);
    success(f.run(&config, &[]));
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
        assert!(!f.run(&config, &[]).status.success());
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
    success(f.run(&config, &[]));
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
        success(f.run(&config, &[]));
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
            let output = f.run(&config, &[]);
            assert!(!output.status.success(), "{kind}");
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("overlap"),
                "{kind}"
            );
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
                let output = f.run(&config, &[]);
                assert!(!output.status.success(), "{kind}, alias={aliased}");
                assert!(String::from_utf8_lossy(&output.stdout).contains("overlap"));
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
        success(f.run(&config, &[]));
        let paths = [
            "target/demo",
            "cache/uniskill/state.toml",
            "store/one",
            "store/one/skills/demo",
            "cache/uniskill/bundles",
        ];
        let before = paths.map(|p| snapshot(&f.path(p)));
        success(f.run(&config, &[]));
        assert_eq!(paths.map(|p| snapshot(&f.path(p))), before);
        f.write("config.toml", "");
        success(f.run(&config, &[]));
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
                success(f.run(&config, &[]));
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
                let output = f.run(&config, &[]);
                assert!(!output.status.success(), "{kind}, alias={aliased}");
                assert!(String::from_utf8_lossy(&output.stdout).contains("overlap"));
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
fn a_harness_cannot_replace_its_own_published_cache_entry() {
    for aliased in [false, true] {
        let f = Fixture::new();
        f.skill("source", "one");
        if aliased {
            f.alias_bundle_storage();
        }
        let config = f.write(
            "config.toml",
            LOCAL.replace("target/{name}", "cache/uniskill/bundles/one/skills/{name}"),
        );
        assert!(!f.run(&config, &[]).status.success());
        assert!(!f.path("cache/uniskill/bundles/one").exists());
        assert!(!f.path("cache/uniskill/state.toml").exists());
    }
}

#[test]
fn publication_preserves_legacy_harness_claims_inside_bundle_storage() {
    for aliased in [false, true] {
        for location in ["root", "descendant", "staging", "previous"] {
            for retained in [false, true] {
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
                success(f.run(&config, &[]));
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
                    assert!(!f.run(&config, &[]).status.success());
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
                    success(f.run(&config, &[]));
                    assert_eq!(
                        fs::read_to_string(f.path("target/demo/SKILL.md")).unwrap(),
                        "one"
                    );
                }
            }
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
            success(f.run(&config, &[]));
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
                assert!(!f.run(&config, &[]).status.success());
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
            success(f.run(&config, &[]));
            let installed = if dependency == "self" {
                "target/demo"
            } else {
                "second/extra"
            };
            let before = snapshot(&f.path(installed));
            success(f.run(&config, &[]));
            assert_eq!(snapshot(&f.path(installed)), before);
            assert_eq!(
                fs::read_to_string(f.path(installed).join("SKILL.md")).unwrap(),
                "one"
            );
        }
    }
}

#[test]
fn fresh_skill_routes_cannot_enter_cache_lifecycle_directories() {
    for aliased in [false, true] {
        for suffix in ["staging", "previous"] {
            let f = Fixture::new();
            f.skill("source", "one");
            if aliased {
                f.alias_bundle_storage();
            }
            let pattern = format!("cache/uniskill/bundles/.one.{suffix}/{{name}}");
            let config = f.write("config.toml", LOCAL.replace("target/{name}", &pattern));
            for _ in 0..2 {
                assert!(!f.run(&config, &[]).status.success());
                assert!(!f.path("cache/uniskill/bundles/one").exists());
                assert!(!f.path("cache/uniskill/state.toml").exists());
                assert!(!f.path(&pattern.replace("{name}", "demo")).is_symlink());
                assert_eq!(
                    fs::read_to_string(f.path("source/SKILL.md")).unwrap(),
                    "one"
                );
            }
            f.write("config.toml", LOCAL);
            success(f.run(&config, &[]));
        }
    }
}

#[test]
fn lifecycle_cleanup_preserves_installed_claims_even_when_their_source_fails() {
    for aliased in [false, true] {
        for suffix in ["staging", "previous"] {
            let f = Fixture::new();
            f.skill("source", "one");
            if aliased {
                f.alias_bundle_storage();
            }
            let config = f.write("config.toml", LOCAL);
            success(f.run(&config, &[]));
            let old = f.path(&format!("cache/uniskill/bundles/.one.{suffix}/demo"));
            fs::create_dir_all(old.parent().unwrap()).unwrap();
            fs::rename(f.path("target/demo"), &old).unwrap();
            let state = f.path("cache/uniskill/state.toml");
            let entry = old.parent().unwrap().canonicalize().unwrap().join("demo");
            let body = fs::read_to_string(&state).unwrap().replace(
                &format!("{:?}", f.path("target/demo").to_string_lossy()),
                &format!("{:?}", entry.to_string_lossy()),
            );
            fs::write(&state, body).unwrap();
            let before = (
                snapshot(&old),
                fs::read_link(&old).unwrap(),
                fs::read(&state).unwrap(),
            );
            for invalid in [false, true] {
                if invalid {
                    fs::remove_file(f.path("source/SKILL.md")).unwrap();
                }
                for _ in 0..2 {
                    assert!(!f.run(&config, &[]).status.success());
                    assert_eq!(
                        (
                            snapshot(&old),
                            fs::read_link(&old).unwrap(),
                            fs::read(&state).unwrap()
                        ),
                        before
                    );
                    assert!(!f.path("target/demo").is_symlink());
                }
            }
            f.write("config.toml", "");
            success(f.run(&config, &[]));
            assert!(!old.is_symlink());
            f.skill("source", "one");
            f.write("config.toml", LOCAL);
            success(f.run(&config, &[]));
            assert_eq!(
                fs::read_to_string(f.path("target/demo/SKILL.md")).unwrap(),
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
        success(f.run(&config, &[]));
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
            let output = f.run(&config, &[]);
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stdout).contains("overlap"));
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
    success(f.run(&config, &[]));
    let paths = ["target/demo", "mirror/demo", "cache/uniskill/state.toml"];
    let before = paths.map(|p| snapshot(&f.path(p)));
    success(f.run(&config, &[]));
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
    success(f.run(&config, &[]));
    let state = f.path("cache/uniskill/state.toml");
    let mut value: toml::Value = toml::from_str(&fs::read_to_string(&state).unwrap()).unwrap();
    let mut alias = value["links"][0].clone();
    alias["path"] = toml::Value::String(f.path("alias/demo").to_string_lossy().into_owned());
    alias["harness"] = toml::Value::String("alias".into());
    value["links"].as_array_mut().unwrap().push(alias);
    fs::write(&state, toml::to_string(&value).unwrap()).unwrap();
    let before = snapshot(&f.path("target/demo"));
    success(f.run(&config, &[]));
    assert_eq!(snapshot(&f.path("target/demo")), before);
    let upgraded = snapshot(&state);
    success(f.run(&config, &[]));
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
    success(f.run(&config, &[]));
    let before = snapshot(&f.path("target/demo"));
    fs::remove_dir_all(f.path("source")).unwrap();
    let two = "[bundles.two]\nharnesses=['fixture']\n[bundles.two.skills.demo]\nsource='other'\n";
    f.write("config.toml", format!("{one}\n{two}"));
    for _ in 0..2 {
        assert!(!f.run(&config, &[]).status.success());
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
    success(f.run(&config, &[]));
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
    success(f.run(&config, &[]));
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
        assert!(!f.run(&config, &[]).status.success());
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
fn overlapping_skill_roots_fail_before_source_or_ownership_changes() {
    let f = Fixture::new();
    f.skill("source", "one");
    f.skill("other", "two");
    let config = f.write("config.toml", LOCAL.to_owned() + "\n[harnesses.nested]\npattern='target/demo/{name}'\n[bundles.two]\nharnesses=['nested']\n[bundles.two.skills.extra]\nsource='other'\n");
    for _ in 0..2 {
        assert!(!f.run(&config, &[]).status.success());
        assert!(!f.path("target/demo").exists());
        assert!(!f.path("source/extra").exists());
        assert!(!f.path("cache/uniskill/bundles/one").exists());
        assert!(!f.path("cache/uniskill/state.toml").exists());
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
    success(f.run(&config, &[]));
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
        assert!(!f.run(&config, &[]).status.success());
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
fn retained_skill_roots_reject_nested_destinations_without_editing_sources() {
    let f = Fixture::new();
    f.skill("source", "one");
    f.skill("other", "two");
    let config = f.write("config.toml", LOCAL);
    success(f.run(&config, &[]));
    let before = (
        snapshot(&f.path("target/demo")),
        fs::read(f.path("cache/uniskill/state.toml")).unwrap(),
    );
    f.write("config.toml", LOCAL.replace("harnesses = [\"fixture\"]", "harnesses = ['missing']") + "\n[harnesses.nested]\npattern='target/demo/{name}'\n[bundles.two]\nharnesses=['nested']\n[bundles.two.skills.extra]\nsource='other'\n");
    for _ in 0..2 {
        assert!(!f.run(&config, &[]).status.success());
        assert_eq!(
            (
                snapshot(&f.path("target/demo")),
                fs::read(f.path("cache/uniskill/state.toml")).unwrap()
            ),
            before
        );
        assert!(!f.path("source/extra").exists());
        assert!(!f.path("cache/uniskill/bundles/two").exists());
    }
}

#[test]
fn a_bundle_cannot_move_its_destination_below_its_existing_skill_root() {
    let f = Fixture::new();
    f.skill("source", "one");
    let config = f.write("config.toml", LOCAL);
    success(f.run(&config, &[]));
    let before = (
        snapshot(&f.path("target/demo")),
        fs::read(f.path("cache/uniskill/state.toml")).unwrap(),
    );
    f.write(
        "config.toml",
        LOCAL.replace("target/{name}", "target/demo/{name}"),
    );
    assert!(!f.run(&config, &[]).status.success());
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
    success(f.run(&config, &[]));
    f.write(
        "config.toml",
        LOCAL.replace("skills.demo", "skills.moved")
            + "\n[bundles.two]\nharnesses=['fixture']\n[bundles.two.skills.demo]\nsource='other'\n",
    );
    assert!(!f.run(&config, &[]).status.success());
    assert!(!f.path("target/demo").is_symlink());
    assert_eq!(
        fs::read_to_string(f.path("target/moved/SKILL.md")).unwrap(),
        "one"
    );
    assert!(!f.path("cache/uniskill/bundles/two").exists());
    success(f.run(&config, &[]));
    assert_eq!(
        fs::read_to_string(f.path("target/demo/SKILL.md")).unwrap(),
        "two"
    );
    let before = (
        snapshot(&f.path("target/demo")),
        snapshot(&f.path("target/moved")),
        snapshot(&f.path("cache/uniskill/state.toml")),
    );
    success(f.run(&config, &[]));
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
fn custom_destinations_do_not_require_unused_builtin_environment_variables() {
    let f = Fixture::new();
    f.skill("source", "fixture");
    let config = f.write("config.toml", LOCAL);
    success(
        f.command(env!("CARGO_BIN_EXE_uniskill"))
            .arg("--config")
            .arg(config)
            .arg("sync")
            .env_remove("HOME")
            .output()
            .unwrap(),
    );
}

#[test]
fn oversized_bundle_names_fail_before_cache_publication() {
    let f = Fixture::new();
    f.skill("source", "fixture");
    let config = f.write(
        "config.toml",
        LOCAL.replace("bundles.one", &format!("bundles.{}", "a".repeat(256))),
    );
    assert!(!f.run(&config, &[]).status.success());
    assert!(!f.path("target/demo").exists());
    assert!(!f.path("cache/uniskill/bundles").exists());
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
        assert!(!f.run(&config, &[]).status.success(), "{kind}");
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

fn linked_project(f: &Fixture, text: &str) -> PathBuf {
    let main = f.repo("main");
    f.write("main/tracked", "fixture");
    success(f.git(&main, &["add", "."]));
    success(f.git(&main, &["commit", "-qm", "fixture"]));
    f.skill("main/source", "one");
    f.write("main/uniskill.toml", text);
    success(
        f.command(env!("CARGO_BIN_EXE_uniskill"))
            .arg("sync")
            .current_dir(&main)
            .output()
            .unwrap(),
    );
    let linked = f.path("linked");
    success(f.git(&main, &["worktree", "add", "-q", linked.to_str().unwrap()]));
    linked
}

#[test]
fn worktree_failed_bundles_keep_their_destinations_reserved() {
    let f = Fixture::new();
    f.skill("main/other", "two");
    let initial = format!("{LOCAL}\n[harnesses.second]\npattern='second/{{name}}'\n[bundles.two]\nharnesses=['second']\n[bundles.two.skills.demo]\nsource='other'\n");
    let linked = linked_project(&f, &initial);
    let run = || {
        f.command(env!("CARGO_BIN_EXE_uniskill"))
            .args(["sync", "--worktree"])
            .current_dir(&linked)
            .output()
            .unwrap()
    };
    success(run());
    let link = linked.join("target/demo");
    let state = linked.join(".uniskill-cache/state.toml");
    let before = (
        snapshot(&link),
        fs::read_link(&link).unwrap(),
        fs::read(&state).unwrap(),
    );
    fs::rename(
        f.path("main/.uniskill-cache/bundles/one"),
        f.path("displaced"),
    )
    .unwrap();
    f.write(
        "main/uniskill.toml",
        initial.replace("harnesses=['second']", "harnesses=['fixture']"),
    );
    for _ in 0..2 {
        assert!(!run().status.success());
        assert_eq!(
            (
                snapshot(&link),
                fs::read_link(&link).unwrap(),
                fs::read(&state).unwrap()
            ),
            before
        );
        assert_eq!(
            fs::read_to_string(linked.join("second/demo/SKILL.md")).unwrap(),
            "two"
        );
    }
    fs::rename(
        f.path("displaced"),
        f.path("main/.uniskill-cache/bundles/one"),
    )
    .unwrap();
    f.write("main/uniskill.toml", initial);
    success(run());
    assert_eq!(
        (
            snapshot(&link),
            fs::read_link(&link).unwrap(),
            fs::read(&state).unwrap()
        ),
        before
    );
}

#[test]
fn worktree_recovery_preserves_shared_main_ownership_until_it_retires() {
    for aliased in [false, true] {
        let f = Fixture::new();
        f.skill("main/other", "two");
        let two = "[harnesses.second]\npattern='retired/{name}'\n[bundles.two]\nharnesses=['second']\n[bundles.two.skills.old]\nsource='other'\n";
        let initial = format!("{LOCAL}\n{two}");
        let linked = linked_project(&f, &initial);
        if aliased {
            fs::rename(f.path("main/.uniskill-cache/bundles"), f.path("store")).unwrap();
            std::os::unix::fs::symlink(f.path("store"), f.path("main/.uniskill-cache/bundles"))
                .unwrap();
        }
        let old = f.path("main/.uniskill-cache/bundles/.one.previous/old");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::rename(f.path("main/retired/old"), &old).unwrap();
        let state = f.path("main/.uniskill-cache/state.toml");
        let entry = old.parent().unwrap().canonicalize().unwrap().join("old");
        let bytes = fs::read_to_string(&state).unwrap().replace(
            &format!("{:?}", f.path("main/retired/old").to_string_lossy()),
            &format!("{:?}", entry.to_string_lossy()),
        );
        let mut records: Vec<_> = bytes.trim().split("\n\n").collect();
        records.sort();
        let bytes = records.join("\n\n") + "\n";
        fs::write(&state, &bytes).unwrap();
        let before = (
            snapshot(&old),
            fs::read_link(&old).unwrap(),
            fs::read(&state).unwrap(),
        );
        f.write(
            "main/uniskill.toml",
            initial.replace("harnesses=['second']", "harnesses=['missing']"),
        );
        let run = |worktree: bool| {
            let mut cmd = f.command(env!("CARGO_BIN_EXE_uniskill"));
            cmd.arg("sync");
            if worktree {
                cmd.arg("--worktree");
            }
            cmd.current_dir(if worktree {
                linked.clone()
            } else {
                f.path("main")
            })
            .output()
            .unwrap()
        };
        for _ in 0..2 {
            assert!(!run(false).status.success());
            assert!(!run(true).status.success());
            assert!(
                old.is_symlink(),
                "worktree recovery removed a retained main link"
            );
            assert_eq!(
                (
                    snapshot(&old),
                    fs::read_link(&old).unwrap(),
                    fs::read(&state).unwrap()
                ),
                before
            );
            assert!(!linked.join("target/demo").is_symlink());
        }
        fs::write(&state, "invalid ownership").unwrap();
        assert!(!run(true).status.success());
        assert_eq!(
            (snapshot(&old), fs::read_link(&old).unwrap()),
            (before.0, before.1.clone())
        );
        assert_eq!(fs::read_to_string(&state).unwrap(), "invalid ownership");
        fs::write(&state, bytes).unwrap();
        f.write("main/uniskill.toml", LOCAL);
        assert!(!run(false).status.success());
        assert!(!old.is_symlink());
        let main_before = fs::read(&state).unwrap();
        success(run(true));
        success(run(true));
        assert_eq!(fs::read(&state).unwrap(), main_before);
        assert_eq!(
            fs::read_to_string(linked.join("target/demo/SKILL.md")).unwrap(),
            "one"
        );
        success(run(false));
        assert!(linked.join("target/demo").is_symlink());
    }
}

#[test]
fn worktrees_consume_and_prune_links_from_aliased_bundle_storage() {
    let f = Fixture::new();
    let linked = linked_project(&f, LOCAL);
    fs::rename(f.path("main/.uniskill-cache/bundles"), f.path("store")).unwrap();
    std::os::unix::fs::symlink(f.path("store"), f.path("main/.uniskill-cache/bundles")).unwrap();
    let main_before = (
        snapshot(&f.path("main/target/demo")),
        fs::read(f.path("main/.uniskill-cache/state.toml")).unwrap(),
    );
    let run = || {
        f.command(env!("CARGO_BIN_EXE_uniskill"))
            .args(["sync", "--worktree"])
            .current_dir(&linked)
            .output()
            .unwrap()
    };
    success(run());
    let before = (
        snapshot(&linked.join("target/demo")),
        snapshot(&linked.join(".uniskill-cache/state.toml")),
    );
    success(run());
    assert_eq!(
        (
            snapshot(&linked.join("target/demo")),
            snapshot(&linked.join(".uniskill-cache/state.toml"))
        ),
        before
    );
    f.write("main/uniskill.toml", "");
    success(run());
    assert!(!linked.join("target/demo").is_symlink());
    assert_eq!(
        (
            snapshot(&f.path("main/target/demo")),
            fs::read(f.path("main/.uniskill-cache/state.toml")).unwrap()
        ),
        main_before
    );
    assert_eq!(
        fs::read_to_string(f.path("main/source/SKILL.md")).unwrap(),
        "one"
    );
}

#[test]
fn worktree_skill_routes_cannot_replace_state_storage_aliases() {
    let f = Fixture::new();
    f.skill("main/other", "two");
    let initial = format!("{LOCAL}\n[harnesses.second]\npattern='second/{{name}}'\n[bundles.two]\nharnesses=['second']\n[bundles.two.skills.state]\nsource='other'\n");
    let linked = linked_project(&f, &initial);
    fs::create_dir(linked.join("ownership")).unwrap();
    std::os::unix::fs::symlink("ownership", linked.join("state")).unwrap();
    std::os::unix::fs::symlink("state", linked.join(".uniskill-cache")).unwrap();
    let run = || {
        f.command(env!("CARGO_BIN_EXE_uniskill"))
            .args(["sync", "--worktree"])
            .current_dir(&linked)
            .output()
            .unwrap()
    };
    success(run());
    let paths = [
        ".uniskill-cache",
        "state",
        "ownership/state.toml",
        "target/demo",
        "second/state",
    ];
    let before = paths.map(|p| (snapshot(&linked.join(p)), fs::read(linked.join(p)).ok()));
    f.write(
        "main/uniskill.toml",
        initial.replace("second/{name}", "{name}"),
    );
    for _ in 0..2 {
        assert!(!run().status.success());
        assert_eq!(
            paths.map(|p| (snapshot(&linked.join(p)), fs::read(linked.join(p)).ok())),
            before
        );
        assert_eq!(
            fs::read_link(linked.join("state")).unwrap(),
            Path::new("ownership")
        );
    }
    f.write("main/uniskill.toml", "");
    success(run());
    assert!(!linked.join("target/demo").is_symlink());
    assert!(!linked.join("second/state").is_symlink());
    assert!(linked.join("state").is_symlink());
}

#[test]
fn worktree_aliases_cannot_enter_an_installed_skill_inside_the_checkout() {
    let f = Fixture::new();
    f.skill("main/other", "two");
    let initial = format!("{LOCAL}\n[harnesses.second]\npattern='second/{{name}}'\n[bundles.two]\nharnesses=['second']\n[bundles.two.skills.extra]\nsource='other'\n");
    let linked = linked_project(&f, &initial);
    f.skill("linked/source", "one");
    let inside = initial.replace(
        "source = \"source\"",
        &format!("source = {:?}", linked.join("source").to_string_lossy()),
    );
    f.write("main/uniskill.toml", &inside);
    success(
        f.command(env!("CARGO_BIN_EXE_uniskill"))
            .arg("sync")
            .current_dir(f.path("main"))
            .output()
            .unwrap(),
    );
    let run = || {
        f.command(env!("CARGO_BIN_EXE_uniskill"))
            .args(["sync", "--worktree"])
            .current_dir(&linked)
            .output()
            .unwrap()
    };
    success(run());
    std::os::unix::fs::symlink(linked.join("target/demo"), linked.join("alias")).unwrap();
    let paths = ["target/demo", "second/extra", ".uniskill-cache/state.toml"];
    let before = paths.map(|p| (snapshot(&linked.join(p)), fs::read(linked.join(p)).ok()));
    f.write(
        "main/uniskill.toml",
        inside.replace("pattern='second/{name}'", "pattern='alias/{name}'"),
    );
    for _ in 0..2 {
        let output = run();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("overlap"));
        assert_eq!(
            paths.map(|p| (snapshot(&linked.join(p)), fs::read(linked.join(p)).ok())),
            before
        );
        assert!(!linked.join("source/extra").exists());
        assert_eq!(
            fs::read_to_string(linked.join("source/SKILL.md")).unwrap(),
            "one"
        );
    }
}

#[test]
fn worktree_destinations_and_state_stay_inside_the_checkout_after_alias_resolution() {
    for kind in [
        "skill",
        "mcp",
        "state",
        "mcp-state",
        "old-ownership",
        "mcp-old-ownership",
        "mcp-journal",
        "mcp-restored-ownership",
    ] {
        let f = Fixture::new();
        let text = if kind == "mcp" {
            "[harnesses.fixture.mcp]\npath='target/native.json'\nformat='json'\ntable=['servers']\n[mcp.docs]\nharnesses=['fixture']\nurl='https://example.invalid/mcp'\n"
        } else {
            LOCAL
        };
        let linked = linked_project(&f, text);
        let outside = f.path("outside");
        fs::create_dir(&outside).unwrap();
        let foreign = outside.join("demo");
        std::os::unix::fs::symlink(f.path("manual"), &foreign).unwrap();
        f.write("outside/native.json", "{}");
        let before = snapshot(&foreign);
        match kind {
            "skill" | "mcp" => std::os::unix::fs::symlink(&outside, linked.join("target")).unwrap(),
            "state" => {
                std::os::unix::fs::symlink(&outside, linked.join(".uniskill-cache")).unwrap()
            }
            "mcp-state" => {
                fs::create_dir(linked.join(".uniskill-cache")).unwrap();
                std::os::unix::fs::symlink(&outside, linked.join(".uniskill-cache/mcp")).unwrap();
            }
            "mcp-old-ownership" | "mcp-journal" | "mcp-restored-ownership" => {
                std::os::unix::fs::symlink(&outside, linked.join("target")).unwrap();
                f.write("main/uniskill.toml", "");
                let native = linked.join("target/native.json");
                let owned = serde_json::json!({"version":1,"entries":[{
                    "identity":{"path":native,"table":["servers"],"name":"docs"},
                    "format":"json","digest":"0".repeat(64)
                }]})
                .to_string();
                let empty = serde_json::json!({"version":1,"entries":[]}).to_string();
                let state = f.write(
                    "linked/.uniskill-cache/mcp/state.json",
                    if kind == "mcp-old-ownership" {
                        &owned
                    } else {
                        &empty
                    },
                );
                if kind != "mcp-old-ownership" {
                    let update = if kind == "mcp-journal" {
                        serde_json::json!({"path":native,"before":null,"after":"{}","mode":384})
                    } else {
                        serde_json::json!({"path":state,"before":owned,"after":empty,"mode":384})
                    };
                    f.write(
                        "linked/.uniskill-cache/mcp/journal.json",
                        serde_json::json!({"version":1,"updates":[update]}).to_string(),
                    );
                }
            }
            _ => {
                fs::remove_file(&foreign).unwrap();
                std::os::unix::fs::symlink(
                    f.path("main/.uniskill-cache/bundles/one/skills/demo"),
                    &foreign,
                )
                .unwrap();
                f.write(
                    "linked/.uniskill-cache/state.toml",
                    format!(
                        "[[links]]\npath={:?}\nskill='demo'\nharness='fixture'\nbundle='one'\n",
                        foreign.to_string_lossy()
                    ),
                );
                f.write("main/uniskill.toml", "");
            }
        }
        let original = fs::read(linked.join(".uniskill-cache/state.toml")).ok();
        let mcp_state = || {
            ["state.json", "journal.json"]
                .map(|name| fs::read(linked.join(".uniskill-cache/mcp").join(name)).ok())
        };
        let original_mcp = mcp_state();
        let outside_entries = || {
            let mut entries: Vec<_> = fs::read_dir(&outside)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            entries.sort();
            entries
        };
        let original_entries = outside_entries();
        let foreign_target = fs::read_link(&foreign).unwrap();
        let output = f
            .command(env!("CARGO_BIN_EXE_uniskill"))
            .args(["sync", "--worktree"])
            .current_dir(&linked)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{kind}");
        assert_eq!(fs::read_link(&foreign).unwrap(), foreign_target, "{kind}");
        if kind != "old-ownership" {
            assert_eq!(snapshot(&foreign), before, "{kind}");
        }
        assert_eq!(
            fs::read_to_string(outside.join("native.json")).unwrap(),
            "{}"
        );
        assert_eq!(
            fs::read(linked.join(".uniskill-cache/state.toml")).ok(),
            original,
            "{kind}"
        );
        assert!(!outside.join("state.toml").exists());
        assert!(!outside.join("mcp").exists());
        assert_eq!(outside_entries(), original_entries, "{kind}");
        assert_eq!(mcp_state(), original_mcp, "{kind}");
    }
}

#[test]
fn default_git_ref_and_branch_named_head_install_distinct_contents() {
    for jobs in ["1", "4"] {
        let f = Fixture::new();
        let origin = f.repo("origin");
        f.skill("origin", "default branch");
        success(f.git(&origin, &["add", "."]));
        success(f.git(&origin, &["commit", "-qm", "fixture"]));
        success(f.git(&origin, &["checkout", "-qb", "head"]));
        f.skill("origin", "explicit head branch");
        success(f.git(&origin, &["commit", "-qam", "head fixture"]));
        success(f.git(&origin, &["checkout", "-q", "main"]));
        let config = f.write(
            "config.toml",
            r#"
[harnesses.fixture]
pattern = "target/{name}"
[bundles.one]
harnesses = ["fixture"]
[bundles.one.skills.default]
repo = "./origin"
[bundles.one.skills.explicit]
repo = "./origin"
ref = "head"
"#,
        );
        success(f.run(&config, &["--jobs", jobs]));
        assert_eq!(
            fs::read_to_string(f.path("target/default/SKILL.md")).unwrap(),
            "default branch"
        );
        assert_eq!(
            fs::read_to_string(f.path("target/explicit/SKILL.md")).unwrap(),
            "explicit head branch"
        );
    }
}

#[test]
fn competing_skill_routes_fail_without_replacing_the_installed_link() {
    let f = Fixture::new();
    f.skill("source", "one");
    f.skill("other", "two");
    let local = LOCAL.replace("[bundles.one]", "[bundles.one]\nlink = false");
    let config = f.write("config.toml", &local);
    success(f.run(&config, &[]));
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
        assert!(!f.run(&config, &[]).status.success());
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
    success(f.run(&config, &[]));
    let state = f.path("cache/uniskill/state.toml");
    let value: toml::Value = toml::from_str(&fs::read_to_string(&state).unwrap()).unwrap();
    assert_eq!(value["links"].as_array().unwrap().len(), 1);
    let before = (snapshot(&f.path("target/demo")), snapshot(&state));
    success(f.run(&config, &[]));
    assert_eq!((snapshot(&f.path("target/demo")), snapshot(&state)), before);
}

#[test]
fn skill_patterns_require_a_name_placeholder_before_installing() {
    let f = Fixture::new();
    f.skill("source", "one");
    let config = f.write(
        "config.toml",
        LOCAL.replace("target/{name}", "target/static"),
    );
    assert!(!f.run(&config, &[]).status.success());
    assert!(!f.path("target/static").exists());
}

#[test]
fn unavailable_ownership_state_fails_and_retry_can_install_then_prune() {
    let f = Fixture::new();
    f.skill("source", "one");
    let config = f.write("config.toml", LOCAL);
    let state = f.path("cache/uniskill/state.toml");
    fs::create_dir_all(&state).unwrap();
    assert!(!f.run(&config, &[]).status.success());
    fs::remove_dir(&state).unwrap();
    success(f.run(&config, &[]));
    f.write("config.toml", "");
    success(f.run(&config, &[]));
    assert!(fs::symlink_metadata(f.path("target/demo")).is_err());
}

#[test]
fn corrupt_ownership_state_is_preserved_and_fails_before_mutation() {
    let f = Fixture::new();
    f.skill("source", "one");
    let config = f.write("config.toml", LOCAL);
    success(f.run(&config, &[]));
    let link = f.path("target/demo");
    let before = snapshot(&link);
    let state = f.write("cache/uniskill/state.toml", "invalid [[");
    assert!(!f.run(&config, &[]).status.success());
    assert_eq!(snapshot(&link), before);
    assert_eq!(fs::read_to_string(state).unwrap(), "invalid [[");
}

#[test]
fn failed_prune_retains_ownership_and_retries_after_permissions_recover() {
    let f = Fixture::new();
    f.skill("source", "one");
    let config = f.write("config.toml", LOCAL);
    success(f.run(&config, &[]));
    f.write("config.toml", "");
    let parent = f.path("target");
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
    let output = f.run(&config, &[]);
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!output.status.success());
    assert!(f.path("target/demo").is_symlink());
    let state = fs::read_to_string(f.path("cache/uniskill/state.toml")).unwrap();
    assert!(state.contains("demo"));
    success(f.run(&config, &[]));
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
    success(f.run(&config, &[]));
    let cache = f.path("cache/uniskill/bundles");
    fs::rename(cache.join("one"), cache.join(".one.previous")).unwrap();
    fs::remove_dir_all(f.path("source")).unwrap();
    assert!(!f.run(&config, &[]).status.success());
    assert_eq!(
        fs::read_to_string(f.path("target/demo/SKILL.md")).unwrap(),
        "one"
    );
    assert!(!cache.join(".one.previous").exists());
}

#[test]
fn declaring_directories_with_literal_variables_are_never_expanded_again() {
    for whole in [false, true] {
        let f = Fixture::new();
        let base = "literal-$UNISKILL_FIXTURE_TOKEN";
        f.skill(&format!("{base}/source"), "explicit");
        f.skill(&format!("{base}/bundle/skills/demo"), "whole");
        let text = if whole {
            "[harnesses.fixture]\npattern = 'target/{name}'\n[bundles.one]\nharnesses = ['fixture']\nsource = 'bundle'\n"
        } else {
            LOCAL
        };
        let config = f.write(&format!("{base}/config.toml"), format!("{text}\n[harnesses.fixture.mcp]\npath = 'native.json'\nformat = 'json'\ntable = ['servers']\n[mcp.docs]\nharnesses = ['fixture']\nurl = 'https://example.invalid/mcp'\n"));
        success(f.run(&config, &[]));
        assert!(f.path(&format!("{base}/native.json")).exists());
        assert_eq!(
            fs::read_to_string(f.path(&format!("{base}/target/demo/SKILL.md"))).unwrap(),
            if whole { "whole" } else { "explicit" }
        );
    }
}

#[test]
fn symlinked_configs_resolve_all_relative_paths_at_the_declaring_file() {
    let f = Fixture::new();
    f.skill("real/source", "fixture");
    let real = f.write("real/config.toml", LOCAL);
    let link = f.path("config.toml");
    std::os::unix::fs::symlink(real, &link).unwrap();
    success(f.run(&link, &[]));
    assert_eq!(
        fs::read_to_string(f.path("real/target/demo/SKILL.md")).unwrap(),
        "fixture"
    );
    assert!(!f.path("target").exists());
}

#[test]
fn native_path_variable_values_are_literal_after_expansion() {
    let f = Fixture::new();
    let config = f.write(
        "config.toml",
        r#"
[harnesses.fixture.mcp]
path = '${UNISKILL_FIXTURE_DIR}/native.json'
format = 'json'
table = ['servers']
[mcp.docs]
harnesses = ['fixture']
url = 'https://example.invalid/mcp'
"#,
    );
    success(f.run(&config, &[]));
    assert!(f
        .path("literal-$UNISKILL_FIXTURE_TOKEN/native.json")
        .is_file());
    assert!(!f.path("literal-expanded/native.json").exists());
}

#[test]
fn worktree_configuration_failures_preserve_installed_links_and_state() {
    for invalid in ["unknown", "mcp-only", "missing-cache"] {
        let f = Fixture::new();
        let main = f.repo("main");
        f.write("main/tracked", "fixture");
        success(f.git(&main, &["add", "."]));
        success(f.git(&main, &["commit", "-qm", "fixture"]));
        f.skill("main/source", "one");
        f.write("main/uniskill.toml", LOCAL);
        success(
            f.command(env!("CARGO_BIN_EXE_uniskill"))
                .arg("sync")
                .current_dir(&main)
                .output()
                .unwrap(),
        );
        success(f.git(
            &main,
            &["worktree", "add", "-q", f.path("linked").to_str().unwrap()],
        ));
        let run = || {
            f.command(env!("CARGO_BIN_EXE_uniskill"))
                .args(["sync", "--worktree"])
                .current_dir(f.path("linked"))
                .output()
                .unwrap()
        };
        success(run());
        let link = f.path("linked/target/demo");
        let state = f.path("linked/.uniskill-cache/state.toml");
        let before = (snapshot(&link), fs::read(&state).unwrap());
        match invalid {
            "unknown" => {
                f.write(
                    "main/uniskill.toml",
                    LOCAL.replace("harnesses = [\"fixture\"]", "harnesses = [\"unknown\"]"),
                );
            }
            "mcp-only" => {
                f.write(
                    "main/uniskill.toml",
                    LOCAL.replace(
                        "pattern = \"target/{name}\"",
                        "mcp = { path = 'native.json', format = 'json', table = ['servers'] }",
                    ),
                );
            }
            _ => {
                fs::rename(
                    f.path("main/.uniskill-cache/bundles/one"),
                    f.path("displaced"),
                )
                .unwrap();
            }
        }
        assert!(!run().status.success());
        assert_eq!((snapshot(&link), fs::read(&state).unwrap()), before);
    }
}

#[test]
fn foreign_hook_with_non_utf8_bytes_is_preserved() {
    let f = Fixture::new();
    let repo = f.repo("repo");
    let bytes = b"#!/bin/sh\n# comment \xff\nexit 0\n";
    let hook = f.write("repo/.git/hooks/post-checkout", bytes);
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o751)).unwrap();
    let before = snapshot(&hook);
    success(
        f.command(env!("CARGO_BIN_EXE_uniskill"))
            .args(["hook", "install"])
            .current_dir(repo)
            .output()
            .unwrap(),
    );
    assert_eq!(fs::read(&hook).unwrap(), bytes);
    assert_eq!(snapshot(&hook), before);
    assert_eq!(
        fs::metadata(hook).unwrap().permissions().mode() & 0o777,
        0o751
    );
}

#[test]
fn global_hook_installation_preserves_reference_transaction_validation() {
    let f = Fixture::new();
    let repo = f.repo("repo");
    f.write("repo/tracked", "fixture");
    success(f.git(&repo, &["add", "."]));
    success(f.git(&repo, &["commit", "-qm", "fixture"]));
    let hook = f.write(
        "repo/.git/hooks/reference-transaction",
        b"#!/bin/sh\ncat >> validation.log\n[ \"$1\" != prepared ]\n",
    );
    fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!f
        .git(&repo, &["update-ref", "refs/heads/check", "HEAD"])
        .status
        .success());
    success(
        f.command(env!("CARGO_BIN_EXE_uniskill"))
            .args(["hook", "install", "--global"])
            .current_dir(&repo)
            .output()
            .unwrap(),
    );
    fs::remove_file(repo.join("validation.log")).unwrap();
    assert!(!f
        .git(&repo, &["update-ref", "refs/heads/check", "HEAD"])
        .status
        .success());
    assert!(!fs::read_to_string(repo.join("validation.log"))
        .unwrap()
        .is_empty());
}

#[test]
fn native_mcp_targets_cannot_mutate_skill_storage_or_traverse_its_aliases() {
    for location in [
        "state.toml",
        "bundles/.one.previous/native.toml",
        "bundles/alias/native.toml",
    ] {
        let f = Fixture::new();
        f.skill("source", "one");
        let config = f.write("config.toml", LOCAL);
        success(f.run(&config, &[]));
        if location.contains("alias") {
            fs::create_dir(f.path("external")).unwrap();
            std::os::unix::fs::symlink(f.path("external"), f.path("cache/uniskill/bundles/alias"))
                .unwrap();
        }
        let target = format!("cache/uniskill/{location}");
        f.write("config.toml", format!("{LOCAL}\n[harnesses.native.mcp]\npath='{target}'\nformat='toml'\ntable=['servers']\n[mcp.docs]\nharnesses=['native']\ncommand='echo'\n"));
        let protected = [f.path("cache/uniskill/state.toml"), f.path("target/demo")];
        let before = protected
            .each_ref()
            .map(|p| (snapshot(p), fs::read(p).ok()));
        for args in [&["--check-mcp"][..], &["--mcp-only"][..], &[][..], &[][..]] {
            let output = f.run(&config, args);
            assert!(!output.status.success(), "{location}");
            assert!(String::from_utf8_lossy(&output.stderr).contains("reserved filesystem storage"));
            assert_eq!(
                protected
                    .each_ref()
                    .map(|p| (snapshot(p), fs::read(p).ok())),
                before
            );
            assert!(!f.path("cache/uniskill/mcp/state.json").exists());
            if location != "state.toml" {
                assert!(!f.path(&target).exists());
            }
        }
    }
}

#[test]
fn inactive_declared_mcp_targets_stay_protected_before_cache_publication() {
    let f = Fixture::new();
    f.skill("source", "one");
    let local = LOCAL.replace("[bundles.one]", "[bundles.one]\nlink=false");
    let config = f.write("config.toml", &local);
    success(f.run(&config, &[]));
    let native = f.write(
        "cache/uniskill/bundles/one/native.json",
        "{\"userSetting\":true}",
    );
    f.write("config.toml", format!("{local}\n[harnesses.native.mcp]\npath='cache/uniskill/bundles/one/native.json'\nformat='json'\ntable=['servers']\n"));
    let before = (
        fs::read(&native).unwrap(),
        snapshot(&native),
        snapshot(&f.path("target/demo")),
    );
    for _ in 0..2 {
        assert!(!f.run(&config, &[]).status.success());
        assert_eq!(
            (
                fs::read(&native).unwrap(),
                snapshot(&native),
                snapshot(&f.path("target/demo"))
            ),
            before
        );
    }
}

#[test]
fn worktree_global_mcp_targets_are_checked_before_shared_cache_recovery() {
    let f = Fixture::new();
    let linked = linked_project(&f, LOCAL);
    let native = f.write(
        "main/.uniskill-cache/bundles/.one.previous/native.json",
        "{\"userSetting\":true}",
    );
    let target = native.to_string_lossy();
    f.write("main/uniskill.toml", format!("{LOCAL}\n[harnesses.native.mcp]\npath='{target}'\nformat='json'\ntable=['servers']\n[mcp.docs]\nharnesses=['native']\ncommand='echo'\n"));
    // Global means outside both checkouts, while still part of shared storage.
    // Alias the shared cache outside the main checkout to expose that boundary.
    fs::rename(f.path("main/.uniskill-cache"), f.path("shared-cache")).unwrap();
    std::os::unix::fs::symlink(f.path("shared-cache"), f.path("main/.uniskill-cache")).unwrap();
    let native = f.path("shared-cache/bundles/.one.previous/native.json");
    let before = (fs::read(&native).unwrap(), snapshot(&native));
    for _ in 0..2 {
        let output = f
            .command(env!("CARGO_BIN_EXE_uniskill"))
            .args(["sync", "--worktree"])
            .current_dir(&linked)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!((fs::read(&native).unwrap(), snapshot(&native)), before);
        assert!(!linked.join(".uniskill-cache/mcp/state.json").exists());
    }
}

#[test]
fn legacy_and_recovery_native_files_are_checked_before_any_storage_mutation() {
    for journal in [false, true] {
        let f = Fixture::new();
        f.skill("source", "one");
        let config = f.write("config.toml", LOCAL);
        success(f.run(&config, &[]));
        let native = f.write(
            "cache/uniskill/bundles/.one.previous/native.json",
            "{\"userSetting\":true}",
        );
        let owned = serde_json::json!({"version":1,"entries":[{
            "identity":{"path":native,"table":["servers"],"name":"docs"},
            "format":"json","digest":"0".repeat(64)
        }]})
        .to_string();
        let empty = r#"{"version":1,"entries":[]}"#;
        let state = f.write(
            "cache/uniskill/mcp/state.json",
            if journal { empty } else { &owned },
        );
        let record = f.path("cache/uniskill/mcp/journal.json");
        if journal {
            fs::write(
                &record,
                serde_json::json!({"version":1,"updates":[{
                    "path":state,"before":empty,"after":owned,"mode":384
                }]})
                .to_string(),
            )
            .unwrap();
        }
        let protected = [native, state, f.path("cache/uniskill/state.toml")];
        let before = protected
            .each_ref()
            .map(|p| (fs::read(p).unwrap(), snapshot(p)));
        let journal_bytes = fs::read(&record).ok();
        for _ in 0..2 {
            assert!(!f.run(&config, &[]).status.success());
            assert_eq!(
                protected
                    .each_ref()
                    .map(|p| (fs::read(p).unwrap(), snapshot(p))),
                before
            );
            assert_eq!(fs::read(&record).ok(), journal_bytes);
        }
    }
}
