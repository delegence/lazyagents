use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde_json::{json, Map, Value};

use crate::harness::agents::{
    harness_scoped_value, remove_string, remove_value, select_harness_value,
    split_markdown_frontmatter, RenderedAgent, SubAgent,
};
use crate::harness::artifact::{
    CommandMode, CommandsDirectory, HarnessArtifact, InstructionFile, JsonConfigFile, McpCodec,
    McpConfig, NativeConfig, PreferenceBinding, PreferenceCodec, PreferenceKind,
    SettingsPreferences, SkillsDirectory, SubagentCodec, SubagentsDirectory,
};
use crate::harness::integration::{
    AppEnvironment, HarnessConfigPaths, HarnessIntegration, ImportedPreference, ProfileRef,
};
use crate::harness::kind::HarnessKind;
use crate::profile::mcp::{McpDefinition, McpTransport};

pub struct ClaudeIntegration;

impl HarnessIntegration for ClaudeIntegration {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Claude
    }

    fn default_config_dir(&self, env: &AppEnvironment) -> std::path::PathBuf {
        env.user_home.join(".claude")
    }

    fn paths_from_config_dir(&self, config_dir: std::path::PathBuf) -> Result<HarnessConfigPaths> {
        let mcp_file_name = config_dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| format!("{name}.json"))
            .unwrap_or_else(|| ".claude.json".to_string());
        let mcp_file = config_dir
            .parent()
            .unwrap_or_else(|| std::path::Path::new(""))
            .join(mcp_file_name);
        Ok(HarnessConfigPaths {
            instruction_target: config_dir.join("CLAUDE.md"),
            skills_dir: config_dir.join("skills"),
            commands_dir: config_dir.join("commands"),
            agents_dir: config_dir.join("agents"),
            settings_file: config_dir.join("settings.json"),
            mcp_file,
            config_dir,
        })
    }

    fn artifacts(&self) -> Vec<Box<dyn HarnessArtifact>> {
        vec![
            Box::new(InstructionFile::new(|paths| &paths.instruction_target)),
            Box::new(SkillsDirectory::new(|paths| &paths.skills_dir)),
            Box::new(CommandsDirectory::new(
                |paths| &paths.commands_dir,
                CommandMode::RecursiveSymlink,
            )),
            Box::new(SubagentsDirectory::new(
                |paths| &paths.agents_dir,
                ClaudeSubagentCodec,
            )),
            Box::new(McpConfig::new(
                JsonConfigFile::new(|paths| &paths.mcp_file).label("Claude MCP JSON"),
                ClaudeMcpCodec,
            )),
        ]
    }

    fn settings(&self) -> Option<Box<dyn crate::harness::artifact::HarnessSettings>> {
        Some(Box::new(
            SettingsPreferences::new(
                JsonConfigFile::new(|paths| &paths.settings_file).label("Claude settings JSON"),
            )
            .model(PreferenceBinding::JsonPointer {
                pointer: "/primaryModel",
            })
            .permission(PreferenceBinding::Custom(Box::new(ClaudePermissionCodec))),
        ))
    }
}

struct ClaudeSubagentCodec;

impl SubagentCodec for ClaudeSubagentCodec {
    fn native_file_name(&self, agent: &SubAgent) -> String {
        format!("{}.md", agent.name)
    }

    fn render(&self, agent: &SubAgent) -> Result<String> {
        Ok(render_claude_agent(agent)?.contents)
    }

    fn should_import(&self, path: &Path) -> bool {
        path.extension().is_some_and(|ext| ext == "md")
    }

    fn parse(&self, path: &Path, contents: &str) -> Result<SubAgent> {
        let fallback_name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid agent path {}", path.display()))?;
        native_markdown_to_neutral(contents, fallback_name, "claude", true)
    }
}

struct ClaudeMcpCodec;

impl McpCodec for ClaudeMcpCodec {
    fn import(&self, config: &NativeConfig) -> Result<Vec<McpDefinition>> {
        import_claude_mcps(json_config_object(config)?)
    }

    fn apply(&self, config: &mut NativeConfig, definitions: &[McpDefinition]) -> Result<()> {
        let document = json_config_object_mut(config)?;
        document.remove("mcpServers");
        if !definitions.is_empty() {
            let mut servers = Map::new();
            for definition in definitions {
                servers.insert(definition.name.clone(), definition.to_claude_value()?);
            }
            document.insert("mcpServers".to_string(), Value::Object(servers));
        }
        Ok(())
    }
}

struct ClaudePermissionCodec;

impl PreferenceCodec for ClaudePermissionCodec {
    fn import(&self, config: &NativeConfig) -> Result<ImportedPreference> {
        Ok(ImportedPreference::new(
            json_config_object(config)?
                .get("permissions")
                .cloned()
                .unwrap_or_else(|| json!("default")),
        ))
    }

