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
pub fn default_harnesses() -> anyhow::Result<HashMap<String, HarnessDef>> {
    let mut registry: HashMap<String, HarnessDef> = toml::from_str(include_str!("harnesses.toml"))?;
    let base = std::env::current_dir()?;
    for harness in registry.values_mut() {
        if let Some(pattern) = &mut harness.pattern {
            *pattern = crate::config::resolve_source_from(pattern, &base)
                .to_string_lossy()
                .into_owned();
        }
        if let Some(target) = &mut harness.mcp {
            target.path = crate::config::resolve_native_path(&target.path, &base)?;
        }
    }
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_registry_has_known_harnesses() {
        let registry = default_harnesses().unwrap();
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
