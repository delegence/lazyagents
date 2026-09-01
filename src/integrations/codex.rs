use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::json;
use toml_edit::{value, Array, DocumentMut, InlineTable, Item, Table, Value as TomlValue};

use crate::harness::agents::{
    harness_scoped_value, select_harness_value, yaml_scalar_string, SubAgent,
};
use crate::harness::artifact::{
    CommandMode, CommandsDirectory, HarnessArtifact, InstructionFile, McpCodec, McpConfig,
    NativeConfig, PreferenceBinding, PreferenceCodec, PreferenceKind, SettingsPreferences,
    SkillsDirectory, SubagentCodec, SubagentsDirectory, TomlConfigFile,
};
use crate::harness::integration::{
    AppEnvironment, HarnessConfigPaths, HarnessIntegration, ImportedPreference, ProfileRef,
};
use crate::harness::kind::HarnessKind;
use crate::profile::mcp::{McpDefinition, McpTransport, McpValue};

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
                CommandMode::FlatCopy,
            )),
            Box::new(SubagentsDirectory::new(
                |paths| &paths.agents_dir,
                CodexSubagentCodec,
            )),
            Box::new(McpConfig::new(
                TomlConfigFile::new(|paths| &paths.settings_file).label("Codex config TOML"),
                CodexMcpCodec,
            )),
            Box::new(
                SettingsPreferences::new(
                    TomlConfigFile::new(|paths| &paths.settings_file).label("Codex config TOML"),
                )
                .model(PreferenceBinding::TomlKey { key: "model" })
                .permission(PreferenceBinding::Custom(Box::new(CodexApprovalCodec))),
            ),
        ]
    }
}

struct CodexApprovalCodec;

impl PreferenceCodec for CodexApprovalCodec {
    fn import(&self, config: &NativeConfig) -> Result<ImportedPreference> {
        let NativeConfig::Toml(document) = config else {
            anyhow::bail!("Codex approval preference requires TOML config");
        };
        let Some(item) = document.get("approval_policy") else {
            return Ok(ImportedPreference::default_value());
        };
        let value = serde_json::to_value(toml_item_to_yaml(item)?)?;
        Ok(ImportedPreference::new(validate_codex_approval(value)?))
    }

    fn apply(
        &self,
        config: &mut NativeConfig,
        profile: &ProfileRef,
        _preference_kind: PreferenceKind,
    ) -> Result<()> {
        let permission = crate::profile::read_profile_config(&profile.path)?
            .permission_preference(&profile.harness_id);
        let Some(permission) = crate::harness::artifact::non_default_value(permission) else {
            return Ok(());
        };
        let NativeConfig::Toml(document) = config else {
            anyhow::bail!("Codex approval preference requires TOML config");
        };
        let permission = validate_codex_approval(permission)?;
        document["approval_policy"] = yaml_to_toml_item(&crate::yaml::to_value(permission)?)?;
        Ok(())
    }

    fn preflight(&self, expected: serde_json::Value) -> Result<()> {
        if let Some(expected) = crate::harness::artifact::non_default_value(expected) {
            validate_codex_approval(expected)?;
        }
        Ok(())
    }

    fn verify(&self, config: &NativeConfig, expected: serde_json::Value) -> Result<()> {
        let Some(expected) = crate::harness::artifact::non_default_value(expected) else {
            return Ok(());
        };
        let expected = validate_codex_approval(expected)?;
        if self.import(config)?.into_value() != expected {
            anyhow::bail!("applied Codex approval policy does not match the profile");
        }
        Ok(())
    }
}

fn validate_codex_approval(value: serde_json::Value) -> Result<serde_json::Value> {
    match value {
        serde_json::Value::String(value)
            if matches!(value.as_str(), "untrusted" | "on-request" | "never") =>
        {
            Ok(serde_json::Value::String(value))
        }
        serde_json::Value::String(value) if value == "on-failure" => {
            Ok(serde_json::Value::String("on-request".to_string()))
        }
        serde_json::Value::Object(outer) => {
            if outer.len() != 1 || !outer.contains_key("granular") {
                anyhow::bail!("Codex approval policy object must contain only granular");
            }
            let granular = outer["granular"].as_object().ok_or_else(|| {
                anyhow::anyhow!("Codex granular approval policy must be an object")
            })?;
            let allowed = [
                "sandbox_approval",
                "rules",
                "mcp_elicitations",
                "skill_approval",
                "request_permissions",
            ];
            if let Some(key) = granular.keys().find(|key| !allowed.contains(&key.as_str())) {
                anyhow::bail!("Codex granular approval policy has unknown field {key}");
            }
            for key in ["sandbox_approval", "rules", "mcp_elicitations"] {
                if !granular.get(key).is_some_and(serde_json::Value::is_boolean) {
                    anyhow::bail!("Codex granular approval field {key} must be a boolean");
                }
            }
            for key in ["skill_approval", "request_permissions"] {
                if granular.get(key).is_some_and(|value| !value.is_boolean()) {
                    anyhow::bail!("Codex granular approval field {key} must be a boolean");
                }
            }
            Ok(serde_json::Value::Object(outer))
        }
        _ => anyhow::bail!(
            "Codex approval policy must be untrusted, on-request, never, or a valid granular object"
        ),
    }
}

