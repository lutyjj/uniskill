use std::collections::HashMap;

use serde::Deserialize;

/// A harness defines where a particular agent expects its skills to live.
#[derive(Debug, Deserialize, Clone)]
pub struct HarnessDef {
    /// Human-readable name for display
    pub label: String,

    /// Pattern with {name} placeholder and $VAR references
    pub pattern: Option<String>,

    pub mcp: Option<crate::mcp::Target>,
}

/// Built-in harness registry. Users can extend or override via config.
fn default_harnesses() -> HashMap<String, HarnessDef> {
    toml::from_str(include_str!("harnesses.toml")).expect("built-in harness definitions are valid")
}

/// Merge user-defined global harnesses over the built-in defaults.
pub fn registry(config: &crate::config::Config) -> anyhow::Result<HashMap<String, HarnessDef>> {
    let mut registry = default_harnesses();
    let base = std::env::current_dir()?;
    for (name, harness) in &mut registry {
        let custom = config.harnesses.get(name);
        let skills_used = config
            .bundles
            .values()
            .any(|bundle| bundle.harnesses.contains(name));
        let mcp_used = config
            .mcp
            .values()
            .any(|server| server.harnesses.contains(name));
        if skills_used && custom.and_then(|h| h.pattern.as_ref()).is_none() {
            if let Some(pattern) = &mut harness.pattern {
                *pattern = crate::config::resolve_source_from(pattern, &base)
                    .to_string_lossy()
                    .into_owned();
            }
        }
        if mcp_used && custom.and_then(|h| h.mcp.as_ref()).is_none() {
            if let Some(target) = &mut harness.mcp {
                target.path = crate::config::resolve_native_path(&target.path, &base)?;
            }
        }
    }
    for (name, harness) in &config.harnesses {
        let target = registry.entry(name.clone()).or_insert_with(|| HarnessDef {
            label: name.clone(),
            pattern: None,
            mcp: None,
        });
        if let Some(label) = &harness.label {
            target.label = label.clone();
        }
        if let Some(pattern) = &harness.pattern {
            target.pattern = Some(pattern.clone());
        }
        if let Some(mcp) = &harness.mcp {
            target.mcp = Some(mcp.clone());
        }
    }
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_registry_has_known_harnesses() {
        let registry = default_harnesses();
        assert!(registry.contains_key("pi"));
        assert!(registry.contains_key("claude-code"));
    }

    #[test]
    fn test_custom_registry_override() {
        let mut registry = HashMap::new();
        registry.insert(
            "pi".to_string(),
            HarnessDef {
                label: "Pi (custom)".to_string(),
                pattern: Some("/custom/agents/skills/{name}".to_string()),
                mcp: None,
            },
        );
        let pi = registry.get("pi");
        assert!(pi.is_some());
        assert_eq!(pi.unwrap().label, "Pi (custom)");
    }
}
