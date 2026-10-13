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
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        fs::write(&config, text).unwrap();
        Self { temp, config }
    }

    fn path(&self, path: &str) -> PathBuf {
        self.temp.path().join(path)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_uniskill"))
            .args(["--config", self.config.to_str().unwrap(), "sync"])
            .args(args)
            .current_dir(self.temp.path())
            .env("XDG_CACHE_HOME", self.path("cache"))
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
fn custom_harness_routes_once_preserves_settings_and_is_idempotent() {
    let f = Fixture::new(JSON_TARGET);
    f.write(
        "native.json",
        br#"{"theme":"dark","tools":{"servers":{"manual":{"command":"manual"}}}}"#,
    );
    succeeded(f.run(&[]));
    assert_eq!(
        f.json("native.json"),
        json!({"theme":"dark","tools":{"servers":{"manual":{"command":"manual"},"docs":{"type":"http","url":"https://example.com/mcp"}}}})
    );
    let bytes = fs::read(f.path("native.json")).unwrap();
    let modified = fs::metadata(f.path("native.json"))
        .unwrap()
        .modified()
        .unwrap();
    succeeded(f.run(&[]));
    succeeded(f.run(&["--check-mcp"]));
    assert_eq!(fs::read(f.path("native.json")).unwrap(), bytes);
    assert_eq!(
        fs::metadata(f.path("native.json"))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    let state = fs::read_to_string(f.path("cache/uniskill/mcp/state.json")).unwrap();
    assert!(!state.contains("https://example.com"));
}

#[test]
fn check_is_read_only_and_reports_pending_native_changes() {
    let f = Fixture::new(JSON_TARGET);
    assert!(!f.run(&["--check-mcp"]).status.success());
    assert!(!f.path("native.json").exists());
    assert!(!f.path("cache").exists());
}

#[test]
fn adoption_requires_identical_definitions_and_default_sync_refuses_unmanaged_entries() {
    let f = Fixture::new(JSON_TARGET);
    let original = br#"{"tools":{"servers":{"docs":{"type":"http","url":"https://example.com/mcp"}}},"keep":true}"#;
    f.write("native.json", original);
    assert!(!f.run(&[]).status.success());
    assert_eq!(fs::read(f.path("native.json")).unwrap(), original);
    succeeded(f.run(&["--adopt-mcp"]));
    assert_eq!(fs::read(f.path("native.json")).unwrap(), original);
    let edited = br#"{"tools":{"servers":{"docs":{"type":"http","url":"https://example.com/edited"}}},"keep":true}"#;
    f.write("native.json", edited);
    assert!(!f.run(&["--adopt-mcp"]).status.success());
    assert_eq!(fs::read(f.path("native.json")).unwrap(), edited);
}

#[test]
fn removing_owned_entries_keeps_manual_servers_and_user_edits() {
    let f = Fixture::new(JSON_TARGET);
    succeeded(f.run(&[]));
    let mut value = f.json("native.json");
    value["tools"]["servers"]["manual"] = json!({"command":"manual"});
    f.write("native.json", &serde_json::to_vec(&value).unwrap());
    fs::write(&f.config, "").unwrap();
    succeeded(f.run(&[]));
    assert_eq!(
        f.json("native.json"),
        json!({"tools":{"servers":{"manual":{"command":"manual"}}}})
    );
    fs::write(&f.config, JSON_TARGET).unwrap();
    succeeded(f.run(&[]));
    let mut value = f.json("native.json");
    value["tools"]["servers"]["docs"]["url"] = json!("https://example.com/edited");
    f.write("native.json", &serde_json::to_vec(&value).unwrap());
    fs::write(&f.config, "").unwrap();
    assert!(!f.run(&[]).status.success());
    assert_eq!(f.json("native.json"), value);
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

#[test]
fn json_and_toml_translate_references_without_resolving_secrets() {
    let f = Fixture::new(
        r#"
[harnesses.json.mcp]
path="native.json"
format="json"
table=["mcpServers"]
[harnesses.toml.mcp]
path="native.toml"
format="toml"
table=["mcp_servers"]
references="fields"
omit=["type"]
[harnesses.toml.mcp.fields]
headers="http_headers"
env_headers="env_http_headers"
[mcp.docs]
harnesses=["json","toml"]
url="https://example.com/mcp"
bearer_token_env_var="DOCS_TOKEN"
[mcp.docs.headers]
X-Region="example"
[mcp.docs.env_headers]
X-Account="ACCOUNT"
[mcp.docs.options.toml]
tool_timeout_sec=45
enabled=true
[mcp.local]
harnesses=["json","toml"]
command="fixture"
args=["--flag"]
env_vars=["LOCAL_TOKEN"]
[mcp.local.env]
MODE="read"
"#,
    );
    f.write(
        "native.toml",
        b"# owned by the agent\nmodel = 'example' # keep this comment\n",
    );
    succeeded(f.run(&[]));
    let value = f.json("native.json");
    assert_eq!(
        value["mcpServers"]["docs"]["headers"]["Authorization"],
        "Bearer ${DOCS_TOKEN}"
    );
    assert_eq!(
        value["mcpServers"]["docs"]["headers"]["X-Account"],
        "${ACCOUNT}"
    );
    assert_eq!(
        value["mcpServers"]["local"]["env"]["LOCAL_TOKEN"],
        "${LOCAL_TOKEN}"
    );
    let text = fs::read_to_string(f.path("native.toml")).unwrap();
    assert!(text.contains("# owned by the agent"));
    assert!(text.contains("# keep this comment"));
    let value: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(
        value["mcp_servers"]["docs"]["bearer_token_env_var"].as_str(),
        Some("DOCS_TOKEN")
    );
    assert_eq!(
        value["mcp_servers"]["docs"]["env_http_headers"]["X-Account"].as_str(),
        Some("ACCOUNT")
    );
    assert_eq!(
        value["mcp_servers"]["local"]["env_vars"][0].as_str(),
        Some("LOCAL_TOKEN")
    );
    assert_eq!(value["model"].as_str(), Some("example"));
    assert_eq!(
        value["mcp_servers"]["docs"]["tool_timeout_sec"].as_integer(),
        Some(45)
    );
    assert_eq!(
        value["mcp_servers"]["docs"]["enabled"].as_bool(),
        Some(true)
    );
}

#[test]
fn includes_resolve_each_files_sources_and_targets_at_their_owner() {
    let f = Fixture::new("includes=['child/config.toml']\n");
    f.write("child/config.toml", JSON_TARGET.as_bytes());
    succeeded(f.run(&[]));
    assert!(f.path("child/native.json").exists());
    assert!(!f.path("native.json").exists());
    f.write("child/config.toml", b"includes=['../config.toml']\n");
    assert!(!f.run(&[]).status.success());
}

#[test]
fn duplicate_server_names_across_includes_and_namespace_aliases_are_rejected() {
    let f = Fixture::new(&format!("includes=['child.toml']\n{JSON_TARGET}"));
    f.write(
        "child.toml",
        b"[mcp.docs]\nharnesses=['fixture']\nurl='https://example.com/mcp'\n",
    );
    assert!(!f.run(&[]).status.success());
    assert!(!f.path("cache").exists());
    fs::write(&f.config, format!("{JSON_TARGET}\n[mcp.a-b]\nharnesses=['fixture']\ncommand='fixture'\n[mcp.a_b]\nharnesses=['fixture']\ncommand='fixture'\n")).unwrap();
    assert!(!f.run(&[]).status.success());
    assert!(!f.path("native.json").exists());
}

#[test]
fn skill_only_configs_create_no_mcp_state() {
    let f = Fixture::new("");
    succeeded(f.run(&[]));
    assert!(!f.path("cache/uniskill/mcp").exists());
}

#[test]
fn symlinked_native_config_is_not_replaced() {
    let f = Fixture::new(JSON_TARGET);
    f.write("original.json", b"{}");
    std::os::unix::fs::symlink(f.path("original.json"), f.path("native.json")).unwrap();
    assert!(!f.run(&[]).status.success());
    assert_eq!(fs::read(f.path("original.json")).unwrap(), b"{}");
    assert!(fs::symlink_metadata(f.path("native.json"))
        .unwrap()
        .file_type()
        .is_symlink());
}

fn git(path: &Path, args: &[&str]) {
    assert!(Command::new("git")
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
    let output = Command::new(env!("CARGO_BIN_EXE_uniskill"))
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
    succeeded(f.run(&[]));
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
fn unsafe_target_mappings_and_state_destinations_fail_without_native_writes() {
    for suffix in [
        "omit = [\"url\"]",
        "fields = { typo = \"url\" }",
        "fields = { type = \"url\" }",
    ] {
        let config = JSON_TARGET.replace(
            "table = [\"tools\", \"servers\"]",
            &format!("table = [\"tools\", \"servers\"]\n{suffix}"),
        );
        let f = Fixture::new(&config);
        assert!(!f.run(&["--mcp-only"]).status.success());
        assert!(!f.path("native.json").exists());
    }
    let f = Fixture::new(&JSON_TARGET.replace("native.json", "cache/uniskill/mcp/state.json"));
    assert!(!f.run(&["--mcp-only"]).status.success());
    assert!(!f.path("cache/uniskill/mcp/state.json").exists());
}

#[test]
fn adoption_check_reports_pending_ownership_without_writing_it() {
    let f = Fixture::new(JSON_TARGET);
    f.write(
        "native.json",
        br#"{"tools":{"servers":{"docs":{"type":"http","url":"https://example.com/mcp"}}}}"#,
    );
    assert!(!f.run(&["--check-mcp", "--adopt-mcp"]).status.success());
    assert!(!f.path("cache").exists());
    succeeded(f.run(&["--mcp-only", "--adopt-mcp"]));
    succeeded(f.run(&["--check-mcp"]));
}

#[test]
fn routes_through_symlinked_ancestors_keep_stable_ownership_for_new_directories() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new(&JSON_TARGET.replace("native.json", "alias/new/native.json"));
    fs::create_dir(f.path("real")).unwrap();
    symlink(f.path("real"), f.path("alias")).unwrap();
    succeeded(f.run(&["--mcp-only"]));
    let state = fs::read(f.path("cache/uniskill/mcp/state.json")).unwrap();
    let modified = fs::metadata(f.path("real/new/native.json"))
        .unwrap()
        .modified()
        .unwrap();
    succeeded(f.run(&["--mcp-only"]));
    assert_eq!(
        fs::read(f.path("cache/uniskill/mcp/state.json")).unwrap(),
        state
    );
    assert_eq!(
        fs::metadata(f.path("real/new/native.json"))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
}

#[test]
fn overlapping_server_maps_are_rejected_before_writing_native_configs() {
    let f = Fixture::new(&format!(
        r#"{JSON_TARGET}
[harnesses.overlap.mcp]
path = "native.json"
format = "json"
table = ["tools", "servers", "docs"]
[mcp.other]
harnesses = ["overlap"]
command = "fixture"
"#
    ));
    assert!(!f.run(&["--mcp-only"]).status.success());
    assert!(!f.path("native.json").exists());
}

#[test]
fn native_connection_aliases_cannot_be_supplied_as_policy_options() {
    let f = Fixture::new(
        &format!(
            "{JSON_TARGET}\n[mcp.docs.headers]\nAuthorization = 'literal'\n\
         [mcp.docs.options.fixture.env_http_headers]\nAuthorization = 'TOKEN'\n"
        )
        .replace(
            "table = [\"tools\", \"servers\"]",
            "table = [\"tools\", \"servers\"]\nfields = { env_headers = 'env_http_headers' }",
        ),
    );
    assert!(!f.run(&["--mcp-only"]).status.success());
    assert!(!f.path("native.json").exists());
    assert!(!f.path("cache/uniskill/mcp/state.json").exists());
}

#[test]
fn final_names_include_unmanaged_entries_but_allow_owned_renames() {
    let f = Fixture::new(&JSON_TARGET.replace("mcp.docs", "mcp.a-b"));
    let original = br#"{"tools":{"servers":{"a_b":{"command":"manual"}}}}"#;
    f.write("native.json", original);
    assert!(!f.run(&["--mcp-only"]).status.success());
    assert_eq!(fs::read(f.path("native.json")).unwrap(), original);
    assert!(!f.path("cache/uniskill/mcp/state.json").exists());
    f.write(
        "native.json",
        br#"{"tools":{"servers":{"manual":{"command":"manual"}}}}"#,
    );
    fs::write(&f.config, JSON_TARGET.replace("mcp.docs", "mcp.a_b")).unwrap();
    succeeded(f.run(&["--mcp-only"]));
    fs::write(&f.config, JSON_TARGET.replace("mcp.docs", "mcp.a-b")).unwrap();
    succeeded(f.run(&["--mcp-only"]));
    let servers = f.json("native.json")["tools"]["servers"]
        .as_object()
        .unwrap()
        .clone();
    assert_eq!(servers.len(), 2);
    assert!(servers.contains_key("a-b"));
    assert!(servers.contains_key("manual"));
    succeeded(f.run(&["--check-mcp"]));
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

    let temp = tempfile::tempdir().unwrap();
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
