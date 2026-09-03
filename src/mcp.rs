use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
enum McpServer {
    Stdio {
        name: String,
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default = "enabled")]
        enabled: bool,
    },
    Http {
        name: String,
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default = "enabled")]
        enabled: bool,
    },
}

fn enabled() -> bool {
    true
}

pub fn load(path: &Path) -> Result<Vec<Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    let servers: Vec<McpServer> = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not valid MCP configuration", path.display()))?;
    servers.into_iter().filter_map(convert).collect()
}

fn convert(server: McpServer) -> Option<Result<Value>> {
    match server {
        McpServer::Stdio {
            name,
            command,
            args,
            env,
            enabled,
        } if enabled => Some(validate_name(&name).map(|()| {
            json!({
                "name": name,
                "command": command,
                "args": args,
                "env": env.into_iter().map(|(name, value)| json!({"name": name, "value": value})).collect::<Vec<_>>()
            })
        })),
        McpServer::Http {
            name,
            url,
            headers,
            enabled,
        } if enabled => Some(validate_name(&name).and_then(|()| {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                bail!("MCP server {name:?} has an invalid HTTP URL");
            }
            Ok(json!({
                "type": "http",
                "name": name,
                "url": url,
                "headers": headers.into_iter().map(|(name, value)| json!({"name": name, "value": value})).collect::<Vec<_>>()
            }))
        })),
        _ => None,
    }
}

fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        bail!("an MCP server has an empty name");
    }
    Ok(())
}

pub fn template() -> &'static [u8] {
    b"[]\n"
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn converts_enabled_servers_to_acp() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mcps.json");
        fs::write(
            &path,
            r#"[
              {"name":"local","transport":"stdio","command":"tool","args":["serve"],"env":{"A":"B"}},
              {"name":"remote","transport":"http","url":"https://example.com/mcp","enabled":false}
            ]"#,
        )
        .unwrap();
        let value = load(&path).unwrap();
        assert_eq!(value.len(), 1);
        assert_eq!(value[0]["name"], "local");
        assert_eq!(value[0]["env"][0]["name"], "A");
    }
}