    fn apply(
        &self,
        config: &mut NativeConfig,
        profile: &ProfileRef,
        _preference_kind: PreferenceKind,
    ) -> Result<()> {
        let profile_config = crate::profile::read_profile_config(&profile.path)?;
        if let Some(permission) =
            non_default_value(profile_config.permission_preference(&profile.harness_id))
        {
            patch_claude_permissions(json_config_object_mut(config)?, permission)?;
        }
        Ok(())
    }
}

fn render_claude_agent(agent: &SubAgent) -> Result<RenderedAgent> {
    let mut map = serde_yaml::Mapping::new();
    map.insert("name".into(), agent.name.clone().into());
    map.insert("description".into(), agent.description.clone().into());
    if let Some(model) = select_harness_value(agent.model.as_ref(), "claude") {
        map.insert("model".into(), model.clone());
    }
    if let Some(tools) = agent.tools.as_ref() {
        map.insert("tools".into(), tools_to_allow_list(tools));
    }
    if let Some(permission) = select_harness_value(agent.permission.as_ref(), "claude") {
        map.insert("permissionMode".into(), permission.clone());
    }
    if let Some(max_turns) = agent.max_turns {
        map.insert(
            "maxTurns".into(),
            serde_yaml::Value::Number(max_turns.into()),
        );
    }
    merge_harness_override(&mut map, agent, "claude")?;
    Ok(RenderedAgent {
        relative_path: std::path::PathBuf::from(format!("{}.md", agent.name)),
        contents: render_native_markdown(map, &agent.body)?,
    })
}

fn native_markdown_to_neutral(
    text: &str,
    fallback_name: &str,
    harness_id: &str,
    require_name: bool,
) -> Result<SubAgent> {
    let (frontmatter, body) = split_markdown_frontmatter(text)?;
    let mut map = serde_yaml::from_str::<serde_yaml::Mapping>(frontmatter)?;
    let name = match remove_string(&mut map, "name")? {
        Some(name) => name,
        None if require_name => anyhow::bail!("native agent is missing name"),
        None => fallback_name.to_string(),
    };
    let description = remove_string(&mut map, "description")?
        .ok_or_else(|| anyhow::anyhow!("native agent is missing description"))?;
    let body = body.trim().to_string();
    let model = harness_scoped_value(harness_id, remove_value(&mut map, "model"));
    let tools = remove_value(&mut map, "tools").map(normalize_tools_for_profile);
    let permission = harness_scoped_value(
        harness_id,
        remove_value(&mut map, "permission").or_else(|| remove_value(&mut map, "permissionMode")),
    );
    let max_turns = remove_value(&mut map, "maxTurns")
        .or_else(|| remove_value(&mut map, "max_turns"))
        .and_then(|value| value.as_u64());
    remove_value(&mut map, "mode");
    remove_value(&mut map, "kind");
    let mut harness = BTreeMap::new();
    if !map.is_empty() {
        harness.insert(harness_id.to_string(), serde_yaml::Value::Mapping(map));
    }
    Ok(SubAgent {
        name,
        description,
        model,
        tools,
        permission,
        max_turns,
        harness,
        body,
    })
}

fn render_native_markdown(map: serde_yaml::Mapping, body: &str) -> Result<String> {
    let yaml = serde_yaml::to_string(&map)?;
    Ok(format!(
        "---\n{}---\n{}\n",
        yaml.strip_prefix("---\n").unwrap_or(&yaml),
        body
    ))
}

fn merge_harness_override(
    map: &mut serde_yaml::Mapping,
    agent: &SubAgent,
    harness_id: &str,
) -> Result<()> {
    let Some(serde_yaml::Value::Mapping(override_map)) = agent.harness.get(harness_id) else {
        return Ok(());
    };
    for (key, value) in override_map {
        if !matches!(key, serde_yaml::Value::String(_)) {
            anyhow::bail!("{harness_id} harness override keys must be strings");
        }
        map.insert(key.clone(), value.clone());
    }
    Ok(())
}

