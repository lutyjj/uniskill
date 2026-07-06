use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub bundles: Vec<Bundle>,

    #[serde(default = "default_harnesses")]
    pub harnesses: HashMap<String, Harness>,
}

#[derive(Debug, Deserialize)]
pub struct Bundle {
    /// Path to the bundle root (supports env var expansion)
    pub source: String,

    /// Which harnesses to wire this bundle into
    pub harnesses: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Harness {
    /// Pattern like "$HOME/.agents/skills/{name}"
    /// {name} is replaced with skill name at runtime
    pub pattern: String,
}

/// Default harness registry — shipped as built-in data.
/// Users extend this in their config; defaults apply if not overridden.
fn default_harnesses() -> HashMap<String, Harness> {
    let mut h = HashMap::new();
    h.insert(
        "pi".to_string(),
        Harness {
            pattern: "$HOME/.agents/skills/{name}".to_string(),
        },
    );
    h.insert(
        "claude-code".to_string(),
        Harness {
            pattern: "$HOME/.claude/skills/{name}".to_string(),
        },
    );
    h
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

/// Resolve a bundle's source path after env var expansion.
pub fn resolve_source(source: &str) -> PathBuf {
    let expanded = expand_env_vars(source);
    PathBuf::from(expanded)
}

/// Resolve the full installation path for a skill in a given harness.
pub fn resolve_install_path(pattern: &str, skill_name: &str) -> String {
    let expanded = expand_env_vars(pattern);
    expanded.replace("{name}", skill_name)
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
[[bundles]]
source = "/tmp/skills"
harnesses = ["pi"]
"#;
        let file = write_temp_toml(content);
        let config = parse_config(file.path()).unwrap();
        assert_eq!(config.bundles.len(), 1);
        assert_eq!(config.bundles[0].source, "/tmp/skills");
        assert_eq!(config.harnesses.get("pi").unwrap().pattern, "$HOME/.agents/skills/{name}");
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
    fn test_resolve_install_path() {
        let home = env::var("HOME").unwrap();
        let path = resolve_install_path("$HOME/.agents/skills/{name}", "caveman");
        assert_eq!(path, format!("{}/.agents/skills/caveman", home));
    }

    #[test]
    fn test_custom_harness_in_config() {
        let content = r#"
[[bundles]]
source = "/tmp/bundle"
harnesses = ["pi", "my-harness"]

[harnesses.my-harness]
pattern = "/custom/path/skills/{name}"
"#;
        let file = write_temp_toml(content);
        let config = parse_config(file.path()).unwrap();
        // Parse gives only user-defined harnesses; defaults merge in cli::sync
        assert!(config.harnesses.contains_key("my-harness"));
        assert_eq!(config.harnesses["my-harness"].pattern, "/custom/path/skills/{name}");
    }

    #[test]
    fn test_parse_config_missing_file() {
        let result = parse_config("/nonexistent/path.toml");
        assert!(result.is_err());
    }
}
