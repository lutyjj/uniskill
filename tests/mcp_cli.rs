mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};

struct Fixture {
    temp: tempfile::TempDir,
    config: PathBuf,
}

impl Fixture {
    fn new(text: &str) -> Self {
        let temp = support::tempdir().unwrap();
        fs::create_dir(temp.path().join("home")).unwrap();
        let config = temp.path().join("config.toml");
        fs::write(&config, text).unwrap();
        Self { temp, config }
    }

    fn path(&self, path: &str) -> PathBuf {
        self.temp.path().join(path)
    }

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.path("gitconfig"))
            .current_dir(self.temp.path());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_uniskill"))
            .args(["--config", self.config.to_str().unwrap(), "sync"])
            .args(args)
            .output()
            .unwrap()
    }

    fn write(&self, path: &str, data: &[u8]) {
        if let Some(parent) = self.path(path).parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(self.path(path), data).unwrap();
    }

    fn json(&self, path: &str) -> Value {
        serde_json::from_slice(&fs::read(self.path(path)).unwrap()).unwrap()
    }
}

fn succeeded(output: Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

const JSON_TARGET: &str = r#"
[harnesses.fixture.mcp]
path = "native.json"
format = "json"
table = ["tools", "servers"]
[mcp.docs]
harnesses = ["fixture"]
url = "https://example.com/mcp"
"#;

#[test]
fn check_is_read_only_and_reports_pending_native_changes() {
    let f = Fixture::new(JSON_TARGET);
    assert!(!f.run(&["--check-mcp"]).status.success());
    assert!(!f.path("native.json").exists());
    assert!(!f.path("cache").exists());
}

#[test]
fn malformed_file_or_one_conflict_prevents_the_entire_mcp_transaction() {
    let text = format!("{JSON_TARGET}\n[harnesses.second.mcp]\npath=\"second.toml\"\nformat=\"toml\"\ntable=[\"mcp_servers\"]\nomit=[\"type\"]\n[mcp.other]\nharnesses=[\"second\"]\ncommand=\"fixture\"\n");
    let f = Fixture::new(&text);
    f.write("second.toml", b"secret = 'DO_NOT_PRINT'\nmalformed = [");
    let output = f.run(&[]);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("DO_NOT_PRINT"));
    assert!(!f.path("native.json").exists());
    f.write("second.toml", b"[mcp_servers.other]\ncommand='unmanaged'\n");
    assert!(!f.run(&[]).status.success());
    assert!(!f.path("native.json").exists());
}

fn git(path: &Path, args: &[&str]) {
    assert!(Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap()
        .status
        .success());
}

#[test]
fn worktree_sync_retargets_project_mcp_and_keeps_global_files_and_ownership_separate() {
    let f = Fixture::new(JSON_TARGET);
    let main = f.path("repo");
    let linked = f.path("linked");
    fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q"]);
    git(
        &main,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "commit",
            "--allow-empty",
            "-qm",
            "fixture",
        ],
    );
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().unwrap(),
            "-b",
            "fixture",
        ],
    );
    fs::write(
        &f.config,
        format!(
            r#"
[harnesses.project.mcp]
path = '{}'
format = 'json'
table = ['mcpServers']
[harnesses.global.mcp]
path = '{}'
format = 'json'
table = ['mcpServers']
[mcp.docs]
harnesses = ['project', 'global']
url = 'https://example.com/mcp'
"#,
            main.join(".mcp.json").display(),
            f.path("global.json").display()
        ),
    )
    .unwrap();
    succeeded(f.run(&[]));
    let global = fs::read(f.path("global.json")).unwrap();
    let output = f
        .command(env!("CARGO_BIN_EXE_uniskill"))
        .args(["--config", f.config.to_str().unwrap(), "sync", "--worktree"])
        .current_dir(&linked)
        .env("XDG_CACHE_HOME", f.path("cache"))
        .output()
        .unwrap();
    succeeded(output);
    assert!(linked.join(".mcp.json").exists());
    assert_eq!(fs::read(f.path("global.json")).unwrap(), global);
    fs::write(&f.config, "").unwrap();
    succeeded(f.run(&[]));
    let worktree: Value =
        serde_json::from_slice(&fs::read(linked.join(".mcp.json")).unwrap()).unwrap();
    assert!(worktree["mcpServers"]["docs"].is_object());
}

