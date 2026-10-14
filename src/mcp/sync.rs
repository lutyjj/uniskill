use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    document::Document,
    transaction::{self, Update},
    Format, Server,
};
use crate::{harnesses::HarnessDef, worktree::WorktreeContext};

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
    pub reservations: crate::filesystem::Reservations,
}

struct Desired {
    entries: BTreeMap<Identity, (Format, Value)>,
    paths: Vec<PathBuf>,
    files: BTreeSet<PathBuf>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    files: Option<BTreeSet<PathBuf>>,
}

struct Plan {
    updates: Vec<Update>,
    report: Report,
}

struct LoadedState {
    bytes: Option<Vec<u8>>,
    owned: BTreeMap<Identity, Owned>,
    files: BTreeSet<PathBuf>,
}

pub fn check(
    servers: &BTreeMap<String, Server>,
    registry: &HashMap<String, HarnessDef>,
    state_dir: &Path,
    worktree: Option<&WorktreeContext>,
    adopt: bool,
    storage: &crate::filesystem::Protection,
) -> Result<Report> {
    let mut resources = reservations(state_dir)?;
    let state_dir = state_directory(state_dir, worktree)?;
    if transaction::pending(&state_dir) {
        bail!("interrupted MCP transaction; run sync to recover before checking");
    }
    let desired = desired_entries(servers, registry, worktree)?;
    resources.entries.extend(desired.paths);
    validate_resources(&resources, storage)?;
    let mut report = plan(desired.entries, desired.files, &state_dir, worktree, adopt)?.report;
    report.reservations = resources;
    Ok(report)
}

pub fn sync(
    servers: &BTreeMap<String, Server>,
    registry: &HashMap<String, HarnessDef>,
    state_dir: &Path,
    worktree: Option<&WorktreeContext>,
    adopt: bool,
    storage: &crate::filesystem::Protection,
) -> Result<Report> {
    let declared_storage = state_dir;
    let state_dir = state_directory(state_dir, worktree)?;
    let desired = desired_entries(servers, registry, worktree)?;
    let mut resources = crate::filesystem::Reservations {
        directories: vec![storage_directory(declared_storage)],
        entries: desired.paths,
    };
    validate_resources(&resources, storage)?;
    if desired.entries.is_empty() && !state_dir.exists() {
        return Ok(Report {
            reservations: resources,
            ..Report::default()
        });
    }
    let _lock = transaction::lock(&state_dir)?;
    let previous = load_state(&state_dir)?;
    let mut paths = desired.files;
    paths.extend(previous.files);
    paths.extend(previous.owned.keys().map(|identity| identity.path.clone()));
    paths.extend(recovery_paths(&state_dir)?);
    resources.entries.extend(paths.iter().cloned());
    validate_resources(&resources, storage)?;
    if let Some(ctx) = worktree {
        for path in &paths {
            ctx.destination(path)?;
        }
    }
    let physical = paths
        .iter()
        .map(|path| crate::filesystem::destination_path(path))
        .collect::<Result<BTreeSet<_>>>()?;
    let _destinations = transaction::lock_destinations(&physical)?;
    let preserved = transaction::recover(&state_dir)?;
    let mut plan = plan(desired.entries, paths, &state_dir, worktree, adopt)?;
    transaction::commit(&state_dir, plan.updates)?;
    plan.report.preserved = preserved;
    plan.report.reservations = resources;
    Ok(plan.report)
}

pub fn storage_directory(base: &Path) -> PathBuf {
    base.join("mcp")
}

/// Read dependencies shared with another scope without reconciling its servers.
pub fn reservations(base: &Path) -> Result<crate::filesystem::Reservations> {
    let state_dir = state_directory(base, None)?;
    let previous = load_state(&state_dir)?;
    let mut paths = previous.files;
    paths.extend(previous.owned.into_keys().map(|identity| identity.path));
    paths.extend(recovery_paths(&state_dir)?);
    Ok(crate::filesystem::Reservations {
        directories: vec![storage_directory(base)],
        entries: paths.into_iter().collect(),
    })
}

fn recovery_paths(state_dir: &Path) -> Result<BTreeSet<PathBuf>> {
    let mut paths = BTreeSet::new();
    for update in transaction::journal_updates(state_dir)? {
        let path = crate::filesystem::destination_path(&update.path)?;
        if path == state_dir.join("state.json") {
            for bytes in [
                update.before.as_deref().map(str::as_bytes),
                Some(update.after.as_bytes()),
            ] {
                let (owned, files) = decode_state(bytes)?;
                paths.extend(files);
                paths.extend(owned.into_keys().map(|identity| identity.path));
            }
        } else {
            paths.insert(path);
        }
    }
    Ok(paths)
}

fn validate_resources(
    resources: &crate::filesystem::Reservations,
    storage: &crate::filesystem::Protection,
) -> Result<()> {
    let mut own = crate::filesystem::Protection::default();
    own.reserve(&crate::filesystem::Reservations {
        directories: resources.directories.clone(),
        entries: Vec::new(),
    })?;
    for path in &resources.entries {
        storage.validate_write(path)?;
        own.validate_write(path)?;
    }
    Ok(())
}

