use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use toml_edit::{value, Array, DocumentMut, Item, Table};

use crate::harness::agents::{
    harness_scoped_value, select_harness_value, yaml_scalar_string, RenderedAgent, SubAgent,
};
use crate::harness::artifact::{
    CommandMode, CommandsDirectory, HarnessArtifact, InstructionFile, McpCodec, McpConfig,
    NativeConfig, PreferenceBinding, SettingsPreferences, SkillsDirectory, SubagentCodec,
    SubagentsDirectory, TomlConfigFile,
};
use crate::harness::integration::{AppEnvironment, HarnessConfigPaths, HarnessIntegration};
use crate::harness::kind::HarnessKind;
use crate::profile::mcp::{McpDefinition, McpTransport};

pub struct CodexIntegration;

impl HarnessIntegration for CodexIntegration {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Codex
    }

    fn default_config_dir(&self, env: &AppEnvironment) -> std::path::PathBuf {
        env.user_home.join(".codex")
    }

    fn paths_from_config_dir(&self, config_dir: std::path::PathBuf) -> Result<HarnessConfigPaths> {
        Ok(HarnessConfigPaths {
            instruction_target: config_dir.join("AGENTS.md"),
            skills_dir: config_dir.join("skills"),
            commands_dir: config_dir.join("prompts"),
            agents_dir: config_dir.join("agents"),
            settings_file: config_dir.join("config.toml"),
            mcp_file: config_dir.join("config.toml"),
            config_dir,
        })
    }

    fn artifacts(&self) -> Vec<Box<dyn HarnessArtifact>> {
        vec![
            Box::new(InstructionFile::new(|paths| &paths.instruction_target)),
            Box::new(SkillsDirectory::new(|paths| &paths.skills_dir)),
            Box::new(CommandsDirectory::new(
                |paths| &paths.commands_dir,
                CommandMode::FlatSymlink,
            )),
            Box::new(SubagentsDirectory::new(
                |paths| &paths.agents_dir,
                CodexSubagentCodec,
            )),
            Box::new(McpConfig::new(
                TomlConfigFile::new(|paths| &paths.settings_file).label("Codex config TOML"),
                CodexMcpCodec,
            )),
        ]
    }

    fn settings(&self) -> Option<Box<dyn crate::harness::artifact::HarnessSettings>> {
        Some(Box::new(
            SettingsPreferences::new(
                TomlConfigFile::new(|paths| &paths.settings_file).label("Codex config TOML"),
            )
            .model(PreferenceBinding::TomlKey { key: "model" })
            .permission(PreferenceBinding::TomlKey {
                key: "approval_policy",
            }),
        ))
    }
}

struct CodexSubagentCodec;

impl SubagentCodec for CodexSubagentCodec {
    fn native_file_name(&self, agent: &SubAgent) -> String {
        format!("{}.toml", agent.name)
    }

    fn render(&self, agent: &SubAgent) -> Result<String> {
        Ok(render_codex_agent(agent)?.contents)
    }

    fn should_import(&self, path: &Path) -> bool {
        path.extension().is_some_and(|ext| ext == "toml")
    }

    fn parse(&self, path: &Path, contents: &str) -> Result<SubAgent> {
        codex_toml_to_neutral(contents)
            .with_context(|| format!("failed to import Codex agent {}", path.display()))
    }
}

struct CodexMcpCodec;

impl McpCodec for CodexMcpCodec {
    fn import(&self, config: &NativeConfig) -> Result<Vec<McpDefinition>> {
        let NativeConfig::Toml(document) = config else {
            anyhow::bail!("Codex MCP codec requires TOML config");
        };
        parse_codex_mcps(document)
    }

    fn apply(&self, config: &mut NativeConfig, definitions: &[McpDefinition]) -> Result<()> {
        let NativeConfig::Toml(document) = config else {
            anyhow::bail!("Codex MCP codec requires TOML config");
        };
        document.as_table_mut().remove("mcp_servers");
        if !definitions.is_empty() {
            let mut servers = Table::new();
            for definition in definitions {
                servers[&definition.name] = Item::Table(definition.to_codex_table()?);
            }
            document["mcp_servers"] = Item::Table(servers);
        }
        Ok(())
    }
}

