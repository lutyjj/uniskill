use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Json,
    Toml,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum References {
    #[default]
    Interpolate,
    Fields,
}

/// A native configuration boundary. Harness identities never select engine behavior.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub path: String,
    pub format: Format,
    pub table: Vec<String>,
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
    #[serde(default)]
    pub omit: BTreeSet<String>,
    #[serde(default)]
    pub references: References,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub harnesses: Vec<String>,
    pub command: Option<String>,
    pub url: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub env_vars: Vec<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub env_headers: BTreeMap<String, String>,
    pub bearer_token_env_var: Option<String>,
    /// Native policy fields, keyed by the stable configured harness identity.
    #[serde(default)]
    pub options: BTreeMap<String, toml::Table>,
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_variable(name: &str) -> bool {
    name.len() <= 256
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

const CONNECTION_FIELDS: &[&str] = &[
    "type",
    "command",
    "url",
    "args",
    "env",
    "env_vars",
    "headers",
    "env_headers",
    "bearer_token_env_var",
];

impl Target {
    pub fn validate(&self) -> Result<()> {
        if self.path.is_empty() || self.path.contains("{name}") || self.path.len() > 4096 {
            bail!("MCP target requires a file path without placeholders");
        }
        if self.table.is_empty()
            || self.table.len() > 32
            || self.table.iter().any(|s| s.is_empty() || s.len() > 256)
        {
            bail!("MCP target requires a nonempty table key path");
        }
        let mut names = BTreeSet::new();
        for (source, destination) in &self.fields {
            if !CONNECTION_FIELDS.contains(&source.as_str())
                || destination.is_empty()
                || destination.len() > 256
                || !names.insert(destination)
            {
                bail!("MCP field mappings require known source fields and unique nonempty destination names");
            }
        }
        if self
            .omit
            .iter()
            .any(|field| !CONNECTION_FIELDS.contains(&field.as_str()))
        {
            bail!("MCP omissions require known connection fields");
        }
        Ok(())
    }
}

impl Server {
    pub fn validate(&self) -> Result<()> {
        if self.harnesses.is_empty()
            || self.harnesses.len() > 128
            || self.harnesses.iter().any(|s| !valid_name(s))
        {
            bail!("MCP server requires valid harness identities");
        }
        if self.command.is_some() == self.url.is_some() {
            bail!("MCP server requires exactly one of command or url");
        }
        if self
            .command
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 4096)
            || self.url.as_ref().is_some_and(|s| {
                !(s.starts_with("http://") || s.starts_with("https://")) || s.len() > 16384
            })
        {
            bail!("MCP server has an invalid command or HTTP URL");
        }
        if self.command.is_some()
            && (!self.headers.is_empty()
                || !self.env_headers.is_empty()
                || self.bearer_token_env_var.is_some())
        {
            bail!("HTTP authentication fields require a URL server");
        }
        if self.url.is_some()
            && (!self.args.is_empty() || !self.env.is_empty() || !self.env_vars.is_empty())
        {
            bail!("stdio fields require a command server");
        }
        if self.args.len() > 1024
            || self.env.len() > 1024
            || self.headers.len() > 1024
            || self.env_headers.len() > 1024
            || self.env_vars.len() > 1024
        {
            bail!("too many MCP arguments, variables, or headers");
        }
        if self
            .env_vars
            .iter()
            .chain(self.env_headers.values())
            .chain(self.bearer_token_env_var.iter())
            .any(|s| !valid_variable(s))
        {
            bail!("invalid MCP environment reference name");
        }
        if self.env_vars.iter().any(|k| self.env.contains_key(k))
            || self
                .env_headers
                .keys()
                .any(|k| self.headers.keys().any(|h| h.eq_ignore_ascii_case(k)))
            || self.bearer_token_env_var.is_some()
                && self
                    .headers
                    .keys()
                    .chain(self.env_headers.keys())
                    .any(|k| k.eq_ignore_ascii_case("Authorization"))
        {
            bail!("MCP literal values and environment references overlap");
        }
        for (harness, options) in &self.options {
            if !self.harnesses.contains(harness)
                || options
                    .keys()
                    .any(|k| CONNECTION_FIELDS.contains(&k.as_str()))
            {
                bail!("MCP options must belong to a routed harness and cannot replace connection fields");
            }
        }
        Ok(())
    }

    pub fn render(&self, target: &Target, harness: &str) -> Result<Value> {
        self.validate()?;
        target.validate()?;
        let mut values = Map::new();
        if let Some(command) = &self.command {
            values.insert("command".into(), Value::from(command.clone()));
            if !self.args.is_empty() {
                values.insert("args".into(), serde_json::to_value(&self.args)?);
            }
        } else if let Some(url) = &self.url {
            values.insert("type".into(), Value::from("http"));
            values.insert("url".into(), Value::from(url.clone()));
        }
        let mut env = self.env.clone();
        let mut headers = self.headers.clone();
        match target.references {
            References::Interpolate => {
                for name in &self.env_vars {
                    env.insert(name.clone(), format!("${{{name}}}"));
                }
                for (header, name) in &self.env_headers {
                    headers.insert(header.clone(), format!("${{{name}}}"));
                }
                if let Some(name) = &self.bearer_token_env_var {
                    headers.insert("Authorization".into(), format!("Bearer ${{{name}}}"));
                }
            }
            References::Fields => {
                if !self.env_vars.is_empty() {
                    values.insert("env_vars".into(), serde_json::to_value(&self.env_vars)?);
                }
                if !self.env_headers.is_empty() {
                    values.insert(
                        "env_headers".into(),
                        serde_json::to_value(&self.env_headers)?,
                    );
                }
                if let Some(name) = &self.bearer_token_env_var {
                    values.insert("bearer_token_env_var".into(), Value::from(name.clone()));
                }
                if env
                    .values()
                    .chain(headers.values())
                    .any(|v| v.contains("${"))
                {
                    bail!("this target requires explicit environment reference fields");
                }
            }
        }
        if !env.is_empty() {
            values.insert("env".into(), serde_json::to_value(env)?);
        }
        if !headers.is_empty() {
            values.insert("headers".into(), serde_json::to_value(headers)?);
        }
        let mut rendered = Map::new();
        for (key, value) in values {
            if target.omit.contains(&key) {
                if key != "type" {
                    bail!("MCP target cannot omit a populated connection field: {key}");
                }
                continue;
            }
            let destination = target.fields.get(&key).unwrap_or(&key);
            if rendered.insert(destination.clone(), value).is_some() {
                bail!("MCP field mappings collide");
            }
        }
        if let Some(options) = self.options.get(harness) {
            let options = serde_json::to_value(options)?;
            for (key, value) in options.as_object().expect("TOML table is an object") {
                if rendered.insert(key.clone(), value.clone()).is_some() {
                    bail!("native MCP options overlap rendered connection fields");
                }
            }
        }
        Ok(Value::Object(rendered))
    }
}
