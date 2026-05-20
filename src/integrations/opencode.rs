use std::collections::BTreeMap;
use std::path::Path;

#[cfg(test)]
use anyhow::Context;
use anyhow::Result;
use serde_json::{json, Map, Value};

#[cfg(test)]
use crate::harness::agents::sub_agent_import_file;
use crate::harness::agents::{
    harness_scoped_value, remove_string, remove_value, select_harness_value,
    split_markdown_frontmatter, RenderedAgent, SubAgent,
};
use crate::harness::artifact::{
    CommandMode, CommandsDirectory, HarnessArtifact, InstructionFile, JsonConfigFile, McpCodec,
    McpConfig, NativeConfig, PreferenceBinding, SettingsPreferences, SkillsDirectory,
    SubagentCodec, SubagentsDirectory,
};
#[cfg(test)]
use crate::harness::integration::ImportedFile;
use crate::harness::integration::{AppEnvironment, HarnessConfigPaths, HarnessIntegration};
use crate::harness::kind::HarnessKind;
use crate::profile::mcp::{McpDefinition, McpTransport};

pub struct OpenCodeIntegration;

impl HarnessIntegration for OpenCodeIntegration {
    fn kind(&self) -> HarnessKind {
        HarnessKind::OpenCode
    }

    fn default_config_dir(&self, env: &AppEnvironment) -> std::path::PathBuf {
        env.user_home.join(".config").join("opencode")
    }

    fn paths_from_config_dir(&self, config_dir: std::path::PathBuf) -> Result<HarnessConfigPaths> {
        Ok(HarnessConfigPaths {
            instruction_target: config_dir.join("AGENTS.md"),
            skills_dir: config_dir.join("skills"),
            commands_dir: config_dir.join("commands"),
            agents_dir: config_dir.join("agents"),
            settings_file: config_dir.join("opencode.json"),
            mcp_file: config_dir.join("opencode.json"),
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
                OpenCodeSubagentCodec,
            )),
            Box::new(McpConfig::new(
                JsonConfigFile::new(|paths| &paths.settings_file).label("OpenCode settings JSON"),
                OpenCodeMcpCodec,
            )),
        ]
    }

    fn settings(&self) -> Option<Box<dyn crate::harness::artifact::HarnessSettings>> {
        Some(Box::new(
            SettingsPreferences::new(
                JsonConfigFile::new(|paths| &paths.settings_file).label("OpenCode settings JSON"),
            )
            .model(PreferenceBinding::JsonPointer { pointer: "/model" })
            .permission(PreferenceBinding::JsonPointer {
                pointer: "/permissions",
            }),
        ))
    }
}

struct OpenCodeSubagentCodec;

impl SubagentCodec for OpenCodeSubagentCodec {
    fn native_file_name(&self, agent: &SubAgent) -> String {
        format!("{}.md", agent.name)
    }

    fn render(&self, agent: &SubAgent) -> Result<String> {
        Ok(render_opencode_agent(agent)?.contents)
    }

    fn should_import(&self, path: &Path) -> bool {
        path.extension().is_some_and(|ext| ext == "md")
    }

    fn parse(&self, path: &Path, contents: &str) -> Result<SubAgent> {
        let fallback_name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid agent path {}", path.display()))?;
        native_markdown_to_neutral(contents, fallback_name, "opencode")
    }
}

struct OpenCodeMcpCodec;

impl McpCodec for OpenCodeMcpCodec {
    fn import(&self, config: &NativeConfig) -> Result<Vec<McpDefinition>> {
        import_opencode_mcps(json_config_object(config)?)
    }

    fn apply(&self, config: &mut NativeConfig, definitions: &[McpDefinition]) -> Result<()> {
        let document = json_config_object_mut(config)?;
        document.remove("mcp");
        if !definitions.is_empty() {
            let mut servers = Map::new();
            for definition in definitions {
                servers.insert(definition.name.clone(), definition.to_opencode_value()?);
            }
            document.insert("mcp".to_string(), Value::Object(servers));
        }
        Ok(())
    }
}

