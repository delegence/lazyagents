use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

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
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpMcp {
    pub url: String,
    pub headers: BTreeMap<String, String>,
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
    env: BTreeMap<String, String>,
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
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
                    .filter(|url| !url.is_empty())
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
}
