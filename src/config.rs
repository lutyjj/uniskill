use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Bundle {
    /// Path to local bundle root; ignored when skills is present.
    pub source: Option<String>,

    /// Which harnesses to wire this bundle into.
    pub harnesses: Vec<String>,

    /// Remote skill definitions for virtual bundles.
    #[serde(default)]
    pub skills: HashMap<String, SkillEntry>,
}

#[derive(Debug, Deserialize)]
pub struct SkillEntry {
    /// HTTP(S) URL to fetch the skill markdown file.
    pub url: String,
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
                while end < chars.len()
                    && (chars[end].is_alphanumeric() || chars[end] == '_')
                {
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
    let config: Config = toml::from_str(&content)
        .map_err(|e| crate::error::AppError::ConfigParse(e))?;
    Ok(config)
}

/// Discover a project-local config (`uniskill.toml`) in the current directory.
/// Returns `Ok` if found, `Err` if not present (caller falls back to global).
pub fn discover_project_config() -> Option<ProjectConfig> {
    let candidate = std::env::current_dir().ok()?.join("uniskill.toml");
    if !candidate.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&candidate).ok()?;
    let config: ProjectConfig = toml::from_str(&content).ok()?;
    Some(config)
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
source = "/tmp/skills"
harnesses = ["pi"]
"#;
        let file = write_temp_toml(content);
        let config = parse_config(file.path()).unwrap();
        assert_eq!(config.bundles.len(), 1);
        let bundle = config.bundles.get("my-skills").unwrap();
        assert_eq!(bundle.source.as_deref(), Some("/tmp/skills"));
        assert!(bundle.skills.is_empty());
        // No [harnesses] section — empty map; defaults come from harnesses.rs
        assert!(config.harnesses.is_empty());
    }

    #[test]
    fn test_env_var_dollar_syntax() {
        let result = expand_env_vars("$HOME/.agents/skills");
        assert_eq!(result, format!("{}/.agents/skills", env::var("HOME").unwrap()));
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
source = "/tmp/bundle"
harnesses = ["my-harness"]

[harnesses.my-harness]
pattern = "/custom/path/skills/{name}"
"#;
        let file = write_temp_toml(content);
        let config = parse_config(file.path()).unwrap();
        // Only user-defined harnesses are in the parsed map; defaults merge in cli::sync
        assert!(config.harnesses.contains_key("my-harness"));
        assert_eq!(config.harnesses["my-harness"].pattern, "/custom/path/skills/{name}");
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
source = "/tmp/skills"
harnesses = ["pi"]
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
source = "/tmp/bundle"
harnesses = ["custom"]

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
        assert_eq!(resolved, PathBuf::from(format!("{}/.dotfiles/skills", home)));
    }

    #[test]
    fn test_resolve_source_absolute_path_unchanged() {
        let resolved = resolve_source("/absolute/path/to/skills");
        assert_eq!(resolved, PathBuf::from("/absolute/path/to/skills"));
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
source = "./my-skills"
harnesses = ["local-agent"]

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
        assert_eq!(bundle.source.as_deref(), Some("./my-skills"));
        assert!(!config.project_harnesses.is_empty());
        let harness = config.project_harnesses.get("local-agent").unwrap();
        assert_eq!(harness.label, Some("Local Agent".to_string()));
        assert_eq!(harness.pattern, ".agents/skills/{name}");
    }

    #[test]
    fn test_parse_virtual_bundle_from_toml() {
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
        assert!(bundle.source.is_none());
        assert_eq!(bundle.skills.len(), 2);
        assert!(bundle.skills.contains_key("caveman"));
        assert_eq!(bundle.skills["caveman"].url, "https://example.com/skill.md");
    }

    #[test]
    fn test_parse_mixed_bundle_from_toml() {
        let content = r#"
[bundles.mixed-bundle]
source = "/local/path"
harnesses = ["pi"]

[bundles.mixed-bundle.skills.remote-skill]
url = "https://example.com/remote.md"
"#;
        let file = write_temp_toml(content);
        let config = parse_config(file.path()).unwrap();
        let bundle = config.bundles.get("mixed-bundle").unwrap();
        assert_eq!(bundle.source.as_deref(), Some("/local/path"));
        assert_eq!(bundle.skills.len(), 1);
    }
}