struct CodexSubagentCodec;

impl SubagentCodec for CodexSubagentCodec {
    fn native_file_name(&self, agent: &SubAgent) -> String {
        format!("{}.toml", agent.name)
    }

    fn render(&self, agent: &SubAgent) -> Result<String> {
        render_codex_agent(agent)
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

    fn preflight_apply(&self, config: &NativeConfig, definitions: &[McpDefinition]) -> Result<()> {
        let NativeConfig::Toml(document) = config else {
            anyhow::bail!("Codex MCP codec requires TOML config");
        };
        parse_codex_mcps(document)?;
        crate::profile::mcp::reject_native_reference_literals(definitions, "Codex", |_| false)?;
        Ok(())
    }
}

fn render_codex_agent(agent: &SubAgent) -> Result<String> {
    if agent.body.trim().is_empty() {
        anyhow::bail!("Codex developer instructions cannot be blank");
    }
    let mut document = DocumentMut::new();
    document["name"] = value(agent.name.clone());
    document["description"] = value(agent.description.clone());
    document["developer_instructions"] = value(agent.body.clone());
    if let Some(model) =
        select_harness_value(agent.model.as_ref(), "codex").and_then(yaml_scalar_string)
    {
        document["model"] = value(model);
    }
    if let Some(permission) = select_harness_value(agent.permission.as_ref(), "codex") {
        if !matches!(
            permission,
            crate::yaml::Value::String(_) | crate::yaml::Value::Mapping(_)
        ) {
            anyhow::bail!("Codex sub-agent approval policy must be a string or object");
        }
        document["approval_policy"] = yaml_to_toml_item(permission)?;
    }
    if let Some(override_value) = agent.harness.get("codex") {
        let crate::yaml::Value::Mapping(map) = override_value else {
            anyhow::bail!("Codex harness override must be an object");
        };
        for (key, val) in map {
            let Some(key) = key.as_str() else {
                anyhow::bail!("Codex harness override keys must be strings");
            };
            document[key] = yaml_to_toml_item(val)?;
        }
    }
    if let Some(item) = document.get("approval_policy") {
        validate_codex_approval(serde_json::to_value(toml_item_to_yaml(item)?)?)?;
    }
    for key in ["name", "description", "developer_instructions"] {
        let value = document
            .get(key)
            .and_then(Item::as_str)
            .ok_or_else(|| anyhow::anyhow!("Codex agent {} {key} must be a string", agent.name))?;
        if value.trim().is_empty() {
            anyhow::bail!("Codex agent {} {key} must not be blank", agent.name);
        }
    }
    Ok(document.to_string())
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
    if name.trim().is_empty() {
        anyhow::bail!("Codex agent name must not be blank");
    }
    if description.trim().is_empty() {
        anyhow::bail!("Codex agent {name} description must not be blank");
    }
    if body.trim().is_empty() {
        anyhow::bail!("Codex agent {name} has blank developer_instructions");
    }
    let model = harness_scoped_value(
        "codex",
        document
            .get("model")
            .and_then(Item::as_str)
            .map(|model| crate::yaml::Value::String(model.to_string())),
    );
    let permission = document
        .get("approval_policy")
        .map(toml_item_to_yaml)
        .transpose()?
        .map(|permission| -> Result<_> {
            let value = validate_codex_approval(serde_json::to_value(permission)?)?;
            Ok(crate::yaml::to_value(value)?)
        })
        .transpose()?
        .and_then(|permission| harness_scoped_value("codex", Some(permission)));
    let mut codex_overrides = crate::yaml::Mapping::new();
    for (key, item) in document.as_table().iter() {
        if matches!(
            key,
            "name" | "description" | "developer_instructions" | "model" | "approval_policy"
        ) {
            continue;
        }
        codex_overrides.insert(
            crate::yaml::Value::String(key.to_string()),
            toml_item_to_yaml(item)
                .with_context(|| format!("failed to import Codex agent field {key}"))?,
        );
    }
    let mut harness = BTreeMap::new();
    if !codex_overrides.is_empty() {
        harness.insert(
            "codex".to_string(),
            crate::yaml::Value::Mapping(codex_overrides),
        );
    }
    Ok(SubAgent {
        name,
        description,
        model,
        tools: None,
        permission,
        max_turns: None,
        harness,
        body,
    })
}

fn toml_item_to_yaml(item: &Item) -> Result<crate::yaml::Value> {
    if item.is_none() {
        return Ok(crate::yaml::Value::Null);
    }
    if let Some(value) = item.as_value() {
        return toml_value_to_yaml(value);
    }
    if let Some(table) = item.as_table() {
        let mut map = crate::yaml::Mapping::new();
        for (key, value) in table.iter() {
            map.insert(
                crate::yaml::Value::String(key.to_string()),
                toml_item_to_yaml(value)?,
            );
        }
        return Ok(crate::yaml::Value::Mapping(map));
    }
    anyhow::bail!("unsupported TOML value {}", item.to_string().trim())
}

fn toml_value_to_yaml(value: &toml_edit::Value) -> Result<crate::yaml::Value> {
    if let Some(value) = value.as_str() {
        return Ok(crate::yaml::Value::String(value.to_string()));
    }
    if let Some(value) = value.as_bool() {
        return Ok(crate::yaml::Value::Bool(value));
    }
    if let Some(value) = value.as_integer() {
        return Ok(crate::yaml::Value::Number(value.into()));
    }
    if let Some(value) = value.as_float() {
        return Ok(crate::yaml::to_value(value)?);
    }
    if let Some(array) = value.as_array() {
        return array
            .iter()
            .map(toml_value_to_yaml)
            .collect::<Result<Vec<_>>>()
            .map(crate::yaml::Value::Sequence);
    }
    if let Some(table) = value.as_inline_table() {
        let mut map = crate::yaml::Mapping::new();
        for (key, value) in table.iter() {
            map.insert(
                crate::yaml::Value::String(key.to_string()),
                toml_value_to_yaml(value)?,
            );
        }
        return Ok(crate::yaml::Value::Mapping(map));
    }
    if let Some(value) = value.as_datetime() {
        return Ok(crate::yaml::Value::String(value.to_string()));
    }
    anyhow::bail!("unsupported TOML value {}", value)
}

fn yaml_to_toml_item(yaml: &crate::yaml::Value) -> Result<Item> {
    Ok(match yaml {
        crate::yaml::Value::Null => anyhow::bail!("TOML cannot represent null"),
        crate::yaml::Value::Bool(v) => value(*v),
        crate::yaml::Value::Number(v) => {
            if let Some(i) = v.as_i64() {
                value(i)
            } else if let Some(f) = v.as_f64() {
                value(f)
            } else {
                anyhow::bail!("unsupported numeric TOML value");
            }
        }
        crate::yaml::Value::String(v) => value(v.clone()),
        crate::yaml::Value::Sequence(values) => {
            let mut array = Array::default();
            for value in values {
                array.push_formatted(yaml_to_toml_value(value)?);
            }
            Item::Value(array.into())
        }
        crate::yaml::Value::Mapping(values) => {
            let mut table = Table::new();
            for (key, value) in values {
                let Some(key) = key.as_str() else {
                    anyhow::bail!("TOML table keys must be strings");
                };
                table[key] = yaml_to_toml_item(value)?;
            }
            Item::Table(table)
        }
        crate::yaml::Value::Tagged(_) => {
            anyhow::bail!("YAML tags are not supported in agent overrides")
        }
    })
}

fn yaml_to_toml_value(yaml: &crate::yaml::Value) -> Result<TomlValue> {
    Ok(match yaml {
        crate::yaml::Value::Null => anyhow::bail!("TOML cannot represent null"),
        crate::yaml::Value::Bool(value) => TomlValue::from(*value),
        crate::yaml::Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                TomlValue::from(integer)
            } else if let Some(float) = value.as_f64() {
                TomlValue::from(float)
            } else {
                anyhow::bail!("unsupported numeric TOML value");
            }
        }
        crate::yaml::Value::String(value) => TomlValue::from(value.clone()),
        crate::yaml::Value::Sequence(values) => {
            let mut array = Array::default();
            for value in values {
                array.push_formatted(yaml_to_toml_value(value)?);
            }
            TomlValue::Array(array)
        }
        crate::yaml::Value::Mapping(values) => {
            let mut table = InlineTable::new();
            for (key, value) in values {
                let Some(key) = key.as_str() else {
                    anyhow::bail!("TOML inline table keys must be strings");
                };
                table.insert(key, yaml_to_toml_value(value)?);
            }
            TomlValue::InlineTable(table)
        }
        crate::yaml::Value::Tagged(_) => {
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
        let enabled = match table.get("enabled") {
            Some(item) => item.as_bool().ok_or_else(|| {
                anyhow::anyhow!("Codex MCP server {name} enabled must be a boolean")
            })?,
            None => true,
        };
        let allowed: &[&str] = if table.get("command").is_some() {
            &["enabled", "command", "args", "env", "env_vars"]
        } else {
            &["enabled", "url", "http_headers", "env_http_headers"]
        };
        if let Some(field) = table
            .iter()
            .map(|(key, _)| key)
            .find(|key| !allowed.contains(key))
        {
            anyhow::bail!(
                "Codex MCP server {name} uses unsupported field {field}; import or replacement would lose native security settings"
            );
        }
        if let Some(command) = table.get("command").and_then(Item::as_str) {
            let args = match table.get("args") {
                None => Vec::new(),
                Some(item) if item.is_array() => {
                    let array = item.as_array().expect("checked array");
                    array
                        .iter()
                        .map(|value| {
                            value.as_str().map(str::to_string).ok_or_else(|| {
                                anyhow::anyhow!("Codex MCP {name} args must be strings")
                            })
                        })
                        .collect::<Result<Vec<_>>>()?
                }
                Some(_) => anyhow::bail!("Codex MCP {name} args must be an array"),
            };
            servers.push(json!({
                "name": name,
                "enabled": enabled,
                "transport": "stdio",
                "command": command,
                "args": args,
                "env": codex_stdio_env(table)?,
            }));
        } else if let Some(url) = table.get("url").and_then(Item::as_str) {
            let headers = table_to_string_map(table.get("http_headers"))?;
            let mut typed_headers = headers
                .into_iter()
                .map(|(key, value)| (key, McpValue::literal(value)))
                .collect::<BTreeMap<_, _>>();
            for (key, env_name) in table_to_string_map(table.get("env_http_headers"))? {
                if typed_headers.contains_key(&key) {
                    anyhow::bail!(
                        "Codex MCP server {name} defines header {key} in both http_headers and env_http_headers"
                    );
                }
                typed_headers.insert(key, McpValue::env(env_name)?);
            }
            servers.push(json!({
                "name": name,
                "enabled": enabled,
                "transport": "http",
                "url": url,
                "headers": typed_headers,
            }));
        } else {
            anyhow::bail!("Codex MCP server {name} must define command or url");
        }
    }

    crate::profile::mcp::parse_mcp_definitions(&serde_json::to_string(&servers)?)
}

fn codex_stdio_env(table: &Table) -> Result<BTreeMap<String, McpValue>> {
    let mut env = table_to_string_map(table.get("env"))?
        .into_iter()
        .map(|(key, value)| (key, McpValue::literal(value)))
        .collect::<BTreeMap<_, _>>();
    let literal_names = env.keys().cloned().collect::<BTreeSet<_>>();
    let mut inherited_names = BTreeSet::new();
    if let Some(env_vars) = table.get("env_vars") {
        let array = env_vars
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Codex MCP env_vars must be an array"))?;
        for value in array {
            let name = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Codex MCP env_vars values must be strings"))?;
            if literal_names.contains(name) {
                anyhow::bail!("Codex MCP defines environment key {name} in both env and env_vars");
            }
            if !inherited_names.insert(name.to_string()) {
                anyhow::bail!("Codex MCP env_vars contains duplicate name {name}");
            }
            env.insert(name.to_string(), McpValue::env(name)?);
        }
    }
    Ok(env)
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
                    let mut literal = BTreeMap::new();
                    let mut env_vars = Array::default();
                    for (key, value) in &stdio.env {
                        match value {
                            McpValue::Literal(value) => {
                                literal.insert(key.clone(), value.clone());
                            }
                            McpValue::Env(name) if name == key => env_vars.push(name.as_str()),
                            McpValue::Env(name) => anyhow::bail!(
                                "Codex stdio MCP environment alias {key} -> {name} cannot be represented"
                            ),
                        }
                    }
                    if !literal.is_empty() {
                        table["env"] = string_map_table(&literal);
                    }
                    if !env_vars.is_empty() {
                        table["env_vars"] = value(env_vars);
                    }
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
    headers: &BTreeMap<String, McpValue>,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut literal = BTreeMap::new();
    let mut env_headers = BTreeMap::new();
    for (key, value) in headers {
        match value {
            McpValue::Literal(value) => {
                literal.insert(key.clone(), value.clone());
            }
            McpValue::Env(name) => {
                env_headers.insert(key.clone(), name.clone());
            }
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
                .contains("\"env\": \"TOKEN\""));
        }
        fn setup_drift_native_config(&self, paths: &HarnessConfigPaths) {
            fs::write(
                &paths.settings_file,
                "model = \"drift-model\"\napproval_policy = \"never\"\n",
            )
            .unwrap();
        }
        fn assert_drift_saved(&self, config: &ProfileConfig) {
            assert_eq!(config.model_preference("codex"), "drift-model");
            assert_eq!(config.permission_preference("codex"), "never");
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
            model: Some(crate::yaml::from_str("opencode: gpt-5.2").unwrap()),
            tools: None,
            permission: Some(crate::yaml::from_str("opencode: ask").unwrap()),
            max_turns: None,
            harness: BTreeMap::new(),
            body: "Implement carefully.".to_string(),
        };

        let rendered = render_codex_agent(&agent).unwrap();

        assert!(!rendered.contains("model ="));
        assert!(!rendered.contains("approval_policy ="));
        assert!(rendered.contains("developer_instructions = \"Implement carefully.\""));
    }