fn tools_to_allow_list(value: &serde_yaml::Value) -> serde_yaml::Value {
    match value {
        serde_yaml::Value::Mapping(map) => serde_yaml::Value::Sequence(
            map.iter()
                .filter_map(|(key, val)| {
                    let key = key.as_str()?;
                    if tool_is_allowed(val) {
                        Some(serde_yaml::Value::String(key.to_string()))
                    } else {
                        None
                    }
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn normalize_tools_for_profile(value: serde_yaml::Value) -> serde_yaml::Value {
    match value {
        serde_yaml::Value::Mapping(map) => {
            let mut out = serde_yaml::Mapping::new();
            for (key, value) in map {
                let normalized = match value {
                    serde_yaml::Value::Bool(true) => serde_yaml::Value::String("allow".to_string()),
                    serde_yaml::Value::Bool(false) => serde_yaml::Value::String("deny".to_string()),
                    other => other,
                };
                out.insert(key, normalized);
            }
            serde_yaml::Value::Mapping(out)
        }
        other => other,
    }
}

fn tool_is_allowed(value: &serde_yaml::Value) -> bool {
    match value {
        serde_yaml::Value::Bool(value) => *value,
        serde_yaml::Value::String(value) => {
            matches!(value.as_str(), "allow" | "allowed" | "true" | "yes")
        }
        _ => false,
    }
}

fn import_claude_mcps(document: &Map<String, Value>) -> Result<Vec<McpDefinition>> {
    let mut servers = Vec::new();
    let Some(mcp_servers) = document.get("mcpServers") else {
        return Ok(Vec::new());
    };
    let Some(mcp_table) = mcp_servers.as_object() else {
        anyhow::bail!("Claude mcpServers must be an object");
    };

    for (name, item) in mcp_table {
        let Some(table) = item.as_object() else {
            anyhow::bail!("Claude MCP server {name} must be an object");
        };
        let enabled = table
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let mcp_type = table.get("type").and_then(Value::as_str).unwrap_or("stdio");

        if mcp_type == "stdio" {
            let command = table.get("command").and_then(Value::as_str).unwrap_or("");
            let args = table
                .get("args")
                .and_then(Value::as_array)
                .map(|array| {
                    array
                        .iter()
                        .map(|value| {
                            value.as_str().map(str::to_string).ok_or_else(|| {
                                anyhow::anyhow!("Claude MCP {name} args must be strings")
                            })
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default();

            let env = if let Some(Value::Object(env_obj)) = table.get("env") {
                let mut map = BTreeMap::new();
                for (k, v) in env_obj {
                    if let Some(s) = v.as_str() {
                        map.insert(k.clone(), s.to_string());
                    } else {
                        anyhow::bail!("Claude MCP env values must be strings");
                    }
                }
                json!(map)
            } else {
                json!({})
            };

            servers.push(json!({
                "name": name,
                "enabled": enabled,
                "transport": "stdio",
                "command": command,
                "args": args,
                "env": env,
            }));
        } else if mcp_type == "http" {
            let url = table.get("url").and_then(Value::as_str).unwrap_or("");

            let headers = if let Some(Value::Object(headers_obj)) = table.get("headers") {
                let mut map = BTreeMap::new();
                for (k, v) in headers_obj {
                    if let Some(s) = v.as_str() {
                        map.insert(k.clone(), s.to_string());
                    } else {
                        anyhow::bail!("Claude MCP headers values must be strings");
                    }
                }
                map
            } else {
                BTreeMap::new()
            };

            servers.push(json!({
                "name": name,
                "enabled": enabled,
                "transport": "http",
                "url": url,
                "headers": headers,
            }));
        } else {
            anyhow::bail!("Claude MCP server {name} has unsupported type {mcp_type}");
        }
    }

    crate::profile::mcp::parse_mcp_definitions(&serde_json::to_string(&servers)?)
}

fn non_default_value(value: Value) -> Option<Value> {
    match value {
        Value::String(ref string_val) if string_val == "default" => None,
        other => Some(other),
    }
}

fn json_config_object(config: &NativeConfig) -> Result<&Map<String, Value>> {
    let NativeConfig::Json(Value::Object(document)) = config else {
        anyhow::bail!("Claude JSON config must be an object");
    };
    Ok(document)
}

fn json_config_object_mut(config: &mut NativeConfig) -> Result<&mut Map<String, Value>> {
    let NativeConfig::Json(value) = config else {
        anyhow::bail!("Claude JSON config must be an object");
    };
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Claude JSON config must be an object"))
}

fn patch_claude_permissions(document: &mut Map<String, Value>, preference: Value) -> Result<()> {
    match preference {
        Value::String(default_mode) => {
            let permissions = document
                .entry("permissions".to_string())
                .or_insert_with(|| json!({}));
            let Some(permissions) = permissions.as_object_mut() else {
                anyhow::bail!("Claude permissions setting must be an object");
            };
            permissions.insert("defaultMode".to_string(), json!(default_mode));
        }
        Value::Object(permissions) => {
            document.insert("permissions".to_string(), Value::Object(permissions));
        }
        other => {
            anyhow::bail!(
                "Claude permission preference must be a string defaultMode or permissions object, got {other}"
            );
        }
    }
    Ok(())
}

impl McpDefinition {
    fn to_claude_value(&self) -> Result<Value> {
        let mut map = Map::new();
        map.insert("enabled".to_string(), json!(self.enabled));
        match &self.transport {
            McpTransport::Stdio(stdio) => {
                map.insert("type".to_string(), json!("stdio"));
                map.insert("command".to_string(), json!(stdio.command));
                if !stdio.args.is_empty() {
                    map.insert("args".to_string(), json!(stdio.args));
                }
                if !stdio.env.is_empty() {
                    map.insert("env".to_string(), json!(stdio.env));
                }
            }
            McpTransport::Http(http) => {
                map.insert("type".to_string(), json!("http"));
                map.insert("url".to_string(), json!(http.url));
                if !http.headers.is_empty() {
                    map.insert("headers".to_string(), json!(http.headers));
                }
            }
        }
        Ok(Value::Object(map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::integration::{HarnessConfigPaths, HarnessIntegration, ProfileImport};
    use crate::integrations::test_suite::template::HarnessTestAdapter;
    use crate::profile::ProfileConfig;
    use std::fs;
    use std::path::Path;

    #[derive(Default)]
    struct ClaudeAdapter;

    impl HarnessTestAdapter for ClaudeAdapter {
        fn integration(&self) -> Box<dyn HarnessIntegration> {
            Box::new(ClaudeIntegration)
        }
        fn bin_name(&self) -> &'static str {
            "claude"
        }
        fn assert_mcp_cleared(&self, paths: &HarnessConfigPaths) {
            let config = fs::read_to_string(&paths.mcp_file).unwrap_or_else(|_| "{}".to_string());
            assert_eq!(
                crate::harness::fs::normalize_json_text(&config),
                serde_json::json!({})
            );
        }
        fn write_malformed_native_config(&self, paths: &HarnessConfigPaths) {
            fs::write(&paths.settings_file, "{ malformed }").unwrap();
        }
        fn supports_nested_commands(&self) -> bool {
            true
        }
        fn write_existing_native_settings(&self, paths: &HarnessConfigPaths) {
            fs::write(&paths.settings_file, r#"{"theme": "dark"}"#).unwrap();
        }
        fn assert_native_settings_preserved(&self, paths: &HarnessConfigPaths) {
            let config = fs::read_to_string(&paths.settings_file).unwrap();
            assert!(config.contains(r#""theme":"dark""#) || config.contains(r#""theme": "dark""#));
        }
        fn setup_native_config_for_import(&self, paths: &HarnessConfigPaths) {
            fs::write(
                &paths.settings_file,
                r#"{"primaryModel": "opus", "permissions": {"defaultMode": "acceptEdits"}}"#,
            )
            .unwrap();
            fs::write(
                &paths.mcp_file,
                r#"{
  "mcpServers": {
    "local": {"command":"server"},
    "remote": {"command":"server", "env": {"Authorization": "$TOKEN"}}
  }
}"#,
            )
            .unwrap();
        }
        fn assert_imported_native_config(&self, import: &ProfileImport) {
            assert_eq!(
                import.model_preference.clone().into_value(),
                serde_json::json!("opus")
            );
            assert!(import
                .mcp_definitions
                .as_ref()
                .unwrap()
                .contains("\"Authorization\": \"$TOKEN\""));
        }
        fn setup_drift_native_config(&self, paths: &HarnessConfigPaths) {
            fs::write(
                &paths.settings_file,
                r#"{"primaryModel": "drift-model", "permissions": {"defaultMode": "drift-perm"}}"#,
            )
            .unwrap();
        }
        fn assert_drift_saved(&self, config: &ProfileConfig) {
            assert_eq!(config.model_preference("claude"), "drift-model");
            assert_eq!(
                config.permission_preference("claude"),
                serde_json::json!({"defaultMode": "drift-perm"})
            );
        }
        fn write_profile_config(&self, profile: &Path) {
            crate::integrations::test_suite::template::write_config(
                profile,
                r#"{
  "name": "work",
  "description": "",
  "models": {"claude": "opus"},
  "permissions": {"claude": "acceptEdits"}
}"#,
            );
        }
        fn assert_applied_native_config(&self, paths: &HarnessConfigPaths) {
            let config = fs::read_to_string(&paths.settings_file).unwrap();
            assert!(config.contains("opus"));
            assert!(config.contains("acceptEdits"));
            let mcp = fs::read_to_string(&paths.mcp_file).unwrap();
            assert!(mcp.contains("local"));
            assert!(mcp.contains("server"));
            assert!(mcp.contains("disabled"));
            assert!(mcp.contains(r#""enabled": false"#));
        }
    }

    crate::define_standard_harness_tests!(ClaudeAdapter);
}
