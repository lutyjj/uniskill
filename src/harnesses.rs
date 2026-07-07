use std::collections::HashMap;

use serde::Deserialize;

/// A harness defines where a particular agent expects its skills to live.
#[derive(Debug, Deserialize, Clone)]
pub struct HarnessDef {
    /// Human-readable name for display
    pub label: String,

    /// Pattern with {name} placeholder and $VAR references
    pub pattern: String,
}

/// Built-in harness registry. Users can extend or override via config.
pub fn default_harnesses() -> HashMap<String, HarnessDef> {
    let mut map = HashMap::new();

    map.insert(
        "pi".to_string(),
        HarnessDef {
            label: "Pi".to_string(),
            pattern: "$HOME/.agents/skills/{name}".to_string(),
        },
    );

    map.insert(
        "claude-code".to_string(),
        HarnessDef {
            label: "Claude Code".to_string(),
            pattern: "$HOME/.claude/skills/{name}".to_string(),
        },
    );

    map
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
                pattern: "/custom/agents/skills/{name}".to_string(),
            },
        );
        let pi = registry.get("pi");
        assert!(pi.is_some());
        assert_eq!(pi.unwrap().label, "Pi (custom)");
    }
}