    #[test]
    fn codex_does_not_render_unsupported_neutral_max_turns() {
        let agent = SubAgent {
            name: "coder".to_string(),
            description: "Writes code".to_string(),
            model: None,
            tools: None,
            permission: None,
            max_turns: Some(u64::MAX),
            harness: Default::default(),
            body: "Write code.".to_string(),
        };

        let rendered = render_codex_agent(&agent).unwrap();

        assert!(!rendered.contains("max_turns"));
    }

    #[test]
    fn yaml_sequences_support_inline_tables_and_nested_arrays() {
        let yaml: crate::yaml::Value =
            crate::yaml::from_str("- name: TOKEN\n  source: local\n  flags: [true, 2, [nested]]\n")
                .unwrap();

        let item = yaml_to_toml_item(&yaml).unwrap();
        let array = item.as_array().unwrap();
        let table = array.get(0).unwrap().as_inline_table().unwrap();
        assert_eq!(table.get("name").unwrap().as_str(), Some("TOKEN"));
        assert!(table.get("flags").unwrap().as_array().is_some());
        assert!(yaml_to_toml_item(&crate::yaml::Value::Null).is_err());
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
max_turns = 7

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
                .and_then(crate::yaml::Value::as_str),
            Some("gpt-5.4")
        );
        assert_eq!(
            agent
                .harness
                .get("codex")
                .and_then(|value| value.get("model_reasoning_effort"))
                .and_then(crate::yaml::Value::as_str),
            Some("high")
        );
        assert_eq!(
            agent
                .harness
                .get("codex")
                .and_then(|value| value.get("env"))
                .and_then(|env| env.get("RUST_LOG"))
                .and_then(crate::yaml::Value::as_str),
            Some("debug")
        );
        assert_eq!(
            agent
                .harness
                .get("codex")
                .and_then(|value| value.get("max_turns"))
                .and_then(crate::yaml::Value::as_u64),
            Some(7)
        );

