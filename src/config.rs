use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The same normalized identity selects a download job and its cached checkout.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct GitSource {
    pub repo: String,
    pub git_ref: Option<String>,
}

impl GitSource {
    pub fn new(repo: &str, git_ref: Option<&str>) -> Self {
        Self {
            repo: normalize_repo_url(repo),
            git_ref: git_ref.map(str::to_owned),
        }
    }
}

/// A bundle routes a set of skills into one or more harnesses.
///
/// Skills come from two composable layers:
/// - a whole-bundle `source` (a local or git directory that contains a
///   `skills/` folder), pulled as a unit — point at a bundle and be done;
/// - explicit per-skill entries under `[bundles.<name>.skills.<skill>]`, which
///   add to, or override by name, whatever the bundle source provided.
#[derive(Debug, Clone, Deserialize)]
pub struct Bundle {
    /// Which harnesses to wire this bundle into.
    pub harnesses: Vec<String>,

    /// Optional whole-bundle source. `url` is not valid here — a url is a single
    /// file, not a bundle; use per-skill `url` entries instead.
    #[serde(flatten)]
    pub source: SourceSpec,

    /// Explicit skill sources keyed by installed skill name.
    #[serde(default)]
    pub skills: HashMap<String, SourceSpec>,

    /// Live-link local sources instead of copying them (default). A local
    /// `source` is a working tree you own, so the assembled skill points
    /// straight at it: edits through a harness land in the source and `git pull`
    /// is live with no re-sync. Set `false` to snapshot local sources by copy.
    /// Remote `repo` and `url` sources are always copied — they have no working
    /// tree to link — regardless of this flag.
    #[serde(default = "default_link")]
    pub link: bool,
}

fn default_link() -> bool {
    true
}

/// Raw source fields as written in TOML. Shared by bundles and skills: the same
/// `source` / `repo` / `ref` / `path` / `url` vocabulary resolves the same way
/// whether it points at a whole bundle or a single skill.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SourceSpec {
    /// Local directory.
    #[serde(default)]
    pub source: Option<String>,

    /// Git repository.
    #[serde(default)]
    pub repo: Option<String>,

    /// Branch, tag, or commit to check out for `repo`.
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,

    /// Path within `repo`, relative to its root. Defaults to the repo root.
    #[serde(default)]
    pub path: Option<String>,

    /// HTTP(S) URL to a single `SKILL.md` (skills only).
    #[serde(default)]
    pub url: Option<String>,
}

/// A resolved source: exactly one place content is fetched from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Local directory.
    Local(String),
    /// Git repository, optionally narrowed to `path` at `git_ref`.
    Git {
        repo: String,
        git_ref: Option<String>,
        path: Option<String>,
    },
    /// A single `SKILL.md` fetched over HTTP(S).
    Url(String),
}

/// Why a [`SourceSpec`] could not be resolved to a single [`Source`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceError {
    /// More than one of `source` / `repo` / `url` was set.
    Conflict,
    /// `ref` or `path` was set with no `repo` to apply it to.
    Dangling,
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceError::Conflict => f.write_str(
                "declares more than one source; set exactly one of `source`, `repo`, or `url`",
            ),
            SourceError::Dangling => {
                f.write_str("sets `ref`/`path` with no `repo` source to apply them to")
            }
        }
    }
}

impl std::error::Error for SourceError {}

impl SourceSpec {
    /// True when no source field is set at all.
    pub fn is_empty(&self) -> bool {
        self.source.is_none()
            && self.repo.is_none()
            && self.url.is_none()
            && self.git_ref.is_none()
            && self.path.is_none()
    }

