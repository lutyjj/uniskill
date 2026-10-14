use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        let f = Self(tempfile::tempdir().unwrap());
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