#[test]
fn repeated_apply_preserves_skills_native_configs_and_ownership_metadata() {
    use std::collections::BTreeMap;
    use std::os::unix::fs::MetadataExt;

    fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u64, i64, i64, Vec<u8>)> {
        fn visit(path: &Path, result: &mut BTreeMap<PathBuf, (u64, i64, i64, Vec<u8>)>) {
            let meta = fs::symlink_metadata(path).unwrap();
            let content = if meta.file_type().is_symlink() {
                fs::read_link(path)
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec()
            } else if meta.is_file() {
                fs::read(path).unwrap()
            } else {
                Vec::new()
            };
            result.insert(
                path.to_path_buf(),
                (meta.ino(), meta.mtime(), meta.mtime_nsec(), content),
            );
            if meta.is_dir() {
                for child in fs::read_dir(path).unwrap() {
                    visit(&child.unwrap().path(), result);
                }
            }
        }
        let mut result = BTreeMap::new();
        visit(root, &mut result);
        result
    }

    let f = Fixture::new(&format!(
        r#"{JSON_TARGET}
[harnesses.fixture]
pattern = "skills/{{name}}"
[bundles.copied]
harnesses = ["fixture"]
link = false
[bundles.copied.skills.copy]
source = "source/copy"
[bundles.live]
harnesses = ["fixture"]
[bundles.live.skills.live]
source = "source/live"
"#
    ));
    f.write("source/copy/SKILL.md", b"copy");
    f.write("source/live/SKILL.md", b"live");
    f.write(
        "native.json",
        br#"{"theme":"dark","tools":{"servers":{"manual":{"command":"manual"}}}}"#,
    );
    succeeded(f.run(&[]));
    assert_eq!(
        f.json("native.json"),
        json!({"theme":"dark","tools":{"servers":{"manual":{"command":"manual"},"docs":{"type":"http","url":"https://example.com/mcp"}}}})
    );
    assert!(!fs::read_to_string(f.path("cache/uniskill/mcp/state.json"))
        .unwrap()
        .contains("https://example.com"));
    let paths = [
        "skills",
        "native.json",
        "cache/uniskill/state.toml",
        "cache/uniskill/mcp/state.json",
        "cache/uniskill/bundles/copied",
        "cache/uniskill/bundles/live",
    ];
    let before: Vec<_> = paths.iter().map(|p| snapshot(&f.path(p))).collect();
    succeeded(f.run(&[]));
    for (path, previous) in paths.iter().zip(before) {
        assert_eq!(snapshot(&f.path(path)), previous, "changed {path}");
    }
    f.write("source/copy/SKILL.md", b"new copy");
    succeeded(f.run(&[]));
    assert_eq!(
        fs::read(f.path("skills/copy/SKILL.md")).unwrap(),
        b"new copy"
    );
}