    /// Classify into exactly one [`Source`], or `None` when nothing is declared.
    pub fn resolve(&self) -> std::result::Result<Option<Source>, SourceError> {
        let source_count =
            self.source.is_some() as u8 + self.repo.is_some() as u8 + self.url.is_some() as u8;
        if source_count > 1 {
            return Err(SourceError::Conflict);
        }
        if self.repo.is_none() && (self.git_ref.is_some() || self.path.is_some()) {
            return Err(SourceError::Dangling);
        }

        match (
            self.source.as_deref(),
            self.repo.as_deref(),
            self.url.as_deref(),
        ) {
            (None, None, None) => Ok(None),
            (Some(path), None, None) => Ok(Some(Source::Local(path.to_string()))),
            (None, Some(repo), None) => Ok(Some(Source::Git {
                repo: repo.to_string(),
                git_ref: self.git_ref.clone(),
                path: self.path.clone(),
            })),
            (None, None, Some(url)) => Ok(Some(Source::Url(url.to_string()))),
            _ => unreachable!("source count was validated above"),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub includes: Vec<String>,

    #[serde(default)]
    pub mcp: BTreeMap<String, crate::mcp::Server>,

    /// Bundle definitions keyed by bundle name.
    #[serde(default)]
    pub bundles: HashMap<String, Bundle>,

    #[serde(default)]
    pub harnesses: HashMap<String, Harness>,
}

#[derive(Debug, Deserialize)]
pub struct Harness {
    /// Human-readable display name (defaults to harness key)
    #[serde(default)]
    pub label: Option<String>,

    /// Pattern like "$HOME/.agents/skills/{name}"
    /// {name} is replaced with skill name at runtime
    #[serde(default)]
    pub pattern: Option<String>,

    #[serde(default)]
    pub mcp: Option<crate::mcp::Target>,
}

#[derive(Debug)]
pub struct ProjectConfigFile {
    pub config: Config,
    pub path: PathBuf,
}

/// Resolve environment variables in a string.
/// Supports $VAR and ${VAR} syntax. Unresolvable vars pass through unchanged.
pub fn expand_env_vars(s: &str) -> String {
    expand_variables(s).0
}

fn expand_variables(s: &str) -> (String, bool) {
    let mut result = String::with_capacity(s.len());
    let mut unresolved = false;
    let mut chars = s.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '$' {
            result.push(ch);
            continue;
        }

        match chars.peek().copied() {
            Some('{') => {
                chars.next();
                let mut var_name = String::new();
                let mut closed = false;
                for next in chars.by_ref() {
                    if next == '}' {
                        closed = true;
                        break;
                    }
                    var_name.push(next);
                }

                if closed {
                    if let Some((name, fallback)) = var_name.split_once(":-") {
                        match env::var(name) {
                            Ok(value) => result.push_str(&value),
                            Err(_) => {
                                let (value, missing) = expand_variables(fallback);
                                result.push_str(&value);
                                unresolved |= missing;
                            }
                        }
                    } else if let Ok(value) = env::var(&var_name) {
                        result.push_str(&value);
                    } else {
                        unresolved = true;
                        result.push_str("${");
                        result.push_str(&var_name);
                        result.push('}');
                    }
                } else {
                    unresolved = true;
                    result.push('$');
                    result.push('{');
                    result.push_str(&var_name);
                }
            }
            Some(next) if next.is_alphanumeric() || next == '_' => {
                let mut var_name = String::new();
                while let Some(next) = chars.peek().copied() {
                    if next.is_alphanumeric() || next == '_' {
                        var_name.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                }

                if let Ok(value) = env::var(&var_name) {
                    result.push_str(&value);
                } else {
                    unresolved = true;
                    result.push('$');
                    result.push_str(&var_name);
                }
            }
            _ => result.push('$'),
        }
    }

    (result, unresolved)
}

/// Parse config from a file path. Falls back to default config path if none given.
pub fn parse_config<P: AsRef<Path>>(path: P) -> crate::error::Result<Config> {
    load_config(path.as_ref(), &mut BTreeSet::new(), 0)
}

fn load_config(
    path: &Path,
    seen: &mut BTreeSet<PathBuf>,
    depth: usize,
) -> crate::error::Result<Config> {
    use crate::error::AppError;
    if !path.exists() {
        return Err(crate::error::AppError::ConfigNotFound(
            path.to_string_lossy().to_string(),
        ));
    }

    let real = path.canonicalize()?;
    if depth > 8 || seen.len() >= 32 || !seen.insert(real.clone()) {
        return Err(AppError::InvalidConfig(
            "config includes must be acyclic and contain at most 32 files and 8 levels".into(),
        ));
    }
    if std::fs::metadata(&real)?.len() > 8 * 1024 * 1024 {
        return Err(AppError::InvalidConfig("config exceeds 8 MiB".into()));
    }
    let content = std::fs::read_to_string(&real)?;
    if content.len() > 8 * 1024 * 1024 {
        return Err(AppError::InvalidConfig("config exceeds 8 MiB".into()));
    }
    let mut config: Config = toml::from_str(&content)
        .map_err(|_| AppError::InvalidConfig(format!("cannot parse config {}", real.display())))?;
    let base = real.parent().expect("canonical file has a parent");
    anchor_paths(&mut config, base)?;
    for include in config.includes.clone() {
        let child_path = resolve_source_from(&include, base);
        let child = load_config(&child_path, seen, depth + 1)?;
        for (name, bundle) in child.bundles {
            if config.bundles.insert(name, bundle).is_some() {
                return Err(AppError::InvalidConfig(
                    "duplicate bundle across config includes".into(),
                ));
            }
        }
        for (name, harness) in child.harnesses {
            if config.harnesses.insert(name, harness).is_some() {
                return Err(AppError::InvalidConfig(
                    "duplicate harness across config includes".into(),
                ));
            }
        }
        for (name, server) in child.mcp {
            if config.mcp.insert(name, server).is_some() {
                return Err(AppError::InvalidConfig(
                    "duplicate MCP server across config includes".into(),
                ));
            }
        }
    }
    if config.mcp.len() > 1024 {
        return Err(AppError::InvalidConfig(
            "config contains more than 1024 MCP servers".into(),
        ));
    }
    for (name, server) in &config.mcp {
        if !crate::mcp::model::valid_name(name) {
            return Err(AppError::InvalidConfig(
                "invalid MCP server identity".into(),
            ));
        }
        server
            .validate()
            .map_err(|e| AppError::InvalidConfig(format!("MCP server {name}: {e}")))?;
    }
    for (name, bundle) in &config.bundles {
        validate_component(name).map_err(|e| AppError::InvalidConfig(e.to_string()))?;
        for skill in bundle.skills.keys() {
            validate_component(skill).map_err(|e| AppError::InvalidConfig(e.to_string()))?;
        }
    }
    for harness in config.harnesses.values() {
        if let Some(pattern) = &harness.pattern {
            validate_pattern(pattern).map_err(|e| AppError::InvalidConfig(e.to_string()))?;
        }
        if let Some(target) = &harness.mcp {
            target
                .validate()
                .map_err(|e| AppError::InvalidConfig(e.to_string()))?;
        }
    }
    Ok(config)
}

fn anchor_paths(config: &mut Config, base: &Path) -> crate::error::Result<()> {
    for harness in config.harnesses.values_mut() {
        if let Some(pattern) = &mut harness.pattern {
            *pattern = resolve_source_from(pattern, base)
                .to_string_lossy()
                .into_owned();
        }
        if let Some(target) = &mut harness.mcp {
            target.path = resolve_native_path(&target.path, base)
                .map_err(|e| crate::error::AppError::InvalidConfig(e.to_string()))?;
        }
    }
    for bundle in config.bundles.values_mut() {
        anchor_bundle_paths(bundle, base);
    }
    Ok(())
}

pub(crate) fn validate_component(name: &str) -> anyhow::Result<()> {
    if name.is_empty()
        || name.len() > 255
        || name.starts_with('.')
        || name.contains(['/', '\\', '\0'])
    {
        anyhow::bail!("bundle and skill identities must be single non-hidden path components");
    }
    Ok(())
}

pub(crate) fn validate_pattern(pattern: &str) -> anyhow::Result<()> {
    if !pattern.contains("{name}") {
        anyhow::bail!("skill target pattern requires a {{name}} placeholder");
    }
    Ok(())
}

pub(crate) fn resolve_native_path(path: &str, base: &Path) -> anyhow::Result<String> {
    let (expanded, unresolved) = expand_variables(path);
    if unresolved {
        anyhow::bail!("MCP target has an unresolved environment variable");
    }
    let path = PathBuf::from(expanded);
    Ok(if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
    .to_string_lossy()
    .into_owned())
}

pub(crate) fn anchor_bundle_paths(bundle: &mut Bundle, base: &Path) {
    for source in std::iter::once(&mut bundle.source).chain(bundle.skills.values_mut()) {
        if let Some(path) = &mut source.source {
            *path = resolve_source_from(path, base)
                .to_string_lossy()
                .into_owned();
        }
        if let Some(repo) = &mut source.repo {
            *repo = resolve_repo_from(repo, base);
        }
    }
}

fn normalize_repo_url(repo: &str) -> String {
    let github = repo
        .strip_prefix("gh:")
        .or_else(|| repo.strip_prefix("github:"));
    let mut parts = repo.split('/');
    let shorthand = matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(owner), Some(name), None)
            if !owner.is_empty() && !name.is_empty() && !repo.contains(':')
                && !repo.starts_with('.') && !repo.starts_with('/')
    );
    match github.or_else(|| shorthand.then_some(repo)) {
        Some(path) => format!(
            "git@github.com:{}.git",
            path.strip_suffix(".git").unwrap_or(path)
        ),
        None => repo.into(),
    }
}