fn render_codex_agent(agent: &SubAgent) -> Result<RenderedAgent> {
    let mut document = DocumentMut::new();
    document["name"] = value(agent.name.clone());
    document["description"] = value(agent.description.clone());
    document["developer_instructions"] = value(agent.body.clone());
    if let Some(model) =
        select_harness_value(agent.model.as_ref(), "codex").and_then(yaml_scalar_string)
    {
        document["model"] = value(model);
    }
    if let Some(permission) =
        select_harness_value(agent.permission.as_ref(), "codex").and_then(yaml_scalar_string)
    {
        document["approval_policy"] = value(permission);
    }
    if let Some(max_turns) = agent.max_turns {
        document["max_turns"] = value(max_turns as i64);
    }
    if let Some(serde_yaml::Value::Mapping(map)) = agent.harness.get("codex") {
        for (key, val) in map {
            let Some(key) = key.as_str() else {
                anyhow::bail!("Codex harness override keys must be strings");
            };
            document[key] = yaml_to_toml_item(val)?;
        }
    }
    Ok(RenderedAgent {
        relative_path: std::path::PathBuf::from(format!("{}.toml", agent.name)),
        contents: document.to_string(),
    })
}

fn codex_toml_to_neutral(text: &str) -> Result<SubAgent> {
    let document = text.parse::<DocumentMut>()?;
    let name = document
        .get("name")
        .and_then(Item::as_str)
        .ok_or_else(|| anyhow::anyhow!("Codex agent is missing name"))?
        .to_string();
    let description = document
        .get("description")
        .and_then(Item::as_str)
        .ok_or_else(|| anyhow::anyhow!("Codex agent is missing description"))?
        .to_string();
    let body = document
        .get("developer_instructions")
        .and_then(Item::as_str)
        .ok_or_else(|| anyhow::anyhow!("Codex agent is missing developer_instructions"))?
        .to_string();
    let model = harness_scoped_value(
        "codex",
        document
            .get("model")
            .and_then(Item::as_str)
            .map(|model| serde_yaml::Value::String(model.to_string())),
    );
    let permission = harness_scoped_value(
        "codex",
        document
            .get("approval_policy")
            .and_then(Item::as_str)
            .map(|permission| serde_yaml::Value::String(permission.to_string())),
    );
    let max_turns = document
        .get("max_turns")
        .and_then(Item::as_integer)
        .and_then(|turns| turns.try_into().ok());
    let mut codex_overrides = serde_yaml::Mapping::new();
    for (key, item) in document.as_table().iter() {
        if matches!(
            key,
            "name"
                | "description"
                | "developer_instructions"
                | "model"
                | "approval_policy"
                | "max_turns"
        ) {
            continue;
        }
        codex_overrides.insert(
            serde_yaml::Value::String(key.to_string()),
            toml_item_to_yaml(item)
                .with_context(|| format!("failed to import Codex agent field {key}"))?,
        );
    }
    let mut harness = BTreeMap::new();
    if !codex_overrides.is_empty() {
        harness.insert(
            "codex".to_string(),
            serde_yaml::Value::Mapping(codex_overrides),
        );
    }
    Ok(SubAgent {
        name,
        description,
        model,
        tools: None,
        permission,
        max_turns,
        harness,
        body,
    })
}

fn toml_item_to_yaml(item: &Item) -> Result<serde_yaml::Value> {
    if item.is_none() {
        return Ok(serde_yaml::Value::Null);
    }
    if let Some(value) = item.as_value() {
        return toml_value_to_yaml(value);
    }
    if let Some(table) = item.as_table() {
        let mut map = serde_yaml::Mapping::new();
        for (key, value) in table.iter() {
            map.insert(
                serde_yaml::Value::String(key.to_string()),
                toml_item_to_yaml(value)?,
            );
        }
        return Ok(serde_yaml::Value::Mapping(map));
    }
    anyhow::bail!("unsupported TOML value {}", item.to_string().trim())
}