#[test]
fn missing_managed_entries_are_recreated_and_mcp_only_skips_skill_materialization() {
    let f = Fixture::new(&format!(
        r#"{JSON_TARGET}
[harnesses.fixture]
pattern = "skills/{{name}}"
[bundles.broken]
harnesses = ["fixture"]
[bundles.broken.skills.missing]
source = "does-not-exist"
"#
    ));
    succeeded(f.run(&["--mcp-only"]));
    assert!(!f.path("cache/uniskill/bundles").exists());
    f.write("native.json", br#"{"keep":true}"#);
    succeeded(f.run(&["--mcp-only"]));
    assert_eq!(f.json("native.json")["keep"], true);
    assert!(f.json("native.json")["tools"]["servers"]["docs"].is_object());
    fs::remove_file(f.path("native.json")).unwrap();
    succeeded(f.run(&["--mcp-only"]));
    assert!(f.json("native.json")["tools"]["servers"]["docs"].is_object());
    assert!(!f.run(&[]).status.success());
}

#[test]
fn configured_targets_cannot_overwrite_mcp_ownership_state() {
    let f = Fixture::new(&JSON_TARGET.replace("native.json", "cache/uniskill/mcp/state.json"));
    assert!(!f.run(&["--mcp-only"]).status.success());
    assert!(!f.path("cache/uniskill/mcp/state.json").exists());
}

#[test]
fn updating_skills_through_the_complete_wrapper_preserves_mcp_ownership() {
    let f = Fixture::new("includes = ['skills.toml', 'mcp.toml']");
    f.write("mcp.toml", JSON_TARGET.as_bytes());
    f.write(
        "skills.toml",
        br#"
[harnesses.skills]
pattern = "skills/{name}"
[bundles.local]
harnesses = ["skills"]
source = "source"
"#,
    );
    f.write("source/skills/first/SKILL.md", b"first");
    succeeded(f.run(&[]));
    let paths = [
        f.path("native.json"),
        f.path("cache/uniskill/mcp/state.json"),
    ];
    let before: Vec<_> = paths
        .iter()
        .map(|p| {
            (
                fs::read(p).unwrap(),
                fs::metadata(p).unwrap().modified().unwrap(),
            )
        })
        .collect();
    f.write("source/skills/second/SKILL.md", b"second");
    succeeded(f.run(&[]));
    assert!(f.path("skills/second/SKILL.md").exists());
    for (path, old) in paths.iter().zip(before) {
        assert_eq!(
            (
                fs::read(path).unwrap(),
                fs::metadata(path).unwrap().modified().unwrap()
            ),
            old
        );
    }
}

#[test]
fn separate_project_writers_and_recovery_share_the_native_destination_lock() {
    use std::fs::{File, TryLockError};
    use std::time::{Duration, Instant};

    let temp = support::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    let native = home.join(".claude.json");
    fs::write(&native, br#"{"keep":true}"#).unwrap();
    let projects: Vec<_> = ["left", "right"]
        .iter()
        .map(|name| {
            let project = temp.path().join(name);
            fs::create_dir(&project).unwrap();
            fs::write(
                project.join("uniskill.toml"),
                format!("[mcp.{name}]\nharnesses = ['claude-code']\ncommand = '{name}'\n"),
            )
            .unwrap();
            project
        })
        .collect();
    let command = |project: &Path| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_uniskill"));
        cmd.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .env("XDG_CACHE_HOME", temp.path().join("cache"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", temp.path().join("gitconfig"));
        cmd.args(["sync", "--mcp-only"])
            .current_dir(project)
            .env("HOME", &home)
            .env_remove("CLAUDE_CONFIG_DIR");
        cmd
    };
    succeeded(command(&projects[0]).output().unwrap());
    let sidecar = fs::read_dir(&home)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".uniskill-mcp-")
        })
        .unwrap();
    let lock = File::options()
        .read(true)
        .write(true)
        .open(sidecar)
        .unwrap();
    lock.lock().unwrap();
    let before = fs::read(&native).unwrap();
    // A completed write with a journal still awaiting removal must also
    // recover under the destination lock used by the other project.
    let state = projects[0].join(".uniskill-cache/mcp");
    let after = String::from_utf8(before).unwrap();
    fs::write(state.join("journal.json"), serde_json::to_vec(&json!({
        "version":1,"updates":[{"path":native,"before":"{\"keep\":true}","after":after,"mode":384}]
    })).unwrap()).unwrap();
    let children: Vec<_> = projects
        .iter()
        .map(|p| command(p).spawn().unwrap())
        .collect();
    let deadline = Instant::now() + Duration::from_secs(5);
    for project in &projects {
        loop {
            let waiting = File::options()
                .read(true)
                .write(true)
                .open(project.join(".uniskill-cache/mcp/lock"))
                .is_ok_and(|file| matches!(file.try_lock(), Err(TryLockError::WouldBlock)));
            if waiting {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "writer did not reach its native lock"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert_eq!(fs::read_to_string(&native).unwrap(), after);
    assert!(state.join("journal.json").exists());
    drop(lock);
    for child in children {
        succeeded(child.wait_with_output().unwrap());
    }
    let result: Value = serde_json::from_slice(&fs::read(&native).unwrap()).unwrap();
    assert_eq!(result["keep"], true);
    assert_eq!(result["mcpServers"]["left"]["command"], "left");
    assert_eq!(result["mcpServers"]["right"]["command"], "right");
    assert!(!state.join("journal.json").exists());
    let bytes = fs::read(&native).unwrap();
    let modified = fs::metadata(&native).unwrap().modified().unwrap();
    for project in &projects {
        succeeded(command(project).output().unwrap());
        let ownership: Value = serde_json::from_slice(
            &fs::read(project.join(".uniskill-cache/mcp/state.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(ownership["entries"].as_array().unwrap().len(), 1);
    }
    assert_eq!(fs::read(&native).unwrap(), bytes);
    assert_eq!(fs::metadata(native).unwrap().modified().unwrap(), modified);
}

#[test]
fn restored_ownership_is_locked_even_when_the_journal_only_updates_state() {
    use std::fs::{File, TryLockError};
    use std::time::{Duration, Instant};

    let f = Fixture::new(JSON_TARGET);
    f.write("native.json", br#"{"keep":true}"#);
    succeeded(f.run(&["--mcp-only"]));
    let state = f.path("cache/uniskill/mcp/state.json");
    let owned = fs::read_to_string(&state).unwrap();
    let definition = f.json("native.json")["tools"]["servers"]["docs"].clone();
    // Removing an already absent owned entry only writes ownership. Simulate
    // interruption after that state write but before its journal was removed.
    f.write("native.json", br#"{"keep":true}"#);
    fs::write(&f.config, "").unwrap();
    succeeded(f.run(&["--mcp-only"]));
    let empty = fs::read_to_string(&state).unwrap();
    let journal = f.path("cache/uniskill/mcp/journal.json");
    fs::write(
        &journal,
        serde_json::to_vec(&json!({
            "version":1,"updates":[{"path":state,"before":owned,"after":empty,"mode":384}]
        }))
        .unwrap(),
    )
    .unwrap();

    f.write(
        "other.toml",
        JSON_TARGET.replace("mcp.docs", "mcp.manual").as_bytes(),
    );
    let other = || {
        let mut cmd = f.command(env!("CARGO_BIN_EXE_uniskill"));
        cmd.args(["--config", f.path("other.toml").to_str().unwrap(), "sync"])
            .current_dir(f.temp.path())
            .env("XDG_CACHE_HOME", f.path("other-cache"));
        cmd
    };
    succeeded(other().arg("--mcp-only").output().unwrap());
    let other_state = f.path("other-cache/uniskill/mcp/state.json");
    let other_owned = fs::read(&other_state).unwrap();
    let mut recreated = f.json("native.json");
    recreated["tools"]["servers"]["docs"] = definition;
    f.write("native.json", &serde_json::to_vec(&recreated).unwrap());
    let original = fs::read(f.path("native.json")).unwrap();
    let sidecar = fs::read_dir(f.temp.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".uniskill-mcp-")
        })
        .unwrap();
    let guard = File::options()
        .read(true)
        .write(true)
        .open(sidecar)
        .unwrap();
    guard.lock().unwrap();
    let mut child = f
        .command(env!("CARGO_BIN_EXE_uniskill"))
        .args(["--config", f.config.to_str().unwrap(), "sync", "--mcp-only"])
        .current_dir(f.temp.path())
        .env("XDG_CACHE_HOME", f.path("cache"))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let file = File::options()
            .read(true)
            .write(true)
            .open(f.path("cache/uniskill/mcp/lock"))
            .unwrap();
        if matches!(file.try_lock(), Err(TryLockError::WouldBlock)) {
            break;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "recovery finished while its restored destination was locked"
        );
        assert!(Instant::now() < deadline, "recovery did not reach its lock");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert!(child.try_wait().unwrap().is_none());
    assert_eq!(fs::read(f.path("native.json")).unwrap(), original);
    assert_eq!(fs::read_to_string(&state).unwrap(), empty);
    assert!(journal.exists());
    drop(guard);
    succeeded(child.wait_with_output().unwrap());
    let result = f.json("native.json");
    assert_eq!(result["keep"], true);
    assert!(result["tools"]["servers"]["docs"].is_null());
    assert!(result["tools"]["servers"]["manual"].is_object());
    assert_eq!(fs::read(&other_state).unwrap(), other_owned);
    succeeded(other().arg("--check-mcp").output().unwrap());
    succeeded(f.run(&["--check-mcp"]));
}

#[test]
fn deleted_retired_directories_expire_tracking_even_with_dangling_aliases() {
    use std::os::unix::fs::symlink;
    for replacement in [None, Some("Unrelated file.")] {
        let f = Fixture::new(&JSON_TARGET.replace("native.json", "alias/config.json"));
        fs::create_dir(f.path("native")).unwrap();
        symlink("native", f.path("alias")).unwrap();
        succeeded(f.run(&["--mcp-only"]));
        fs::write(&f.config, "").unwrap();
        succeeded(f.run(&["--mcp-only"]));
        let state = f.path("cache/uniskill/mcp/state.json");
        let before = fs::read(&state).unwrap();
        fs::remove_file(f.path("native/config.json")).unwrap();
        for entry in fs::read_dir(f.path("native")).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        fs::remove_dir(f.path("native")).unwrap();
        if let Some(bytes) = replacement {
            f.write("native", bytes.as_bytes());
        }
        let check = f.run(&["--check-mcp"]);
        assert!(!check.status.success());
        assert!(String::from_utf8_lossy(&check.stderr).contains("MCP definitions need syncing"));
        assert_eq!(fs::read(&state).unwrap(), before);
        succeeded(f.run(&["--mcp-only"]));
        assert_eq!(f.json("cache/uniskill/mcp/state.json")["files"], json!([]));
        f.write("source/SKILL.md", b"Fixture skill.\n");
        fs::write(&f.config, "[harnesses.fixture]\npattern='skills/{name}'\n[bundles.local]\nharnesses=['fixture']\n[bundles.local.skills.fixture]\nsource='source'\n").unwrap();
        succeeded(f.run(&[]));
        assert!(f.path("skills/fixture/SKILL.md").is_file());
        succeeded(f.run(&["--check-mcp"]));
        if let Some(bytes) = replacement {
            assert_eq!(fs::read_to_string(f.path("native")).unwrap(), bytes);
        } else {
            assert!(!f.path("native").exists());
        }
        assert_eq!(fs::read_link(f.path("alias")).unwrap(), Path::new("native"));
    }
}
