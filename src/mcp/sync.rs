use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    document::Document,
    transaction::{self, Update},
    Format, Server,
};
use crate::{config, harnesses::HarnessDef, worktree::WorktreeContext};

#[derive(Debug, Clone)]
pub struct Event {
    pub server: String,
    pub target: PathBuf,
    pub status: &'static str,
}

#[derive(Debug, Default)]
pub struct Report {
    pub events: Vec<Event>,
    pub changed: bool,
    pub preserved: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    path: PathBuf,
    table: Vec<String>,
    name: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Owned {
    identity: Identity,
    format: Format,
    digest: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    entries: Vec<Owned>,
}

struct Plan {
    updates: Vec<Update>,
    report: Report,
}

struct LoadedState {
    bytes: Option<Vec<u8>>,
    owned: BTreeMap<Identity, Owned>,
}

pub fn check(
    servers: &BTreeMap<String, Server>,
    registry: &HashMap<String, HarnessDef>,
    state_dir: &Path,
    worktree: Option<&WorktreeContext>,
    adopt: bool,
) -> Result<Report> {
    let state_dir = absolute_path(&state_dir.join("mcp").to_string_lossy())?;
    if transaction::pending(&state_dir) {
        bail!("interrupted MCP transaction; run sync to recover before checking");
    }
    Ok(plan(
        desired_entries(servers, registry, worktree)?,
        &state_dir,
        worktree,
        adopt,
    )?
    .report)
}

pub fn sync(
    servers: &BTreeMap<String, Server>,
    registry: &HashMap<String, HarnessDef>,
    state_dir: &Path,
    worktree: Option<&WorktreeContext>,
    adopt: bool,
) -> Result<Report> {
    let state_dir = absolute_path(&state_dir.join("mcp").to_string_lossy())?;
    let desired = desired_entries(servers, registry, worktree)?;
    if desired.is_empty() && !state_dir.exists() {
        return Ok(Report::default());
    }
    let _lock = transaction::lock(&state_dir)?;
    let previous = load_state(&state_dir)?;
    let mut paths: BTreeSet<PathBuf> = desired
        .keys()
        .chain(previous.owned.keys())
        .map(|identity| identity.path.clone())
        .collect();
    for path in transaction::journal_targets(&state_dir)? {
        if path != state_dir.join("state.json") {
            paths.insert(absolute_path(&path.to_string_lossy())?);
        }
    }
    let _destinations = transaction::lock_destinations(&paths)?;
    let preserved = transaction::recover(&state_dir)?;
    let mut plan = plan(desired, &state_dir, worktree, adopt)?;
    transaction::commit(&state_dir, plan.updates)?;
    plan.report.preserved = preserved;
    Ok(plan.report)
}

fn absolute_path(path: &str) -> Result<PathBuf> {
    if path.contains('$') {
        bail!("MCP target has an unresolved environment variable");
    }
    let path = Path::new(path);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            part => normalized.push(part.as_os_str()),
        }
    }
    if normalized.file_name().is_none() {
        bail!("MCP target must name a file");
    }
    let mut ancestor = normalized.parent().expect("absolute file has a parent");
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| anyhow!("MCP target has no existing ancestor"))?;
    }
    normalized = ancestor
        .canonicalize()?
        .join(normalized.strip_prefix(ancestor)?);
    Ok(normalized)
}

fn fingerprint(value: &Value) -> Result<String> {
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), canonical(v)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(array) => Value::Array(array.iter().map(canonical).collect()),
            other => other.clone(),
        }
    }
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&canonical(value))?)
    ))
}

fn desired_entries(
    servers: &BTreeMap<String, Server>,
    registry: &HashMap<String, HarnessDef>,
    worktree: Option<&WorktreeContext>,
) -> Result<BTreeMap<Identity, (Format, Value)>> {
    let mut desired = BTreeMap::<Identity, (Format, Value)>::new();
    for (name, server) in servers {
        if !super::model::valid_name(name) {
            bail!("invalid MCP server identity");
        }
        server
            .validate()
            .with_context(|| format!("MCP server {name}"))?;
        for harness_name in &server.harnesses {
            let harness = registry
                .get(harness_name)
                .ok_or_else(|| anyhow!("MCP server {name}: unknown harness {harness_name}"))?;
            let target = harness.mcp.as_ref().ok_or_else(|| {
                anyhow!("MCP server {name}: harness {harness_name} has no MCP target")
            })?;
            let path = absolute_path(&config::expand_env_vars(&target.path))?;
            let path = match worktree {
                Some(ctx) => match ctx.retarget(&path.to_string_lossy()) {
                    Some(path) => PathBuf::from(path),
                    None => continue,
                },
                None => path,
            };
            let identity = Identity {
                path,
                table: target.table.clone(),
                name: name.clone(),
            };
            let value = server
                .render(target, harness_name)
                .with_context(|| format!("MCP server {name} for harness {harness_name}"))?;
            if let Some((format, old)) = desired.get(&identity) {
                if *format != target.format || *old != value {
                    bail!("conflicting MCP routes target the same server entry");
                }
            } else {
                desired.insert(identity, (target.format, value));
            }
        }
    }
    Ok(desired)
}

