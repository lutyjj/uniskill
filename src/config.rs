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
    use std::io::Write;

    fn write_temp_toml(content: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    #[test]
    fn test_parse_minimal_config() {
        let content = r#"
[bundles.my-skills]
harnesses = ["pi"]

[bundles.my-skills.skills.caveman]
source = "/tmp/skills/caveman"
"#;
        let file = write_temp_toml(content);
        let config = parse_config(file.path()).unwrap();
        assert_eq!(config.bundles.len(), 1);
        let bundle = config.bundles.get("my-skills").unwrap();
        assert_eq!(
            bundle.skills["caveman"].source.as_deref(),
            Some("/tmp/skills/caveman")
        );
        // No [harnesses] section — empty map; defaults come from harnesses.rs
        assert!(config.harnesses.is_empty());
    }

    #[test]
    fn test_env_var_dollar_syntax() {
        let result = expand_env_vars("$HOME/.agents/skills");
        assert_eq!(
            result,
            format!("{}/.agents/skills", env::var("HOME").unwrap())
        );
    }

    #[test]
    fn test_env_var_brace_syntax() {
        let result = expand_env_vars("${HOME}/skills/{name}");
        let home = env::var("HOME").unwrap();
        assert_eq!(result, format!("{}/skills/{{name}}", home));
    }

    #[test]
    fn test_unresolvable_var_passes_through() {
        let result = expand_env_vars("$NONEXISTENT_VAR_PLACEHOLDER/path");
        assert_eq!(result, "$NONEXISTENT_VAR_PLACEHOLDER/path");
    }

    #[test]
    fn test_expand_env_vars_handles_non_ascii_prefix() {
        let home = env::var("HOME").unwrap();
        let result = expand_env_vars("żółć/${HOME}/skills");
        assert_eq!(result, format!("żółć/{home}/skills"));
    }

    #[test]
    fn test_custom_harness_in_config() {
        let content = r#"
[bundles.my-bundle]
harnesses = ["my-harness"]

[bundles.my-bundle.skills.example]
source = "/tmp/bundle/skills/example"

[harnesses.my-harness]
pattern = "/custom/path/skills/{name}"
"#;
        let file = write_temp_toml(content);
        let config = parse_config(file.path()).unwrap();
        // Only user-defined harnesses are in the parsed map; defaults merge in cli::sync
        assert!(config.harnesses.contains_key("my-harness"));
        assert_eq!(
            config.harnesses["my-harness"].pattern.as_deref(),
            Some("/custom/path/skills/{name}")
        );
    }

    #[test]
    fn test_parse_config_missing_file() {
        let result = parse_config("/nonexistent/path.toml");
        assert!(result.is_err());
    }

    #[test]
    fn test_no_harnesses_section_is_empty() {
        // After removing default_harnesses from config.rs, empty config should
        // produce an empty harnesses map — defaults come from harnesses.rs
        let content = r#"
[bundles.my-bundle]
harnesses = ["pi"]

[bundles.my-bundle.skills.example]
source = "/tmp/skills/example"
"#;
        let file = write_temp_toml(content);
        let config = parse_config(file.path()).unwrap();
        // No [harnesses] section means empty map
        assert!(config.harnesses.is_empty());
    }

    #[test]
    fn test_custom_harness_with_label() {
        let content = r#"
[bundles.my-bundle]
harnesses = ["custom"]

[bundles.my-bundle.skills.example]
source = "/tmp/bundle/skills/example"

[harnesses.custom]
label = "My Custom Agent"
pattern = "/opt/custom/skills/{name}"
"#;
        let file = write_temp_toml(content);
        let config = parse_config(file.path()).unwrap();
        let harness = config.harnesses.get("custom").unwrap();
        assert_eq!(harness.label, Some("My Custom Agent".to_string()));
        assert_eq!(
            harness.pattern.as_deref(),
            Some("/opt/custom/skills/{name}")
        );
    }

    #[test]
    fn test_resolve_source_expands_env_vars() {
        let home = env::var("HOME").unwrap();
        let resolved = resolve_source("$HOME/.dotfiles/skills");
        assert_eq!(
            resolved,
            PathBuf::from(format!("{}/.dotfiles/skills", home))
        );
    }

    #[test]
    fn test_resolve_source_absolute_path_unchanged() {
        let resolved = resolve_source("/absolute/path/to/skills");
        assert_eq!(resolved, PathBuf::from("/absolute/path/to/skills"));
    }

    #[test]
    fn test_resolve_source_from_joins_relative_path_to_config_dir() {
        let resolved = resolve_source_from("./skills", Path::new("/repo"));
        assert_eq!(resolved, PathBuf::from("/repo/./skills"));
    }

    #[test]
    fn test_resolve_source_from_keeps_absolute_path() {
        let resolved = resolve_source_from("/skills", Path::new("/repo"));
        assert_eq!(resolved, PathBuf::from("/skills"));
    }

    #[test]
    fn test_resolve_repo_from_keeps_github_shorthand() {
        let resolved = resolve_repo_from("lutyjj/agent-skills", Path::new("/repo"));
        assert_eq!(resolved, "lutyjj/agent-skills");
    }

    #[test]
    fn test_resolve_repo_from_joins_relative_local_repo() {
        let resolved = resolve_repo_from("../skills-repo", Path::new("/repo/config"));
        assert_eq!(resolved, "/repo/config/../skills-repo");
    }

    #[test]
    fn test_expand_env_vars_multiple_vars() {
        let home = env::var("HOME").unwrap();
        let result = expand_env_vars("$HOME/.agents/$USER");
        // USER might not be set — it passes through if unresolvable
        assert!(result.starts_with(&home));
    }

    #[test]
    fn test_expand_env_vars_no_vars() {
        let result = expand_env_vars("/static/path/to/skills/{name}");
        assert_eq!(result, "/static/path/to/skills/{name}");
    }

    #[test]
    fn test_project_config_with_harnesses_key() {
        // Global and project files share the same config shape.
        let content = r#"
[bundles.local-skills]
harnesses = ["local-agent"]

[bundles.local-skills.skills.example]
source = "./my-skills/example"

[harnesses.local-agent]
label = "Local Agent"
pattern = ".agents/skills/{name}"
"#;
        let file = write_temp_toml(content);
        let content_bytes = std::fs::read_to_string(file.path()).unwrap();
        // Deserialize the unresolved wire shape directly.
        let config: Config = toml::from_str(&content_bytes).unwrap();
        assert_eq!(config.bundles.len(), 1);
        let bundle = config.bundles.get("local-skills").unwrap();
        assert_eq!(
            bundle.skills["example"].source.as_deref(),
            Some("./my-skills/example")
        );
        assert!(!config.harnesses.is_empty());
        let harness = config.harnesses.get("local-agent").unwrap();
        assert_eq!(harness.label, Some("Local Agent".to_string()));
        assert_eq!(harness.pattern.as_deref(), Some(".agents/skills/{name}"));
    }

    #[test]
    fn test_parse_project_config_reports_invalid_toml() {
        let file = write_temp_toml("[bundles");
        let result = parse_config(file.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_url_skill_from_toml() {
        let content = r#"
[bundles.important-stuff]
harnesses = ["pi"]

[bundles.important-stuff.skills.caveman]
url = "https://example.com/skill.md"

[bundles.important-stuff.skills.code-design]
url = "https://example.com/code-design.md"
"#;
        let file = write_temp_toml(content);
        let config = parse_config(file.path()).unwrap();
        assert_eq!(config.bundles.len(), 1);
        let bundle = config.bundles.get("important-stuff").unwrap();
        assert_eq!(bundle.skills.len(), 2);
        assert!(bundle.skills.contains_key("caveman"));
        assert_eq!(
            bundle.skills["caveman"].url.as_deref(),
            Some("https://example.com/skill.md")
        );
    }

    #[test]
    fn test_parse_git_skill_from_toml() {
        let content = r#"
[bundles.generic]
harnesses = ["pi", "claude-code"]

[bundles.generic.skills.code-design]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic/skills/code-design"
"#;
        let file = write_temp_toml(content);
        let config = parse_config(file.path()).unwrap();
        let bundle = config.bundles.get("generic").unwrap();
        let skill = &bundle.skills["code-design"];
        assert_eq!(skill.repo.as_deref(), Some("gh:lutyjj/agent-skills"));
        assert_eq!(skill.git_ref.as_deref(), Some("main"));
        assert_eq!(
            skill.path.as_deref(),
            Some("bundles/generic/skills/code-design")
        );
        assert_eq!(bundle.harnesses, vec!["pi", "claude-code"]);
        assert_eq!(
            skill.resolve().unwrap(),
            Some(Source::Git {
                repo: "gh:lutyjj/agent-skills".to_string(),
                git_ref: Some("main".to_string()),
                path: Some("bundles/generic/skills/code-design".to_string()),
            })
        );
    }

    #[test]
    fn test_parse_mixed_skill_sources_from_toml() {
        let content = r#"
[bundles.mixed-bundle]
harnesses = ["pi"]

[bundles.mixed-bundle.skills.remote-skill]
url = "https://example.com/remote.md"

[bundles.mixed-bundle.skills.local-skill]
source = "/local/path/skills/local-skill"
"#;
        let file = write_temp_toml(content);
        let config = parse_config(file.path()).unwrap();
        let bundle = config.bundles.get("mixed-bundle").unwrap();
        assert_eq!(bundle.skills.len(), 2);
        assert_eq!(
            bundle.skills["remote-skill"].resolve().unwrap(),
            Some(Source::Url("https://example.com/remote.md".to_string()))
        );
        assert_eq!(
            bundle.skills["local-skill"].resolve().unwrap(),
            Some(Source::Local("/local/path/skills/local-skill".to_string()))
        );
    }

    #[test]
    fn test_parse_whole_bundle_git_source() {
        // A bundle can point at a whole remote bundle directory as a unit,
        // with no explicit skill entries at all.
        let content = r#"
[bundles.generic]
harnesses = ["pi", "claude-code"]
repo = "gh:lutyjj/agent-skills"
ref = "main"
path = "bundles/generic"
"#;
        let file = write_temp_toml(content);
        let config = parse_config(file.path()).unwrap();
        let bundle = config.bundles.get("generic").unwrap();
        assert!(bundle.skills.is_empty());
        assert!(!bundle.source.is_empty());
        assert_eq!(
            bundle.source.resolve().unwrap(),
            Some(Source::Git {
                repo: "gh:lutyjj/agent-skills".to_string(),
                git_ref: Some("main".to_string()),
                path: Some("bundles/generic".to_string()),
            })
        );
    }

    #[test]
    fn test_bundle_source_and_skills_compose() {
        // A whole-bundle source and explicit skills coexist: pull the bundle,
        // then layer an extra url skill on top.
        let content = r#"
[bundles.generic]
harnesses = ["pi"]
repo = "gh:lutyjj/agent-skills"
path = "bundles/generic"

[bundles.generic.skills.caveman]
url = "https://example.com/caveman.md"
"#;
        let file = write_temp_toml(content);
        let config = parse_config(file.path()).unwrap();
        let bundle = config.bundles.get("generic").unwrap();
        assert!(matches!(
            bundle.source.resolve().unwrap(),
            Some(Source::Git { .. })
        ));
        assert_eq!(
            bundle.skills["caveman"].resolve().unwrap(),
            Some(Source::Url("https://example.com/caveman.md".to_string()))
        );
    }

    #[test]
    fn test_bundle_link_defaults_true() {
        let content = r#"
[bundles.generic]
harnesses = ["pi"]
source = "../bundles/generic"
"#;
        let file = write_temp_toml(content);
        let config = parse_config(file.path()).unwrap();
        assert!(config.bundles.get("generic").unwrap().link);
    }

    #[test]
    fn test_bundle_link_can_be_disabled() {
        let content = r#"
[bundles.generic]
harnesses = ["pi"]
source = "../bundles/generic"
link = false
"#;
        let file = write_temp_toml(content);
        let config = parse_config(file.path()).unwrap();
        assert!(!config.bundles.get("generic").unwrap().link);
    }

    #[test]
    fn test_source_spec_empty_resolves_to_none() {
        let spec = SourceSpec::default();
        assert!(spec.is_empty());
        assert_eq!(spec.resolve().unwrap(), None);
    }

    #[test]
    fn test_source_spec_conflict_is_rejected() {
        let spec = SourceSpec {
            source: Some("/tmp/skill".to_string()),
            url: Some("https://example.com/skill.md".to_string()),
            ..SourceSpec::default()
        };
        assert_eq!(spec.resolve(), Err(SourceError::Conflict));
    }

    #[test]
    fn test_source_spec_dangling_ref_is_rejected() {
        let spec = SourceSpec {
            path: Some("bundles/generic".to_string()),
            ..SourceSpec::default()
        };
        assert!(!spec.is_empty());
        assert_eq!(spec.resolve(), Err(SourceError::Dangling));
    }

    #[test]
    fn test_source_spec_rejects_path_on_local_source() {
        let spec = SourceSpec {
            source: Some("./skills".to_string()),
            path: Some("ignored".to_string()),
            ..SourceSpec::default()
        };
        assert_eq!(spec.resolve(), Err(SourceError::Dangling));
    }

    #[test]
    fn test_source_spec_rejects_ref_on_url_source() {
        let spec = SourceSpec {
            url: Some("https://example.com/SKILL.md".to_string()),
            git_ref: Some("main".to_string()),
            ..SourceSpec::default()
        };
        assert_eq!(spec.resolve(), Err(SourceError::Dangling));
    }
}