/// Discover a project-local config (`uniskill.toml`) in the current directory.
pub fn discover_project_config() -> crate::error::Result<Option<ProjectConfigFile>> {
    let candidate = std::env::current_dir()?.join("uniskill.toml");
    if !candidate.exists() {
        return Ok(None);
    }

    let config = parse_config(&candidate)?;
    Ok(Some(ProjectConfigFile {
        config,
        path: candidate,
    }))
}

/// Resolve a bundle's source path after env var expansion.
pub fn resolve_source(source: &str) -> PathBuf {
    let expanded = expand_env_vars(source);
    if expanded.is_empty() {
        PathBuf::new()
    } else {
        PathBuf::from(expanded)
    }
}

/// Resolve a source path relative to the config file that declared it.
pub fn resolve_source_from(source: &str, base_dir: &Path) -> PathBuf {
    let resolved = resolve_source(source);
    if resolved.as_os_str().is_empty() || resolved.is_absolute() {
        resolved
    } else {
        base_dir.join(resolved)
    }
}

pub fn resolve_repo_from(repo: &str, base_dir: &Path) -> String {
    let expanded = expand_env_vars(repo);
    if is_local_repo_reference(&expanded) {
        let path = PathBuf::from(expanded);
        let resolved = if path.is_absolute() {
            path
        } else {
            base_dir.join(path)
        };
        resolved.to_string_lossy().to_string()
    } else {
        expanded
    }
}

