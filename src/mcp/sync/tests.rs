use super::*;
use serde_json::json;
use std::fs;

struct Fixture {
    temp: tempfile::TempDir,
    config: PathBuf,
}

impl Fixture {
    fn new(text: &str) -> Self {
        let temp = crate::test_support::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        fs::write(&config, text).unwrap();
        Self { temp, config }
    }

    fn path(&self, path: &str) -> PathBuf {
        self.temp.path().join(path)
    }

    fn write(&self, path: &str, data: &[u8]) {
        fs::create_dir_all(self.path(path).parent().unwrap()).unwrap();
        fs::write(self.path(path), data).unwrap();
    }

    fn json(&self, path: &str) -> Value {
        serde_json::from_slice(&fs::read(self.path(path)).unwrap()).unwrap()
    }

    fn apply(&self, adopt: bool) -> Result<Report> {
        let config = crate::config::parse_config(&self.config)?;
        let registry = crate::harnesses::registry(&config)?;
        let cache = self.path("cache/uniskill");
        let storage = crate::cache::storage(&cache, &cache)?;
        sync(&config.mcp, &registry, &cache, None, adopt, &storage)
    }

    fn check(&self, adopt: bool) -> Result<Report> {
        let config = crate::config::parse_config(&self.config)?;
        let registry = crate::harnesses::registry(&config)?;
        let cache = self.path("cache/uniskill");
        let storage = crate::cache::storage(&cache, &cache)?;
        check(&config.mcp, &registry, &cache, None, adopt, &storage)
    }
}

const JSON_TARGET: &str = "[harnesses.fixture.mcp]\npath='native.json'\nformat='json'\ntable=['tools','servers']\n[mcp.docs]\nharnesses=['fixture']\nurl='https://example.com/mcp'\n";

#[test]
fn adoption_requires_identical_definitions_and_default_sync_refuses_unmanaged_entries() {
    let f = Fixture::new(JSON_TARGET);
    let original = br#"{"tools":{"servers":{"docs":{"type":"http","url":"https://example.com/mcp"}}},"keep":true}"#;
    f.write("native.json", original);
    assert!(f.apply(false).is_err());
    assert_eq!(fs::read(f.path("native.json")).unwrap(), original);
    assert!(f.check(true).unwrap().changed);
    assert!(!f.path("cache/uniskill/mcp/state.json").exists());
    f.apply(true).unwrap();
    assert_eq!(fs::read(f.path("native.json")).unwrap(), original);
    let edited = br#"{"tools":{"servers":{"docs":{"type":"http","url":"https://example.com/edited"}}},"keep":true}"#;
    f.write("native.json", edited);
    assert!(f.apply(true).is_err());
    assert_eq!(fs::read(f.path("native.json")).unwrap(), edited);
}

