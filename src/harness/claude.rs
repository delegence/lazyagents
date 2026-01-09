use std::collections::BTreeMap;

use serde_json::Value;

use crate::core::{McpServer, Profile};
use crate::error::{Error, Result};
use crate::harness::{
    load_jsonc_or_empty, mcp_servers_to_json, write_json_pretty, AgentPaths, ApplyReport,
};

pub fn apply(
    _profile: &Profile,
    paths: &AgentPaths,
    mcp_servers: &BTreeMap<String, McpServer>,
    model: Option<&str>,
    _report: &mut ApplyReport,
) -> Result<()> {
    let mut settings = load_jsonc_or_empty(&paths.config_file)?;
    let settings_obj = settings.as_object_mut().ok_or_else(|| {
        Error::InvalidInput(format!(
            "expected JSON object in {}",
            paths.config_file.display()
        ))
    })?;

    if let Some(model) = model {
        settings_obj.insert("model".to_string(), Value::String(model.to_string()));
    }

    let servers = mcp_servers_to_json(mcp_servers);
    settings_obj.insert("mcpServers".to_string(), Value::Object(servers));

    write_json_pretty(&paths.config_file, &settings)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::apply;
    use crate::core::{McpServer, Profile};
    use crate::harness::AgentPaths;

    #[test]
    fn writes_mcp_servers_into_settings_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_file = dir.path().join("settings.local.json");

        let mut mcps = BTreeMap::new();
        let mut server = McpServer::default();
        server.command = Some("run-mcp".to_string());
        mcps.insert("local".to_string(), server);

        let profile = Profile {
            id: "work".to_string(),
            agents: vec!["claude".to_string()],
            skills: Vec::new(),
            commands: Vec::new(),
            mcps: Vec::new(),
            models: BTreeMap::new(),
            extra: BTreeMap::new(),
        };

        let paths = AgentPaths {
            base_dir: dir.path().to_path_buf(),
            rules_file: dir.path().join("CLAUDE.md"),
            skills_dir: dir.path().join("skills"),
            commands_dir: dir.path().join("commands"),
            config_file: config_file.clone(),
        };

        let mut report = crate::harness::ApplyReport::default();
        apply(
            &profile,
            &paths,
            &mcps,
            Some("claude-3-5-sonnet"),
            &mut report,
        )
        .unwrap();

        let contents = std::fs::read_to_string(&config_file).expect("read settings");
        let value: serde_json::Value = serde_json::from_str(&contents).expect("parse settings");
        let obj = value.as_object().expect("root object");
        assert_eq!(
            obj.get("model").and_then(|v| v.as_str()),
            Some("claude-3-5-sonnet")
        );
        let mcp = obj.get("mcpServers").and_then(|v| v.as_object()).unwrap();
        assert!(mcp.contains_key("local"));
    }
}
