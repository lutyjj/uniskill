use std::borrow::Cow;

use anyhow::{anyhow, bail, Result};
use serde_json::{Map, Value};
use toml_edit::{DocumentMut, Item, Table};

use super::Format;

pub enum Document {
    Json(Value),
    Toml(DocumentMut),
}

impl Document {
    pub fn parse(bytes: Option<&[u8]>, format: Format) -> Result<Self> {
        match format {
            Format::Json => {
                let value = match bytes {
                    Some(bytes) => serde_json::from_slice(bytes)
                        .map_err(|_| anyhow!("invalid native JSON configuration"))?,
                    None => Value::Object(Map::new()),
                };
                if !value.is_object() {
                    bail!("native JSON configuration must be an object");
                }
                Ok(Self::Json(value))
            }
            Format::Toml => {
                let text = std::str::from_utf8(bytes.unwrap_or_default())
                    .map_err(|_| anyhow!("native TOML configuration is not UTF-8"))?;
                let document = text
                    .parse::<DocumentMut>()
                    .map_err(|_| anyhow!("invalid native TOML configuration"))?;
                Ok(Self::Toml(document))
            }
        }
    }

    fn root(&self) -> Result<Cow<'_, Value>> {
        Ok(match self {
            Self::Json(value) => Cow::Borrowed(value),
            Self::Toml(document) => Cow::Owned(
                toml_edit::de::from_document::<Value>(document.clone())
                    .map_err(|_| anyhow!("cannot read native TOML server definitions"))?,
            ),
        })
    }

    fn collection<'a>(root: &'a Value, keys: &[String]) -> Result<Option<&'a Map<String, Value>>> {
        let mut node = root;
        for key in keys {
            let table = node
                .as_object()
                .ok_or_else(|| anyhow!("MCP table key traverses a non-object value"))?;
            match table.get(key) {
                Some(value) => node = value,
                None => return Ok(None),
            }
        }
        Ok(Some(node.as_object().ok_or_else(|| {
            anyhow!("native MCP server collection must be an object")
        })?))
    }

    pub fn names(&self, keys: &[String]) -> Result<Vec<String>> {
        let root = self.root()?;
        Ok(Self::collection(&root, keys)?
            .map(|table| table.keys().cloned().collect())
            .unwrap_or_default())
    }

    pub fn get(&self, keys: &[String], name: &str) -> Result<Option<Value>> {
        let root = self.root()?;
        let Some(table) = Self::collection(&root, keys)? else {
            return Ok(None);
        };
        if table.get(name).is_some_and(|v| !v.is_object()) {
            bail!("native MCP server entry must be an object");
        }
        Ok(table.get(name).cloned())
    }

    pub fn set(&mut self, keys: &[String], name: &str, value: Option<&Value>) -> Result<()> {
        match self {
            Self::Json(root) => {
                let mut node = root;
                for key in keys {
                    let table = node
                        .as_object_mut()
                        .ok_or_else(|| anyhow!("MCP table key traverses a non-object value"))?;
                    if !table.contains_key(key) {
                        if value.is_none() {
                            return Ok(());
                        }
                        table.insert(key.clone(), Value::Object(Map::new()));
                    }
                    node = table.get_mut(key).expect("key exists");
                }
                let table = node
                    .as_object_mut()
                    .ok_or_else(|| anyhow!("native MCP server collection must be an object"))?;
                match value {
                    Some(value) => {
                        table.insert(name.into(), value.clone());
                    }
                    None => {
                        table.remove(name);
                    }
                }
            }
            Self::Toml(document) => {
                let mut node = document.as_item_mut();
                for key in keys {
                    let table = node
                        .as_table_like_mut()
                        .ok_or_else(|| anyhow!("MCP table key traverses a non-table value"))?;
                    if !table.contains_key(key) {
                        if value.is_none() {
                            return Ok(());
                        }
                        let mut child = Table::new();
                        child.set_implicit(true);
                        table.insert(key, Item::Table(child));
                    }
                    node = table.get_mut(key).expect("key exists");
                }
                let table = node
                    .as_table_like_mut()
                    .ok_or_else(|| anyhow!("native MCP server collection must be a table"))?;
                match value {
                    Some(value) => {
                        let value = to_toml(value)?;
                        let child = toml_edit::ser::to_document(&value).map_err(|_| {
                            anyhow!("MCP entry contains values unsupported by TOML")
                        })?;
                        table.insert(name, Item::Table(child.as_table().clone()));
                    }
                    None => {
                        table.remove(name);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn bytes(&self) -> Result<Vec<u8>> {
        match self {
            Self::Json(root) => {
                let mut bytes = serde_json::to_vec_pretty(root)?;
                bytes.push(b'\n');
                Ok(bytes)
            }
            Self::Toml(document) => Ok(document.to_string().into_bytes()),
        }
    }
}

fn to_toml(value: &Value) -> Result<toml::Value> {
    Ok(match value {
        Value::Null => bail!("null is unsupported by native TOML configuration"),
        Value::Bool(v) => toml::Value::Boolean(*v),
        Value::String(v) => toml::Value::String(v.clone()),
        Value::Number(v) => {
            if let Some(integer) = v.as_i64() {
                toml::Value::Integer(integer)
            } else if v.is_f64() {
                toml::Value::Float(
                    v.as_f64()
                        .ok_or_else(|| anyhow!("number is unsupported by TOML"))?,
                )
            } else {
                bail!("integer is outside TOML's signed 64-bit range");
            }
        }
        Value::Array(v) => toml::Value::Array(v.iter().map(to_toml).collect::<Result<_>>()?),
        Value::Object(v) => toml::Value::Table(
            v.iter()
                .map(|(k, v)| Ok((k.clone(), to_toml(v)?)))
                .collect::<Result<_>>()?,
        ),
    })
}
