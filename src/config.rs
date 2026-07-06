use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Bundle {
    /// Which harnesses to wire this bundle into.
    pub harnesses: Vec<String>,

    /// Skill definitions keyed by installed skill name.
    #[serde(default)]
    pub skills: HashMap<String, SkillEntry>,
}

#[derive(Debug, Deserialize, Default)]
pub struct SkillEntry {
    /// HTTP(S) URL to fetch a single SKILL.md file.
    #[serde(default)]
    pub url: Option<String>,

    /// Local skill directory.
    #[serde(default)]
    pub source: Option<String>,

    /// Git repository containing this skill directory.
    #[serde(default)]
    pub repo: Option<String>,

    /// Branch, tag, or commit to check out for this skill's repository.
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,

    /// Skill directory path, relative to the inherited or explicit repo/source.
    #[serde(default)]
    pub path: Option<String>,
}

impl SkillEntry {
    pub fn source_kind(&self) -> SkillSourceKind {
        match (
            self.url.is_some(),
            self.source.is_some(),
            self.repo.is_some(),
        ) {
            (true, false, false) => SkillSourceKind::Url,
            (false, true, false) => SkillSourceKind::Local,
            (false, false, true) => SkillSourceKind::Git,
            _ => SkillSourceKind::Invalid,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSourceKind {
    Url,
    Local,
    Git,
    Invalid,
}

#[derive(Debug, Deserialize)]
pub struct Config {
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
    pub pattern: String,
}

/// Project-local harness (relative to project root).
#[derive(Debug, Deserialize)]
pub struct LocalHarness {
    /// Human-readable display name (defaults to harness key)
    #[serde(default)]
    pub label: Option<String>,

    /// Relative pattern like ".claude/skills/{name}"
    pub pattern: String,
}

/// Minimal config for project-level `uniskill.toml`.
#[derive(Debug, Deserialize)]
pub struct ProjectConfig {
    /// Bundle definitions keyed by bundle name.
    #[serde(default)]
    pub bundles: HashMap<String, Bundle>,

    /// Local harness definitions with relative patterns;
    /// deserialises from the `[harnesses.XXX]` TOML key to match DESIGN.md.
    #[serde(default, rename = "harnesses")]
    pub project_harnesses: HashMap<String, LocalHarness>,
}

#[derive(Debug)]
pub struct ProjectConfigFile {
    pub config: ProjectConfig,
    pub path: PathBuf,
}

/// Resolve environment variables in a string.
/// Supports $VAR and ${VAR} syntax. Unresolvable vars pass through unchanged.
pub fn expand_env_vars(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '$' && i + 1 < chars.len() {
            if chars[i + 1] == '{' {
                // ${VAR} syntax
                if let Some(end) = s[i + 2..].find('}') {
                    let var_name = &s[i + 2..i + 2 + end];
                    if let Ok(value) = env::var(var_name) {
                        result.push_str(&value);
                    } else {
                        // Leave unresolvable vars as-is
                        result.push_str(&s[i..i + 2 + end + 1]);
                    }
                    i += 3 + end;
                } else {
                    result.push('$');
                    i += 1;
                }
            } else {
                // $VAR syntax — collect alphanumeric/underscore chars
                let start = i + 1;
                let mut end = start;
                while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                    end += 1;
                }
                if end > start {
                    let var_name: String = chars[start..end].iter().collect();
                    if let Ok(value) = env::var(&var_name) {
                        result.push_str(&value);
                    } else {
                        // Leave unresolvable vars as-is
                        result.push('$');
                        result.push_str(&var_name);
                    }
                    i = end;
                } else {
                    result.push('$');
                    i += 1;
                }
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

/// Parse config from a file path. Falls back to default config path if none given.
pub fn parse_config<P: AsRef<Path>>(path: P) -> crate::error::Result<Config> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(crate::error::AppError::ConfigNotFound(
            path.to_string_lossy().to_string(),
        ));
    }

    let content = std::fs::read_to_string(path)?;
    let config: Config = toml::from_str(&content).map_err(crate::error::AppError::ConfigParse)?;
    Ok(config)
}

pub fn parse_project_config<P: AsRef<Path>>(path: P) -> crate::error::Result<ProjectConfig> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(crate::error::AppError::ConfigNotFound(
            path.to_string_lossy().to_string(),
        ));
    }

    let content = std::fs::read_to_string(path)?;
    let config: ProjectConfig =
        toml::from_str(&content).map_err(crate::error::AppError::ConfigParse)?;
    Ok(config)
}

/// Discover a project-local config (`uniskill.toml`) in the current directory.
pub fn discover_project_config() -> crate::error::Result<Option<ProjectConfigFile>> {
    let candidate = std::env::current_dir()?.join("uniskill.toml");
    if !candidate.exists() {
        return Ok(None);
    }

    let config = parse_project_config(&candidate)?;
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
            config.harnesses["my-harness"].pattern,
            "/custom/path/skills/{name}"
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
        assert_eq!(harness.pattern, "/opt/custom/skills/{name}");
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
        // Project-level TOML uses [harnesses.XXX] which deserialises into
        // project_harnesses via serde rename.
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
        // Deserialize as ProjectConfig directly
        let config: ProjectConfig = toml::from_str(&content_bytes).unwrap();
        assert_eq!(config.bundles.len(), 1);
        let bundle = config.bundles.get("local-skills").unwrap();
        assert_eq!(
            bundle.skills["example"].source.as_deref(),
            Some("./my-skills/example")
        );
        assert!(!config.project_harnesses.is_empty());
        let harness = config.project_harnesses.get("local-agent").unwrap();
        assert_eq!(harness.label, Some("Local Agent".to_string()));
        assert_eq!(harness.pattern, ".agents/skills/{name}");
    }

    #[test]
    fn test_parse_project_config_reports_invalid_toml() {
        let file = write_temp_toml("[bundles");
        let result = parse_project_config(file.path());
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
        assert_eq!(skill.source_kind(), SkillSourceKind::Git);
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
            bundle.skills["remote-skill"].source_kind(),
            SkillSourceKind::Url
        );
        assert_eq!(
            bundle.skills["local-skill"].source_kind(),
            SkillSourceKind::Local
        );
    }
}
