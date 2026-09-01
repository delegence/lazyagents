use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpSummary {
    Empty,
    Servers(Vec<String>),
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpDefinition {
    pub name: String,
    pub enabled: bool,
    pub transport: McpTransport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransport {
    Stdio(StdioMcp),
    Http(HttpMcp),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioMcp {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, McpValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpMcp {
    pub url: String,
    pub headers: BTreeMap<String, McpValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpValue {
    Literal(String),
    Env(String),
}

pub(crate) fn reject_native_reference_literals(
    definitions: &[McpDefinition],
    harness: &str,
    is_reference: impl Fn(&str) -> bool,
) -> Result<()> {
    for definition in definitions {
        let values = match &definition.transport {
            McpTransport::Stdio(stdio) => &stdio.env,
            McpTransport::Http(http) => &http.headers,
        };
        if let Some((key, McpValue::Literal(value))) = values
            .iter()
            .find(|(_, value)| matches!(value, McpValue::Literal(text) if is_reference(text)))
        {
            anyhow::bail!(
                "{harness} MCP server {} value {key} is a literal that matches native environment-reference syntax: {value}",
                definition.name
            );
        }
    }
    Ok(())
}

impl McpValue {
    pub fn literal(value: impl Into<String>) -> Self {
        Self::Literal(value.into())
    }

    pub fn env(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            anyhow::bail!("environment variable name {name:?} is invalid");
        }
        Ok(Self::Env(name))
    }
}

impl Serialize for McpValue {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Literal(value) => serializer.serialize_str(value),
            Self::Env(name) => {
                let mut map = BTreeMap::new();
                map.insert("env", name);
                map.serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for McpValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum RawValue {
            Literal(String),
            Env { env: String },
        }
        match RawValue::deserialize(deserializer)? {
            RawValue::Literal(value) => Ok(Self::Literal(value)),
            RawValue::Env { env } => McpValue::env(env).map_err(serde::de::Error::custom),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawMcpDefinition {
    name: String,
    #[serde(default = "default_true")]
    enabled: bool,
    transport: Option<String>,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, McpValue>,
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, McpValue>,
}

pub fn read_mcp_definitions(profile_path: &Path) -> Result<Vec<McpDefinition>> {
    let path = profile_path.join("mcps.json");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()))
        }
    };
    parse_mcp_definitions(&text)
        .with_context(|| format!("invalid MCP definitions at {}", path.display()))
}

pub(crate) fn parse_mcp_definitions(text: &str) -> Result<Vec<McpDefinition>> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }

    let definitions: Vec<RawMcpDefinition> =
        serde_json::from_str(text).context("invalid MCP definitions file")?;
    validate_mcp_definitions(definitions)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpValidationError {
    pub path: String,
    pub message: String,
}

pub(crate) fn collect_mcp_validation_errors(text: &str) -> Vec<McpValidationError> {
    if text.trim().is_empty() {
        return Vec::new();
    }

    let value = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => value,
        Err(error) => {
            return vec![McpValidationError {
                path: "mcps.json".to_string(),
                message: format!("invalid MCP definitions file: {error}"),
            }];
        }
    };

    let Some(definitions) = value.as_array() else {
        return vec![McpValidationError {
            path: "mcps.json".to_string(),
            message: "invalid MCP definitions file: expected an array".to_string(),
        }];
    };

    let mut errors = Vec::new();
    let mut names = BTreeSet::new();
    for (index, definition) in definitions.iter().enumerate() {
        let path = format!("mcps.json[{index}]");
        let Some(definition) = definition.as_object() else {
            errors.push(McpValidationError {
                path,
                message: "MCP definition must be an object".to_string(),
            });
            continue;
        };

        let name = definition
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if let Err(error) = validate_mcp_name(name) {
            errors.push(McpValidationError {
                path: path.clone(),
                message: error.to_string(),
            });
        } else if !names.insert(name.to_string()) {
            errors.push(McpValidationError {
                path: path.clone(),
                message: format!("duplicate MCP name {name}"),
            });
        }

        match definition
            .get("transport")
            .and_then(serde_json::Value::as_str)
        {
            Some("stdio") => {
                let command = definition
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim();
                if command.is_empty() {
                    errors.push(McpValidationError {
                        path,
                        message: format!("stdio MCP {name} requires command"),
                    });
                }
            }
            Some("http") => {
                let url = definition
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim();
                if url.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
                    errors.push(McpValidationError {
                        path,
                        message: format!("http MCP {name} requires url"),
                    });
                }
            }
            Some(other) => errors.push(McpValidationError {
                path,
                message: format!("unsupported MCP transport: {other}"),
            }),
            None => errors.push(McpValidationError {
                path,
                message: format!("MCP {name} requires transport"),
            }),
        }
    }

    errors
}

