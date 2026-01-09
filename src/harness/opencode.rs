use std::collections::BTreeMap;

use serde_json::{Map, Value};

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
    let mut root = load_jsonc_or_empty(&paths.config_file)?;
    let root_obj = root.as_object_mut().ok_or_else(|| {
        Error::InvalidInput(format!(
            "expected JSON object in {}",
            paths.config_file.display()
        ))
    })?;

    let mcp_map = mcp_servers_to_json(mcp_servers);
    root_obj.insert("mcp".to_string(), Value::Object(mcp_map));

    if let Some(model) = model {
        if let Some(agent_value) = root_obj.get_mut("agent") {
            if let Some(agent_obj) = agent_value.as_object_mut() {
                let general_value = agent_obj
                    .entry("general".to_string())
                    .or_insert_with(|| Value::Object(Map::new()));
                if let Some(general_obj) = general_value.as_object_mut() {
                    general_obj.insert("model".to_string(), Value::String(model.to_string()));
                } else {
                    *general_value = Value::Object({
                        let mut map = Map::new();
                        map.insert("model".to_string(), Value::String(model.to_string()));
                        map
                    });
                }
            } else {
                root_obj.insert("model".to_string(), Value::String(model.to_string()));
            }
        } else {
            root_obj.insert("model".to_string(), Value::String(model.to_string()));
        }
    }

    write_json_pretty(&paths.config_file, &root)
}