fn toml_value_to_yaml(value: &toml_edit::Value) -> Result<serde_yaml::Value> {
    if let Some(value) = value.as_str() {
        return Ok(serde_yaml::Value::String(value.to_string()));
    }
    if let Some(value) = value.as_bool() {
        return Ok(serde_yaml::Value::Bool(value));
    }
    if let Some(value) = value.as_integer() {
        return Ok(serde_yaml::Value::Number(value.into()));
    }
    if let Some(value) = value.as_float() {
        return Ok(serde_yaml::to_value(value)?);
    }
    if let Some(array) = value.as_array() {
        return array
            .iter()
            .map(toml_value_to_yaml)
            .collect::<Result<Vec<_>>>()
            .map(serde_yaml::Value::Sequence);
    }
    if let Some(table) = value.as_inline_table() {
        let mut map = serde_yaml::Mapping::new();
        for (key, value) in table.iter() {
            map.insert(
                serde_yaml::Value::String(key.to_string()),
                toml_value_to_yaml(value)?,
            );
        }
        return Ok(serde_yaml::Value::Mapping(map));
    }
    if let Some(value) = value.as_datetime() {
        return Ok(serde_yaml::Value::String(value.to_string()));
    }
    anyhow::bail!("unsupported TOML value {}", value)
}

fn yaml_to_toml_item(yaml: &serde_yaml::Value) -> Result<Item> {
    Ok(match yaml {
        serde_yaml::Value::Null => Item::None,
        serde_yaml::Value::Bool(v) => value(*v),
        serde_yaml::Value::Number(v) => {
            if let Some(i) = v.as_i64() {
                value(i)
            } else if let Some(f) = v.as_f64() {
                value(f)
            } else {
                anyhow::bail!("unsupported numeric TOML value");
            }
        }
        serde_yaml::Value::String(v) => value(v.clone()),
        serde_yaml::Value::Sequence(values) => {
            let mut array = Array::default();
            for value in values {
                match yaml_to_toml_item(value)? {
                    Item::Value(value) => array.push_formatted(value),
                    _ => anyhow::bail!("TOML arrays only support scalar values"),
                }
            }
            Item::Value(array.into())
        }
        serde_yaml::Value::Mapping(values) => {
            let mut table = Table::new();
            for (key, value) in values {
                let Some(key) = key.as_str() else {
                    anyhow::bail!("TOML table keys must be strings");
                };
                table[key] = yaml_to_toml_item(value)?;
            }
            Item::Table(table)
        }
        serde_yaml::Value::Tagged(_) => {
            anyhow::bail!("YAML tags are not supported in agent overrides")
        }
    })
}