pub fn canonical_mcp_json(definitions: &[McpDefinition]) -> Result<String> {
    #[derive(Serialize)]
    struct CanonicalDefinition<'a> {
        name: &'a str,
        enabled: bool,
        transport: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        command: Option<&'a str>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        args: Vec<&'a str>,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        env: &'a BTreeMap<String, McpValue>,
        #[serde(skip_serializing_if = "Option::is_none")]
        url: Option<&'a str>,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        headers: &'a BTreeMap<String, McpValue>,
    }

    let mut definitions = definitions.iter().collect::<Vec<_>>();
    definitions.sort_by(|left, right| left.name.cmp(&right.name));

    let canonical = definitions
        .into_iter()
        .map(|definition| match &definition.transport {
            McpTransport::Stdio(stdio) => CanonicalDefinition {
                name: &definition.name,
                enabled: definition.enabled,
                transport: "stdio",
                command: Some(&stdio.command),
                args: stdio.args.iter().map(String::as_str).collect(),
                env: &stdio.env,
                url: None,
                headers: empty_headers(),
            },
            McpTransport::Http(http) => CanonicalDefinition {
                name: &definition.name,
                enabled: definition.enabled,
                transport: "http",
                command: None,
                args: Vec::new(),
                env: empty_headers(),
                url: Some(&http.url),
                headers: &http.headers,
            },
        })
        .collect::<Vec<_>>();

    if canonical.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!("{}\n", serde_json::to_string_pretty(&canonical)?))
    }
}

fn empty_headers() -> &'static BTreeMap<String, McpValue> {
    static EMPTY: std::sync::OnceLock<BTreeMap<String, McpValue>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(BTreeMap::new)
}

fn validate_mcp_definitions(definitions: Vec<RawMcpDefinition>) -> Result<Vec<McpDefinition>> {
    let mut names = BTreeSet::new();
    let mut validated = Vec::new();
    for definition in definitions {
        validate_mcp_name(&definition.name)?;
        if !names.insert(definition.name.clone()) {
            anyhow::bail!("duplicate MCP name {}", definition.name);
        }

        let transport = match definition.transport.as_deref() {
            Some("stdio") => McpTransport::Stdio(StdioMcp {
                command: definition
                    .command
                    .map(|command| command.trim().to_string())
                    .filter(|command| !command.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("stdio MCP {} requires command", definition.name)
                    })?,
                args: definition.args,
                env: definition.env,
            }),
            Some("http") => McpTransport::Http(HttpMcp {
                url: definition
                    .url
                    .map(|url| url.trim().to_string())
                    .filter(|url| !url.is_empty())
                    .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
                    .ok_or_else(|| anyhow::anyhow!("http MCP {} requires url", definition.name))?,
                headers: definition.headers,
            }),
            Some(other) => anyhow::bail!("unsupported MCP transport: {other}"),
            None => anyhow::bail!("MCP {} requires transport", definition.name),
        };

        validated.push(McpDefinition {
            name: definition.name,
            enabled: definition.enabled,
            transport,
        });
    }
    Ok(validated)
}

fn validate_mcp_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("MCP name cannot be empty");
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        anyhow::bail!(
            "MCP name {name} may contain only ASCII letters, numbers, dash, and underscore"
        );
    }
    Ok(())
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_empty_invalid_mcp_definitions_fail() {
        let error = parse_mcp_definitions("not json").unwrap_err();
        assert!(error.to_string().contains("invalid MCP definitions file"));
    }

    #[test]
    fn missing_mcp_definitions_are_empty() {
        let temp = tempfile::tempdir().unwrap();
        let definitions = read_mcp_definitions(temp.path()).unwrap();
        assert!(definitions.is_empty());
    }

    #[test]
    fn empty_mcp_definitions_are_empty() {
        let definitions = parse_mcp_definitions(" \n\t").unwrap();
        assert!(definitions.is_empty());
    }

    #[test]
    fn validates_enabled_and_disabled_definitions() {
        let definitions = parse_mcp_definitions(
            r#"[
  {"name":"local","transport":"stdio","command":"server","args":["--flag"],"env":{"TOKEN":"$TOKEN"}},
  {"name":"remote","enabled":false,"transport":"http","url":"https://example.test","headers":{"Authorization":"$TOKEN"}}
]"#,
        )
        .unwrap();

        assert_eq!(definitions.len(), 2);
        assert!(definitions[0].enabled);
        assert!(!definitions[1].enabled);
    }

    #[test]
    fn duplicate_names_fail_even_when_disabled() {
        let error = parse_mcp_definitions(
            r#"[
  {"name":"same","transport":"stdio","command":"server"},
  {"name":"same","enabled":false,"transport":"http","url":"https://example.test"}
]"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("duplicate MCP name same"));
    }

    #[test]
    fn disabled_definitions_require_transport() {
        let error = parse_mcp_definitions(r#"[{"name":"draft","enabled":false}]"#).unwrap_err();
        assert!(error.to_string().contains("MCP draft requires transport"));
    }

    #[test]
    fn disabled_stdio_definitions_require_command() {
        let error =
            parse_mcp_definitions(r#"[{"name":"draft","enabled":false,"transport":"stdio"}]"#)
                .unwrap_err();
        assert!(error
            .to_string()
            .contains("stdio MCP draft requires command"));
    }

    #[test]
    fn http_definitions_require_url() {
        let error = parse_mcp_definitions(r#"[{"name":"remote","transport":"http"}]"#).unwrap_err();
        assert!(error.to_string().contains("http MCP remote requires url"));
    }

    #[test]
    fn stdio_definitions_reject_whitespace_command() {
        let error =
            parse_mcp_definitions(r#"[{"name":"local","transport":"stdio","command":"  "}]"#)
                .unwrap_err();
        assert!(error
            .to_string()
            .contains("stdio MCP local requires command"));
    }

    #[test]
    fn http_definitions_require_http_url() {
        let error =
            parse_mcp_definitions(r#"[{"name":"remote","transport":"http","url":"ftp://bad"}]"#)
                .unwrap_err();
        assert!(error.to_string().contains("http MCP remote requires url"));
    }
}