fn state_directory(base: &Path, worktree: Option<&WorktreeContext>) -> Result<PathBuf> {
    let file = storage_directory(base).join("state.json");
    let file = match worktree {
        Some(ctx) => ctx.destination(&file)?,
        None => crate::filesystem::destination_path(&file)?,
    };
    Ok(file.parent().expect("state file has parent").to_path_buf())
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
) -> Result<Desired> {
    let mut desired = BTreeMap::<Identity, (Format, Value)>::new();
    let mut paths = Vec::new();
    let mut files = BTreeSet::new();
    for harness in registry.values() {
        if let Some(target) = &harness.mcp {
            paths.push(PathBuf::from(&target.path));
            let physical = crate::filesystem::destination_path(Path::new(&target.path))?;
            if let Some(ctx) = worktree {
                if let Some(path) = ctx.retarget(&physical.to_string_lossy()) {
                    paths.push(PathBuf::from(&path));
                }
            }
        }
    }
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
            let declared = Path::new(&target.path);
            let path = crate::filesystem::destination_path(declared)?;
            paths.push(declared.to_path_buf());
            let path = match worktree {
                Some(ctx) => match ctx.retarget(&path.to_string_lossy()) {
                    Some(path) => {
                        files.insert(PathBuf::from(&path));
                        paths.push(PathBuf::from(&path));
                        ctx.destination(Path::new(&path))?
                    }
                    None => continue,
                },
                None => {
                    files.insert(declared.to_path_buf());
                    path
                }
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
    paths.sort();
    paths.dedup();
    Ok(Desired {
        entries: desired,
        paths,
        files,
    })
}

fn load_state(state_dir: &Path) -> Result<LoadedState> {
    let bytes = transaction::read(&state_dir.join("state.json"))?;
    let (owned, files) = decode_state(bytes.as_deref())?;
    let active: BTreeSet<_> = owned.keys().map(|identity| &identity.path).collect();
    let mut retained = BTreeSet::new();
    for file in files {
        match std::fs::symlink_metadata(&file) {
            Ok(_) => {
                retained.insert(file);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if active.contains(&crate::filesystem::destination_path(&file)?) {
                    retained.insert(file);
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(LoadedState {
        bytes,
        owned,
        files: retained,
    })
}

type Ownership = BTreeMap<Identity, Owned>;

fn decode_state(bytes: Option<&[u8]>) -> Result<(Ownership, BTreeSet<PathBuf>)> {
    let previous: State = match bytes {
        Some(bytes) => serde_json::from_slice(bytes).map_err(|_| {
            anyhow!("invalid MCP ownership state; refusing to change native configuration")
        })?,
        None => State {
            version: 2,
            entries: Vec::new(),
            files: Some(BTreeSet::new()),
        },
    };
    if !matches!(previous.version, 1 | 2)
        || previous.entries.len() > 16384
        || (previous.version == 2) != previous.files.is_some()
    {
        bail!("unsupported or oversized MCP ownership state");
    }
    let mut files = previous.files.unwrap_or_default();
    if files.len() > 16384 || files.iter().any(|path| !path.is_absolute()) {
        bail!("invalid MCP tracked files");
    }
    let physical_files = files
        .iter()
        .map(|path| crate::filesystem::destination_path(path))
        .collect::<Result<BTreeSet<_>>>()?;
    let mut owned = BTreeMap::new();
    for mut entry in previous.entries {
        if !entry.identity.path.is_absolute()
            || entry.identity.table.is_empty()
            || entry.digest.len() != 64
            || !super::model::valid_name(&entry.identity.name)
        {
            bail!("invalid MCP ownership identity");
        }
        entry.identity.path = crate::filesystem::destination_path(&entry.identity.path)?;
        if previous.version == 1 {
            files.insert(entry.identity.path.clone());
        } else if !physical_files.contains(&entry.identity.path) {
            bail!("MCP ownership refers to an untracked file");
        }
        if owned.insert(entry.identity.clone(), entry).is_some() {
            bail!("duplicate MCP ownership identity");
        }
    }
    Ok((owned, files))
}

fn plan(
    desired: BTreeMap<Identity, (Format, Value)>,
    mut files: BTreeSet<PathBuf>,
    state_dir: &Path,
    worktree: Option<&WorktreeContext>,
    adopt: bool,
) -> Result<Plan> {
    let state_path = state_dir.join("state.json");
    let LoadedState {
        bytes: state_bytes,
        owned,
        files: previous_files,
    } = load_state(state_dir)?;
    files.extend(previous_files);
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
        if let Some(ctx) = worktree {
            ctx.destination(&identity.path)?;
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
    files.extend(entries.iter().map(|entry| entry.identity.path.clone()));
    let mut retained = BTreeSet::new();
    for file in files {
        let physical = crate::filesystem::destination_path(&file)?;
        let exists = match std::fs::symlink_metadata(&file) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if exists || updates.iter().any(|update| update.path == physical) {
            retained.insert(file);
        }
    }
    if retained.len() > 16384 {
        bail!("MCP tracked files exceed 16384 entries");
    }
    let state = State {
        version: 2,
        entries,
        files: Some(retained),
    };
    let after = serde_json::to_vec_pretty(&state)?;
    if (!state.entries.is_empty()
        || state.files.as_ref().is_some_and(|files| !files.is_empty())
        || state_bytes.is_some())
        && state_bytes.as_ref() != Some(&after)
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