fn parse_codex_mcps(document: &DocumentMut) -> Result<Vec<McpDefinition>> {
    let mut servers = Vec::new();
    let Some(mcp_item) = document.as_table().get("mcp_servers") else {
        return Ok(Vec::new());
    };
    let Some(mcp_table) = mcp_item.as_table() else {
        anyhow::bail!("Codex config mcp_servers must be a table");
    };

    for (name, item) in mcp_table.iter() {
        let Some(table) = item.as_table() else {
            anyhow::bail!("Codex MCP server {name} must be a table");
        };
        let enabled = table.get("enabled").and_then(Item::as_bool).unwrap_or(true);
        if let Some(command) = table.get("command").and_then(Item::as_str) {
            let args = table
                .get("args")
                .and_then(Item::as_array)
                .map(|array| {
                    array
                        .iter()
                        .map(|value| {
                            value.as_str().map(str::to_string).ok_or_else(|| {
                                anyhow::anyhow!("Codex MCP {name} args must be strings")
                            })
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default();
            servers.push(json!({
                "name": name,
                "enabled": enabled,
                "transport": "stdio",
                "command": command,
                "args": args,
                "env": table_to_json_object(table.get("env"))?,
            }));
        } else if let Some(url) = table.get("url").and_then(Item::as_str) {
            let mut headers = table_to_string_map(table.get("http_headers"))?;
            for (key, env_name) in table_to_string_map(table.get("env_http_headers"))? {
                headers.insert(key, format!("${env_name}"));
            }
            servers.push(json!({
                "name": name,
                "enabled": enabled,
                "transport": "http",
                "url": url,
                "headers": headers,
            }));
        } else {
            anyhow::bail!("Codex MCP server {name} must define command or url");
        }
    }

    crate::profile::mcp::parse_mcp_definitions(&serde_json::to_string(&servers)?)
}

fn table_to_json_object(item: Option<&Item>) -> Result<Value> {
    let map = table_to_string_map(item)?;
    Ok(json!(map))
}

fn table_to_string_map(item: Option<&Item>) -> Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    let Some(item) = item else {
        return Ok(map);
    };
    let Some(table) = item.as_table() else {
        anyhow::bail!("Codex MCP nested values must be tables");
    };
    for (key, value) in table.iter() {
        let value = value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Codex MCP value {key} must be a string"))?;
        map.insert(key.to_string(), value.to_string());
    }
    Ok(map)
}

impl McpDefinition {
    fn to_codex_table(&self) -> Result<Table> {
        let mut table = Table::new();
        table["enabled"] = value(self.enabled);
        match &self.transport {
            McpTransport::Stdio(stdio) => {
                table["command"] = value(stdio.command.as_str());
                if !stdio.args.is_empty() {
                    let mut args = Array::default();
                    for arg in &stdio.args {
                        args.push(arg.as_str());
                    }
                    table["args"] = value(args);
                }
                if !stdio.env.is_empty() {
                    table["env"] = string_map_table(&stdio.env);
                }
            }
            McpTransport::Http(http) => {
                table["url"] = value(http.url.as_str());
                let (literal, env_headers) = split_headers(&http.headers);
                if !literal.is_empty() {
                    table["http_headers"] = string_map_table(&literal);
                }
                if !env_headers.is_empty() {
                    table["env_http_headers"] = string_map_table(&env_headers);
                }
            }
        }
        Ok(table)
    }
}

fn string_map_table(values: &BTreeMap<String, String>) -> Item {
    let mut table = Table::new();
    for (key, value) in values {
        table[key] = value.clone().into();
    }
    Item::Table(table)
}

fn split_headers(
    headers: &BTreeMap<String, String>,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut literal = BTreeMap::new();
    let mut env_headers = BTreeMap::new();
    for (key, value) in headers {
        if let Some(env_name) = value.strip_prefix('$') {
            env_headers.insert(key.clone(), env_name.to_string());
        } else {
            literal.insert(key.clone(), value.clone());
        }
    }
    (literal, env_headers)
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
    struct CodexAdapter;

    impl HarnessTestAdapter for CodexAdapter {
        fn integration(&self) -> Box<dyn HarnessIntegration> {
            Box::new(CodexIntegration)
        }
        fn bin_name(&self) -> &'static str {
            "codex"
        }
        fn assert_mcp_cleared(&self, paths: &HarnessConfigPaths) {
            let config = fs::read_to_string(&paths.settings_file).unwrap();
            assert!(!config.contains("mcp_servers"));
        }
        fn write_malformed_native_config(&self, paths: &HarnessConfigPaths) {
            fs::write(&paths.settings_file, "malformed = [").unwrap();
        }
        fn supports_nested_commands(&self) -> bool {
            false
        }
        fn write_existing_native_settings(&self, paths: &HarnessConfigPaths) {
            fs::write(&paths.settings_file, "other = true\n").unwrap();
        }
        fn assert_native_settings_preserved(&self, paths: &HarnessConfigPaths) {
            let config = fs::read_to_string(&paths.settings_file).unwrap_or_default();
            assert!(config.contains("other = true"));
        }
        fn setup_native_config_for_import(&self, paths: &HarnessConfigPaths) {
            fs::write(
                &paths.settings_file,
                r#"
model = "gpt-imported"
approval_policy = "on-request"

[mcp_servers.local]
command = "server"
args = ["--flag"]
enabled = true

[mcp_servers.local.env]
TOKEN = "$TOKEN"

[mcp_servers.remote]
url = "https://mcp.example"

[mcp_servers.remote.http_headers]
X-Literal = "abc"

[mcp_servers.remote.env_http_headers]
Authorization = "TOKEN"
"#,
            )
            .unwrap();
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
                "model = \"drift-model\"\napproval_policy = \"drift-perm\"\n",
            )
            .unwrap();
        }
        fn assert_drift_saved(&self, config: &ProfileConfig) {
            assert_eq!(config.model_preference("codex"), "drift-model");
            assert_eq!(config.permission_preference("codex"), "drift-perm");
        }
        fn write_profile_config(&self, profile: &Path) {
            crate::integrations::test_suite::template::write_config(
                profile,
                r#"{
  "name": "work",
  "description": "",
  "models": {"codex": "gpt-5.2"},
  "permissions": {"codex": "on-request"}
}"#,
            );
        }
        fn assert_applied_native_config(&self, paths: &HarnessConfigPaths) {
            let config = fs::read_to_string(&paths.settings_file).unwrap();
            assert!(config.contains("model = \"gpt-5.2\""));
            assert!(config.contains("approval_policy = \"on-request\""));
            assert!(config.contains("[mcp_servers.local]"));
            assert!(config.contains("command = \"server\""));
            assert!(config.contains("[mcp_servers.disabled]"));
            assert!(config.contains("enabled = false"));
        }
    }

    crate::define_standard_harness_tests!(CodexAdapter);

    #[test]
    fn codex_render_ignores_other_harness_model_values() {
        let agent = SubAgent {
            name: "coder".to_string(),
            description: "Writes code".to_string(),
            model: Some(serde_yaml::from_str("opencode: gpt-5.2").unwrap()),
            tools: None,
            permission: Some(serde_yaml::from_str("opencode: ask").unwrap()),
            max_turns: None,
            harness: BTreeMap::new(),
            body: "Implement carefully.".to_string(),
        };

        let rendered = render_codex_agent(&agent).unwrap();

        assert!(!rendered.contents.contains("model ="));
        assert!(!rendered.contents.contains("approval_policy ="));
        assert!(rendered
            .contents
            .contains("developer_instructions = \"Implement carefully.\""));
    }

    #[test]
    fn codex_import_preserves_native_only_fields_under_harness_override() {
        let agent = codex_toml_to_neutral(
            r#"
name = "reviewer"
description = "Reviews code"
developer_instructions = "Review carefully."
model = "gpt-5.4"
approval_policy = "on-request"
model_reasoning_effort = "high"
sandbox_mode = "workspace-write"

[env]
RUST_LOG = "debug"
"#,
        )
        .unwrap();

        assert_eq!(
            agent
                .model
                .as_ref()
                .and_then(|model| model.get("codex"))
                .and_then(serde_yaml::Value::as_str),
            Some("gpt-5.4")
        );
        assert_eq!(
            agent
                .harness
                .get("codex")
                .and_then(|value| value.get("model_reasoning_effort"))
                .and_then(serde_yaml::Value::as_str),
            Some("high")
        );
        assert_eq!(
            agent
                .harness
                .get("codex")
                .and_then(|value| value.get("env"))
                .and_then(|env| env.get("RUST_LOG"))
                .and_then(serde_yaml::Value::as_str),
            Some("debug")
        );

        let rendered = render_codex_agent(&agent).unwrap();
        assert!(rendered
            .contents
            .contains("model_reasoning_effort = \"high\""));
        assert!(rendered
            .contents
            .contains("sandbox_mode = \"workspace-write\""));
        assert!(rendered.contents.contains("[env]"));
        assert!(rendered.contents.contains("RUST_LOG = \"debug\""));
    }
}