fn render_opencode_agent(agent: &SubAgent) -> Result<RenderedAgent> {
    let mut map = serde_yaml::Mapping::new();
    map.insert(yaml_key("name"), agent.name.clone().into());
    map.insert(yaml_key("description"), agent.description.clone().into());
    map.insert(yaml_key("mode"), "subagent".into());
    if let Some(model) = select_harness_value(agent.model.as_ref(), "opencode") {
        map.insert(yaml_key("model"), model.clone());
    }
    if let Some(tools) = agent.tools.as_ref() {
        map.insert(yaml_key("tools"), tools_to_bool_map(tools));
    }
    if let Some(permission) = select_harness_value(agent.permission.as_ref(), "opencode") {
        map.insert(yaml_key("permission"), permission.clone());
    }
    if let Some(max_turns) = agent.max_turns {
        map.insert(
            yaml_key("maxTurns"),
            serde_yaml::Value::Number(max_turns.into()),
        );
    }
    merge_harness_override(&mut map, agent, "opencode")?;
    Ok(RenderedAgent {
        relative_path: std::path::PathBuf::from(format!("{}.md", agent.name)),
        contents: render_native_markdown(map, &agent.body)?,
    })
}

#[cfg(test)]
fn import_opencode_agents(path: &Path) -> Result<Option<Vec<ImportedFile>>> {
    if !path.exists() {
        return Ok(Some(Vec::new()));
    }
    let mut imported = Vec::new();
    for file in crate::harness::fs::import_files_recursive(path, path)? {
        if file.relative_path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let text = String::from_utf8(file.contents).with_context(|| {
            format!(
                "OpenCode agent {} is not UTF-8",
                file.relative_path.display()
            )
        })?;
        let fallback_name = file
            .relative_path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                anyhow::anyhow!("invalid agent path {}", file.relative_path.display())
            })?;
        let neutral = native_markdown_to_neutral(&text, fallback_name, "opencode")?;
        imported.push(sub_agent_import_file(&neutral));
    }
    imported.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(Some(imported))
}