#[test]
fn removing_owned_entries_keeps_manual_servers_and_user_edits() {
    let f = Fixture::new(JSON_TARGET);
    f.apply(false).unwrap();
    let mut value = f.json("native.json");
    value["tools"]["servers"]["manual"] = json!({"command":"manual"});
    f.write("native.json", &serde_json::to_vec(&value).unwrap());
    fs::write(&f.config, "").unwrap();
    f.apply(false).unwrap();
    assert_eq!(
        f.json("native.json"),
        json!({"tools":{"servers":{"manual":{"command":"manual"}}}})
    );
    fs::write(&f.config, JSON_TARGET).unwrap();
    f.apply(false).unwrap();
    let mut value = f.json("native.json");
    value["tools"]["servers"]["docs"]["url"] = json!("https://example.com/edited");
    f.write("native.json", &serde_json::to_vec(&value).unwrap());
    fs::write(&f.config, "").unwrap();
    assert!(f.apply(false).is_err());
    assert_eq!(f.json("native.json"), value);
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
    f.apply(false).unwrap();
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
fn symlinked_native_config_is_not_replaced() {
    let f = Fixture::new(JSON_TARGET);
    f.write("original.json", b"{}");
    std::os::unix::fs::symlink(f.path("original.json"), f.path("native.json")).unwrap();
    assert!(f.apply(false).is_err());
    assert_eq!(fs::read(f.path("original.json")).unwrap(), b"{}");
    assert!(fs::symlink_metadata(f.path("native.json"))
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn routes_through_symlinked_ancestors_keep_stable_ownership_for_new_directories() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new(&JSON_TARGET.replace("native.json", "alias/new/native.json"));
    fs::create_dir(f.path("real")).unwrap();
    symlink(f.path("real"), f.path("alias")).unwrap();
    f.apply(false).unwrap();
    let state = fs::read(f.path("cache/uniskill/mcp/state.json")).unwrap();
    let modified = fs::metadata(f.path("real/new/native.json"))
        .unwrap()
        .modified()
        .unwrap();
    f.apply(false).unwrap();
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
    assert!(f.apply(false).is_err());
    assert!(!f.path("native.json").exists());
}

#[test]
fn final_names_include_unmanaged_entries_but_allow_owned_renames() {
    let f = Fixture::new(&JSON_TARGET.replace("mcp.docs", "mcp.a-b"));
    let original = br#"{"tools":{"servers":{"a_b":{"command":"manual"}}}}"#;
    f.write("native.json", original);
    assert!(f.apply(false).is_err());
    assert_eq!(fs::read(f.path("native.json")).unwrap(), original);
    assert!(!f.path("cache/uniskill/mcp/state.json").exists());
    f.write(
        "native.json",
        br#"{"tools":{"servers":{"manual":{"command":"manual"}}}}"#,
    );
    fs::write(&f.config, JSON_TARGET.replace("mcp.docs", "mcp.a_b")).unwrap();
    f.apply(false).unwrap();
    fs::write(&f.config, JSON_TARGET.replace("mcp.docs", "mcp.a-b")).unwrap();
    f.apply(false).unwrap();
    let servers = f.json("native.json")["tools"]["servers"]
        .as_object()
        .unwrap()
        .clone();
    assert_eq!(servers.len(), 2);
    assert!(servers.contains_key("a-b"));
    assert!(servers.contains_key("manual"));
    assert!(!f.check(false).unwrap().changed);
}

#[test]
fn invalid_restored_ownership_preserves_the_journal_and_current_state() {
    let f = Fixture::new("");
    let state = f.path("cache/uniskill/mcp/state.json");
    let current = r#"{"version":1,"entries":[]}"#;
    f.write("cache/uniskill/mcp/state.json", current.as_bytes());
    let journal = serde_json::to_vec(&json!({
        "version":1,"updates":[{"path":state,"before":"invalid","after":current,"mode":384}]
    }))
    .unwrap();
    f.write("cache/uniskill/mcp/journal.json", &journal);
    assert!(f.apply(false).is_err());
    assert_eq!(fs::read_to_string(state).unwrap(), current);
    assert_eq!(
        fs::read(f.path("cache/uniskill/mcp/journal.json")).unwrap(),
        journal
    );
}

#[test]
fn retired_native_files_remain_tracked_until_deleted_without_granting_entry_ownership() {
    let f = Fixture::new(JSON_TARGET);
    f.write("native.json", br#"{"userSetting":true}"#);
    f.apply(false).unwrap();
    let state = f.path("cache/uniskill/mcp/state.json");
    // Exercise automatic v1 loading through the production migration path.
    let mut legacy: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    legacy["version"] = json!(1);
    legacy.as_object_mut().unwrap().remove("files");
    fs::write(&state, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let before_native = fs::read(f.path("native.json")).unwrap();
    f.apply(false).unwrap();
    assert_eq!(fs::read(f.path("native.json")).unwrap(), before_native);
    fs::write(&f.config, "").unwrap();
    f.apply(false).unwrap();
    let retired = f.json("cache/uniskill/mcp/state.json");
    assert_eq!(retired["version"], 2);
    assert_eq!(retired["entries"], json!([]));
    assert_eq!(retired["files"], json!([f.path("native.json")]));
    assert_eq!(f.json("native.json")["userSetting"], true);
    let before = (
        fs::read(&state).unwrap(),
        fs::metadata(&state).unwrap().modified().unwrap(),
        fs::read(f.path("native.json")).unwrap(),
    );
    f.apply(false).unwrap();
    assert!(!f.check(false).unwrap().changed);
    assert_eq!(
        (
            fs::read(&state).unwrap(),
            fs::metadata(&state).unwrap().modified().unwrap(),
            fs::read(f.path("native.json")).unwrap()
        ),
        before
    );
    f.write("native.json", br#"{"userSetting":true,"tools":{"servers":{"docs":{"url":"https://example.com/mcp","type":"http"}}}}"#);
    fs::write(&f.config, JSON_TARGET).unwrap();
    assert!(f.apply(false).is_err());
    fs::write(&f.config, "").unwrap();
    fs::remove_file(f.path("native.json")).unwrap();
    f.apply(false).unwrap();
    assert_eq!(f.json("cache/uniskill/mcp/state.json")["files"], json!([]));
    assert!(!f.path("native.json").exists());
}

#[test]
fn recovery_preserved_native_files_are_tracked_without_prior_entry_ownership() {
    let f = Fixture::new("");
    let native = f.path("native.json");
    f.write("native.json", br#"{"externalSetting":true}"#);
    f.write(
        "cache/uniskill/mcp/journal.json",
        &serde_json::to_vec(&json!({
            "version":1,"updates":[{"path":native,"before":null,"after":"{}","mode":384}]
        }))
        .unwrap(),
    );
    let before_native = fs::read(&native).unwrap();
    f.apply(false).unwrap();
    assert_eq!(fs::read(&native).unwrap(), before_native);
    let state = f.json("cache/uniskill/mcp/state.json");
    assert_eq!(state["entries"], json!([]));
    assert_eq!(state["files"], json!([native]));
    let path = f.path("cache/uniskill/mcp/state.json");
    let before = (
        fs::read(&path).unwrap(),
        fs::metadata(&path).unwrap().modified().unwrap(),
    );
    f.apply(false).unwrap();
    assert_eq!(
        (
            fs::read(&path).unwrap(),
            fs::metadata(&path).unwrap().modified().unwrap()
        ),
        before
    );
}

#[test]
fn interrupted_retired_tracking_cleanup_recovers_with_deleted_directory_aliases() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new(&JSON_TARGET.replace("native.json", "alias/config.json"));
    fs::create_dir(f.path("native")).unwrap();
    symlink("native", f.path("alias")).unwrap();
    f.apply(false).unwrap();
    fs::write(&f.config, "").unwrap();
    f.apply(false).unwrap();
    let state = f.path("cache/uniskill/mcp/state.json");
    let before = fs::read_to_string(&state).unwrap();
    fs::remove_file(f.path("native/config.json")).unwrap();
    for entry in fs::read_dir(f.path("native")).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    fs::remove_dir(f.path("native")).unwrap();
    let after = json!({"version":2,"entries":[],"files":[]}).to_string();
    fs::write(&state, &after).unwrap();
    let journal = f.path("cache/uniskill/mcp/journal.json");
    fs::write(
        &journal,
        serde_json::to_vec(&json!({"version":1,"updates":[{
            "path":state,"before":before,"after":after,"mode":384
        }]}))
        .unwrap(),
    )
    .unwrap();
    f.apply(false).unwrap();
    assert_eq!(f.json("cache/uniskill/mcp/state.json")["files"], json!([]));
    assert!(!journal.exists());
    assert!(!f.path("native").exists());
    let converged = (
        fs::read(&state).unwrap(),
        fs::metadata(&state).unwrap().modified().unwrap(),
    );
    f.apply(false).unwrap();
    assert!(!f.check(false).unwrap().changed);
    assert_eq!(
        (
            fs::read(&state).unwrap(),
            fs::metadata(&state).unwrap().modified().unwrap()
        ),
        converged
    );
}

#[test]
fn active_ownership_and_native_recovery_writes_reject_dangling_parents() {
    use std::os::unix::fs::symlink;
    for recovery in [false, true] {
        let f = Fixture::new(&JSON_TARGET.replace("native.json", "native/config.json"));
        f.apply(false).unwrap();
        fs::write(&f.config, "").unwrap();
        if recovery {
            f.apply(false).unwrap();
        }
        fs::remove_file(f.path("native/config.json")).unwrap();
        for entry in fs::read_dir(f.path("native")).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        fs::remove_dir(f.path("native")).unwrap();
        symlink("missing", f.path("native")).unwrap();
        let state = f.path("cache/uniskill/mcp/state.json");
        let before = fs::read(&state).unwrap();
        let journal = f.path("cache/uniskill/mcp/journal.json");
        if recovery {
            fs::write(
                &journal,
                serde_json::to_vec(&json!({"version":1,"updates":[{
                    "path":f.path("native/config.json"),"before":null,"after":"{}","mode":384
                }]}))
                .unwrap(),
            )
            .unwrap();
        }
        let journal_bytes = fs::read(&journal).ok();
        for _ in 0..2 {
            assert!(f.apply(false).is_err());
            assert_eq!(fs::read(&state).unwrap(), before);
            assert_eq!(fs::read(&journal).ok(), journal_bytes);
            assert_eq!(
                fs::read_link(f.path("native")).unwrap(),
                Path::new("missing")
            );
            assert!(!f.path("missing").exists());
        }
    }
}