fn is_local_repo_reference(repo: &str) -> bool {
    repo.starts_with('/')
        || repo.starts_with("./")
        || repo.starts_with("../")
        || repo.starts_with("~/")
        || repo.starts_with('$')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_expansion_preserves_literals_and_unresolved_names() {
        let home = env::var("HOME").unwrap();
        for (input, expected) in [
            ("$HOME/skills", format!("{home}/skills")),
            ("żółć/${HOME}/{name}", format!("żółć/{home}/{{name}}")),
            ("$HOME/$HOME", format!("{home}/{home}")),
            ("/literal/{name}/$", "/literal/{name}/$".into()),
            (
                "$UNISKILL_TEST_MISSING/path",
                "$UNISKILL_TEST_MISSING/path".into(),
            ),
        ] {
            assert_eq!(expand_env_vars(input), expected, "{input}");
        }
    }

    #[test]
    fn source_specs_select_one_kind_and_reject_misplaced_git_fields() {
        for (input, expected) in [
            ("", Ok(None)),
            ("source='local'", Ok(Some(Source::Local("local".into())))),
            (
                "url='https://example.com/skill'",
                Ok(Some(Source::Url("https://example.com/skill".into()))),
            ),
            (
                "repo='owner/repo'\nref='main'\npath='skills'",
                Ok(Some(Source::Git {
                    repo: "owner/repo".into(),
                    git_ref: Some("main".into()),
                    path: Some("skills".into()),
                })),
            ),
            (
                "source='local'\nurl='https://example.com/skill'",
                Err(SourceError::Conflict),
            ),
            ("path='skills'", Err(SourceError::Dangling)),
            ("source='local'\npath='skills'", Err(SourceError::Dangling)),
            (
                "url='https://example.com/skill'\nref='main'",
                Err(SourceError::Dangling),
            ),
        ] {
            let spec: SourceSpec = toml::from_str(input).unwrap();
            assert_eq!(spec.resolve(), expected, "{input}");
        }
    }

    #[test]
    fn git_identity_normalizes_repository_aliases_and_preserves_ref_selection() {
        let expected = GitSource::new("git@github.com:owner/repo.git", Some("main"));
        for repo in ["owner/repo", "gh:owner/repo", "github:owner/repo.git"] {
            assert_eq!(GitSource::new(repo, Some("main")), expected);
        }
        assert_eq!(
            GitSource::new("https://example.com/repo.git", None).repo,
            "https://example.com/repo.git"
        );
        assert_ne!(
            GitSource::new("owner/repo", None),
            GitSource::new("owner/repo", Some("HEAD"))
        );
        assert_ne!(
            GitSource::new("owner/repo", Some("main")),
            GitSource::new("owner/repo", Some("other"))
        );
    }

    #[test]
    fn includes_anchor_paths_at_the_declaring_file_and_reject_cycles() {
        let temp = crate::test_support::tempdir().unwrap();
        let root = temp.path().join("config.toml");
        let child = temp.path().join("child/config.toml");
        std::fs::create_dir(child.parent().unwrap()).unwrap();
        std::fs::write(&root, "includes=['child/config.toml']").unwrap();
        std::fs::write(&child, "[harnesses.custom]\npattern='target/{name}'\n[bundles.local]\nharnesses=['custom']\n[bundles.local.skills.demo]\nsource='source'\n").unwrap();
        let config = parse_config(&root).unwrap();
        assert_eq!(
            config.harnesses["custom"].pattern.as_deref(),
            temp.path().join("child/target/{name}").to_str()
        );
        assert_eq!(
            config.bundles["local"].skills["demo"].source.as_deref(),
            temp.path().join("child/source").to_str()
        );
        std::fs::write(&child, "includes=['../config.toml']").unwrap();
        assert!(parse_config(&root).is_err());
    }

    #[test]
    fn includes_reject_duplicate_domain_identities() {
        for fragment in [
            "[bundles.local]\nharnesses=['custom']\nsource='source'",
            "[harnesses.custom]\npattern='target/{name}'",
            "[mcp.docs]\nharnesses=['custom']\ncommand='fixture'",
        ] {
            let temp = crate::test_support::tempdir().unwrap();
            let root = temp.path().join("config.toml");
            std::fs::write(&root, format!("includes=['child.toml']\n{fragment}")).unwrap();
            std::fs::write(temp.path().join("child.toml"), fragment).unwrap();
            assert!(parse_config(&root).is_err(), "{fragment}");
        }
    }

    #[test]
    fn invalid_destination_shapes_are_rejected_by_config_loading() {
        let temp = crate::test_support::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        for text in [
            format!(
                "[bundles.{}]\nharnesses=['custom']\nsource='source'",
                "a".repeat(256)
            ),
            "[harnesses.custom]\npattern='target/static'".into(),
        ] {
            std::fs::write(&path, text).unwrap();
            assert!(parse_config(&path).is_err());
        }
    }
}