fn native_markdown_to_neutral(
    text: &str,
    fallback_name: &str,
    harness_id: &str,
) -> Result<SubAgent> {
    let (frontmatter, body) = split_markdown_frontmatter(text)?;
    let mut map = serde_yaml::from_str::<serde_yaml::Mapping>(frontmatter)?;
    let name = remove_string(&mut map, "name")?.unwrap_or_else(|| fallback_name.to_string());
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

fn tools_to_bool_map(value: &serde_yaml::Value) -> serde_yaml::Value {
    match value {
        serde_yaml::Value::Mapping(map) => {
            let mut out = serde_yaml::Mapping::new();
            for (key, value) in map {
                out.insert(key.clone(), serde_yaml::Value::Bool(tool_is_allowed(value)));
            }
            serde_yaml::Value::Mapping(out)
        }
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
        serde_yaml::Value::Sequence(items) => {
            let mut out = serde_yaml::Mapping::new();
            for item in items {
                if let Some(tool) = item.as_str() {
                    out.insert(
                        yaml_key(tool),
                        serde_yaml::Value::String("allow".to_string()),
                    );
                }
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

fn yaml_key(key: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(key.to_string())
}

fn import_opencode_mcps(document: &Map<String, Value>) -> Result<Vec<McpDefinition>> {
    let mut servers = Vec::new();
    let Some(mcp_servers) = document.get("mcp") else {
        return Ok(Vec::new());
    };
    let Some(mcp_table) = mcp_servers.as_object() else {
        anyhow::bail!("OpenCode mcp must be an object");
    };

    for (name, item) in mcp_table {
        let Some(table) = item.as_object() else {
            anyhow::bail!("OpenCode MCP server {name} must be an object");
        };
        let enabled = table
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let mcp_type = table.get("type").and_then(Value::as_str).unwrap_or("local");

        if mcp_type == "local" {
            let command_array = table
                .get("command")
                .and_then(Value::as_array)
                .map(|array| {
                    array
                        .iter()
                        .map(|value| {
                            value.as_str().map(str::to_string).ok_or_else(|| {
                                anyhow::anyhow!("OpenCode MCP {name} command must be strings")
                            })
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default();

            if command_array.is_empty() {
                anyhow::bail!("OpenCode MCP {name} must have a non-empty command array");
            }

            let command = command_array[0].clone();
            let args = command_array[1..].to_vec();

            let env = if let Some(Value::Object(env_obj)) = table.get("environment") {
                let mut map = BTreeMap::new();
                for (k, v) in env_obj {
                    if let Some(s) = v.as_str() {
                        map.insert(k.clone(), s.to_string());
                    } else {
                        anyhow::bail!("OpenCode MCP environment values must be strings");
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
        } else if mcp_type == "remote" {
            let url = table.get("url").and_then(Value::as_str).unwrap_or("");

            let headers = if let Some(Value::Object(headers_obj)) = table.get("headers") {
                let mut map = BTreeMap::new();
                for (k, v) in headers_obj {
                    if let Some(s) = v.as_str() {
                        map.insert(k.clone(), s.to_string());
                    } else {
                        anyhow::bail!("OpenCode MCP headers values must be strings");
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
            anyhow::bail!("OpenCode MCP server {name} has unsupported type {mcp_type}");
        }
    }

    crate::profile::mcp::parse_mcp_definitions(&serde_json::to_string(&servers)?)
}

fn json_config_object(config: &NativeConfig) -> Result<&Map<String, Value>> {
    let NativeConfig::Json(Value::Object(document)) = config else {
        anyhow::bail!("OpenCode JSON config must be an object");
    };
    Ok(document)
}

fn json_config_object_mut(config: &mut NativeConfig) -> Result<&mut Map<String, Value>> {
    let NativeConfig::Json(value) = config else {
        anyhow::bail!("OpenCode JSON config must be an object");
    };
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("OpenCode JSON config must be an object"))
}

impl McpDefinition {
    fn to_opencode_value(&self) -> Result<Value> {
        let mut map = Map::new();
        map.insert("enabled".to_string(), json!(self.enabled));
        match &self.transport {
            McpTransport::Stdio(stdio) => {
                map.insert("type".to_string(), json!("local"));
                let mut cmd_array = vec![stdio.command.clone()];
                cmd_array.extend(stdio.args.clone());
                map.insert("command".to_string(), json!(cmd_array));

                if !stdio.env.is_empty() {
                    map.insert("environment".to_string(), json!(stdio.env));
                }
            }
            McpTransport::Http(http) => {
                map.insert("type".to_string(), json!("remote"));
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
    struct OpenCodeAdapter;

    impl HarnessTestAdapter for OpenCodeAdapter {
        fn integration(&self) -> Box<dyn HarnessIntegration> {
            Box::new(OpenCodeIntegration)
        }
        fn bin_name(&self) -> &'static str {
            "opencode"
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
            fs::write(&paths.settings_file, r#"{"other": true}"#).unwrap();
        }
        fn assert_native_settings_preserved(&self, paths: &HarnessConfigPaths) {
            let config = fs::read_to_string(&paths.settings_file).unwrap();
            assert!(config.contains(r#""other":true"#) || config.contains(r#""other": true"#));
        }
        fn setup_native_config_for_import(&self, paths: &HarnessConfigPaths) {
            fs::write(&paths.settings_file, r#"{
  "model": "gpt-imported",
  "permissions": "on-request",
  "mcp": {
    "local": {"command":["server"]},
    "remote": {"type": "remote", "url": "https://mcp.example", "headers": {"Authorization": "$TOKEN"}}
  }
}"#).unwrap();
        }
        fn assert_imported_native_config(&self, import: &ProfileImport) {
            assert_eq!(
                import.model_preference.clone().into_value(),
                serde_json::json!("gpt-imported")
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
                r#"{"model": "drift-model", "permissions": "drift-perm"}"#,
            )
            .unwrap();
        }
        fn assert_drift_saved(&self, config: &ProfileConfig) {
            assert_eq!(config.model_preference("opencode"), "drift-model");
            assert_eq!(config.permission_preference("opencode"), "drift-perm");
        }
        fn write_profile_config(&self, profile: &Path) {
            crate::integrations::test_suite::template::write_config(
                profile,
                r#"{
  "name": "work",
  "description": "",
  "models": {"opencode": "gpt-5.2"},
  "permissions": {"opencode": "on-request"}
}"#,
            );
        }
        fn assert_applied_native_config(&self, paths: &HarnessConfigPaths) {
            let config = fs::read_to_string(&paths.settings_file).unwrap();
            assert!(config.contains("gpt-5.2"));
            assert!(config.contains("on-request"));
            let mcp = fs::read_to_string(&paths.mcp_file).unwrap();
            assert!(mcp.contains("local"));
            assert!(mcp.contains("server"));
            assert!(mcp.contains("disabled"));
            assert!(mcp.contains(r#""enabled": false"#));
        }
    }

    crate::define_standard_harness_tests!(OpenCodeAdapter);

    #[test]
    fn imports_opencode_agent_without_name_as_sub_agent() {
        let temp = tempfile::tempdir().unwrap();
        let agents = temp.path().join("agents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(
            agents.join("coder.md"),
            r#"---
description: Writes focused Rust changes
mode: subagent
model: gpt-5.2
tools:
  edit: true
  shell: false
temperature: 0.2
---
Implement the change carefully.
"#,
        )
        .unwrap();

        let imported = import_opencode_agents(&agents).unwrap().unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(
            imported[0].relative_path,
            std::path::PathBuf::from("coder.md")
        );

        let contents = String::from_utf8(imported[0].contents.clone()).unwrap();
        let agent = crate::harness::agents::parse_sub_agent(&contents).unwrap();
        assert_eq!(agent.name, "coder");
        assert_eq!(agent.description, "Writes focused Rust changes");
        assert_eq!(
            agent
                .model
                .as_ref()
                .and_then(|model| model.get("opencode"))
                .and_then(serde_yaml::Value::as_str),
            Some("gpt-5.2")
        );
        assert!(agent
            .model
            .as_ref()
            .and_then(|model| model.get("codex"))
            .is_none());
        assert_eq!(
            agent
                .tools
                .as_ref()
                .and_then(|tools| tools.get("edit"))
                .and_then(serde_yaml::Value::as_str),
            Some("allow")
        );
        assert_eq!(
            agent
                .tools
                .as_ref()
                .and_then(|tools| tools.get("shell"))
                .and_then(serde_yaml::Value::as_str),
            Some("deny")
        );
        assert!(agent
            .harness
            .get("opencode")
            .and_then(|value| value.get("temperature"))
            .is_some());
        assert_eq!(agent.body, "Implement the change carefully.");
    }

    #[test]
    fn imports_opencode_agent_with_empty_body() {
        let temp = tempfile::tempdir().unwrap();
        let agents = temp.path().join("agents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(
            agents.join("coder.md"),
            r#"---
description: Writes focused Rust changes
mode: subagent
tools:
  edit: true
---
"#,
        )
        .unwrap();

        let imported = import_opencode_agents(&agents).unwrap().unwrap();
        let contents = String::from_utf8(imported[0].contents.clone()).unwrap();
        let agent = crate::harness::agents::parse_sub_agent(&contents).unwrap();

        assert_eq!(agent.name, "coder");
        assert_eq!(agent.description, "Writes focused Rust changes");
        assert_eq!(agent.body, "");
    }
}