fn load_state(state_dir: &Path) -> Result<LoadedState> {
    let state_path = state_dir.join("state.json");
    let state_bytes = transaction::read(&state_path)?;
    let previous: State = match &state_bytes {
        Some(bytes) => serde_json::from_slice(bytes).map_err(|_| {
            anyhow!("invalid MCP ownership state; refusing to change native configuration")
        })?,
        None => State {
            version: 1,
            entries: Vec::new(),
        },
    };
    if previous.version != 1 || previous.entries.len() > 16384 {
        bail!("unsupported or oversized MCP ownership state");
    }
    let mut owned = BTreeMap::new();
    for mut entry in previous.entries {
        if !entry.identity.path.is_absolute()
            || entry.identity.table.is_empty()
            || entry.digest.len() != 64
            || !super::model::valid_name(&entry.identity.name)
        {
            bail!("invalid MCP ownership identity");
        }
        entry.identity.path = absolute_path(&entry.identity.path.to_string_lossy())?;
        if owned.insert(entry.identity.clone(), entry).is_some() {
            bail!("duplicate MCP ownership identity");
        }
    }
    Ok(LoadedState {
        bytes: state_bytes,
        owned,
    })
}

fn plan(
    desired: BTreeMap<Identity, (Format, Value)>,
    state_dir: &Path,
    worktree: Option<&WorktreeContext>,
    adopt: bool,
) -> Result<Plan> {
    let state_path = state_dir.join("state.json");
    let LoadedState {
        bytes: state_bytes,
        owned,
    } = load_state(state_dir)?;
    let mut identities: BTreeSet<Identity> = desired.keys().cloned().collect();
    identities.extend(owned.keys().cloned());
    let mut tables = BTreeMap::<PathBuf, BTreeSet<Vec<String>>>::new();
    for identity in &identities {
        tables
            .entry(identity.path.clone())
            .or_default()
            .insert(identity.table.clone());
    }
    for tables in tables.values() {
        let tables: Vec<_> = tables.iter().collect();
        if tables.windows(2).any(|pair| pair[1].starts_with(pair[0])) {
            bail!("MCP table paths in one file must not overlap");
        }
    }
    let mut documents = BTreeMap::<PathBuf, (Format, Option<Vec<u8>>, Document, bool)>::new();
    let mut entries = Vec::new();
    let mut report = Report::default();
    for identity in identities {
        if identity.path.starts_with(state_dir) {
            bail!("MCP target overlaps its ownership state directory");
        }
        if worktree.is_some_and(|ctx| !identity.path.starts_with(&ctx.worktree_root)) {
            bail!("worktree MCP ownership contains a target outside its checkout");
        }
        let wanted = desired.get(&identity);
        let old = owned.get(&identity);
        let format = wanted
            .map(|(f, _)| *f)
            .unwrap_or_else(|| old.expect("identity has an owner").format);
        if !documents.contains_key(&identity.path) {
            let before = transaction::read(&identity.path)?;
            let document = Document::parse(before.as_deref(), format)
                .with_context(|| format!("MCP target {}", identity.path.display()))?;
            documents.insert(identity.path.clone(), (format, before, document, false));
        }
        let (file_format, _, document, changed) = documents
            .get_mut(&identity.path)
            .expect("document was loaded");
        if *file_format != format {
            bail!("one MCP file cannot have two serialization formats");
        }
        let current = document.get(&identity.table, &identity.name)?;
        let owned_unchanged = match (&current, old) {
            (Some(current), Some(old)) => fingerprint(current)? == old.digest,
            _ => false,
        };
        let (status, next) = match (wanted, current.as_ref(), old) {
            (Some((_, wanted)), Some(current), _) if current == wanted && (owned_unchanged || adopt) => {
                (if owned_unchanged { "ok" } else { "adopted" }, Some(wanted))
            }
            (Some((_, wanted)), None, _) => ("created", Some(wanted)),
            (Some((_, wanted)), Some(_), Some(_)) if owned_unchanged => ("updated", Some(wanted)),
            (None, Some(_), Some(_)) if owned_unchanged => ("removed", None),
            (None, None, Some(_)) => ("absent", None),
            _ => bail!("MCP server {} at {} is unmanaged or externally edited; preserve it or use --adopt-mcp with an identical definition", identity.name, identity.path.display()),
        };
        if let Some(value) = next {
            entries.push(Owned {
                identity: identity.clone(),
                format,
                digest: fingerprint(value)?,
            });
        }
        if matches!(status, "created" | "updated" | "removed") {
            document.set(&identity.table, &identity.name, next)?;
            *changed = true;
            report.changed = true;
        }
        report.events.push(Event {
            server: identity.name,
            target: identity.path,
            status,
        });
    }
    for (path, table_paths) in tables {
        let document = &documents[&path].2;
        for table in table_paths {
            let mut namespaces = BTreeSet::new();
            for name in document.names(&table)? {
                if !namespaces.insert(name.replace('-', "_")) {
                    bail!("MCP server names collide after namespace normalization");
                }
            }
        }
    }
    let mut updates = Vec::new();
    for (path, (_, before, document, changed)) in documents {
        if changed {
            let after = document.bytes()?;
            if before.as_ref() != Some(&after) {
                updates.push(Update {
                    mode: transaction::mode(&path)?,
                    path,
                    before: before.map(String::from_utf8).transpose()?,
                    after: String::from_utf8(after)?,
                });
            }
        }
    }
    if entries.len() > 16384 {
        bail!("MCP ownership exceeds 16384 entries");
    }
    let state = State {
        version: 1,
        entries,
    };
    let after = serde_json::to_vec_pretty(&state)?;
    if (!state.entries.is_empty() || state_bytes.is_some()) && state_bytes.as_ref() != Some(&after)
    {
        report.changed = true;
        updates.push(Update {
            path: state_path,
            before: state_bytes.map(String::from_utf8).transpose()?,
            after: String::from_utf8(after)?,
            mode: 0o600,
        });
    }
    Ok(Plan { updates, report })
}