        let rendered = render_codex_agent(&agent).unwrap();
        assert!(rendered.contains("model_reasoning_effort = \"high\""));
        assert!(rendered.contains("sandbox_mode = \"workspace-write\""));
        assert!(rendered.contains("[env]"));
        assert!(rendered.contains("RUST_LOG = \"debug\""));
        assert!(rendered.contains("max_turns = 7"));
    }

    #[test]
    fn codex_approval_codec_preserves_granular_policy_objects() {
        let document = r#"approval_policy = { granular = { sandbox_approval = true, rules = false, mcp_elicitations = true, request_permissions = false } }"#
            .parse::<DocumentMut>()
            .unwrap();

        let imported = CodexApprovalCodec
            .import(&NativeConfig::Toml(document))
            .unwrap()
            .into_value();

        assert_eq!(imported["granular"]["sandbox_approval"], true);
        assert_eq!(imported["granular"]["request_permissions"], false);
    }

    #[test]
    fn codex_agent_round_trips_granular_approval_policy() {
        let imported = codex_toml_to_neutral(
            r#"
name = "reviewer"
description = "Reviews code"
developer_instructions = "Review carefully."
approval_policy = { granular = { sandbox_approval = true, rules = false, mcp_elicitations = true } }
"#,
        )
        .unwrap();

        assert_eq!(
            imported
                .permission
                .as_ref()
                .and_then(|value| value.get("codex"))
                .and_then(|value| value.get("granular"))
                .and_then(|value| value.get("sandbox_approval"))
                .and_then(crate::yaml::Value::as_bool),
            Some(true)
        );
        assert!(render_codex_agent(&imported)
            .unwrap()
            .contains("sandbox_approval = true"));
    }

    #[test]
    fn codex_stdio_environment_references_round_trip() {
        let definitions = crate::profile::mcp::parse_mcp_definitions(
            r#"[{"name":"x","transport":"stdio","command":"x","env":{"TOKEN":{"env":"TOKEN"},"LITERAL":"$TOKEN"}}]"#,
        )
        .unwrap();
        let mut config = NativeConfig::Toml(DocumentMut::new());
        CodexMcpCodec.apply(&mut config, &definitions).unwrap();
        assert_eq!(CodexMcpCodec.import(&config).unwrap(), definitions);
        let NativeConfig::Toml(config) = config else {
            unreachable!()
        };
        let text = config.to_string();
        assert!(text.contains("env_vars = [\"TOKEN\"]"));
        assert!(text.contains("LITERAL = \"$TOKEN\""));
    }

    #[test]
    fn codex_rejects_malformed_enabled_and_blank_override_instructions() {
        let config = "[mcp_servers.x]\ncommand = \"x\"\nenabled = \"yes\"\n"
            .parse::<DocumentMut>()
            .unwrap();
        assert!(parse_codex_mcps(&config).is_err());

        let mut harness = BTreeMap::new();
        harness.insert(
            "codex".to_string(),
            crate::yaml::from_str("developer_instructions: ''").unwrap(),
        );
        let agent = SubAgent {
            name: "reviewer".to_string(),
            description: "Reviews".to_string(),
            model: None,
            tools: None,
            permission: None,
            max_turns: None,
            harness,
            body: "Body".to_string(),
        };
        assert!(render_codex_agent(&agent).is_err());
        let mut scalar = agent.clone();
        scalar.harness.insert(
            "codex".to_string(),
            crate::yaml::Value::String("bad".to_string()),
        );
        assert!(render_codex_agent(&scalar).is_err());
    }

    #[test]
    fn codex_rejects_malformed_args_duplicate_sources_and_approval_shapes() {
        for args in ["args = \"bad\"", "args = { bad = true }", "args = 1"] {
            let config = format!("[mcp_servers.x]\ncommand = \"x\"\n{args}\n")
                .parse::<DocumentMut>()
                .unwrap();
            assert!(parse_codex_mcps(&config).is_err());
        }
        for text in [
            "[mcp_servers.x]\ncommand = \"x\"\nenv_vars = [\"TOKEN\"]\n[mcp_servers.x.env]\nTOKEN = \"literal\"\n",
            "[mcp_servers.x]\nurl = \"https://example.com\"\n[mcp_servers.x.http_headers]\nAuthorization = \"literal\"\n[mcp_servers.x.env_http_headers]\nAuthorization = \"TOKEN\"\n",
        ] {
            assert!(parse_codex_mcps(&text.parse::<DocumentMut>().unwrap()).is_err());
        }
        for value in [
            json!(true),
            json!(["never"]),
            json!({"unknown": true}),
            json!({"granular":{"sandbox_approval":true,"rules":false}}),
            json!({"granular":{"sandbox_approval":"yes","rules":false,"mcp_elicitations":true}}),
        ] {
            assert!(validate_codex_approval(value).is_err());
        }
        assert_eq!(
            validate_codex_approval(json!("on-failure")).unwrap(),
            json!("on-request")
        );
    }

    #[test]
    fn codex_custom_approval_verifies_effective_value() {
        let config = NativeConfig::Toml("approval_policy = \"never\"\n".parse().unwrap());
        assert!(CodexApprovalCodec
            .verify(&config, json!("on-request"))
            .is_err());
    }
}
