use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde_json::{json, Map, Value};

use crate::harness::agents::{
    merge_harness_override, normalize_tool_permissions, parse_native_markdown_agent,
    render_native_markdown, select_harness_value, tools_to_allow_list,
    validate_lowercase_agent_slug, validate_merged_markdown_agent, NativeAgentParseOptions,
    SubAgent,
};
use crate::harness::artifact::{
    non_default_value, CommandMode, CommandsDirectory, HarnessArtifact, InstructionFile,
    JsonConfigFile, McpCodec, McpConfig, NativeConfig, PreferenceBinding, PreferenceCodec,
    PreferenceKind, SettingsPreferences, SkillsDirectory, SubagentCodec, SubagentsDirectory,
};
use crate::harness::integration::{
    AppEnvironment, HarnessConfigPaths, HarnessIntegration, ImportedPreference, ProfileRef,
};
use crate::harness::kind::HarnessKind;
use crate::profile::mcp::{McpDefinition, McpTransport, McpValue};

pub struct ClaudeIntegration;

impl HarnessIntegration for ClaudeIntegration {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Claude
    }

    fn default_config_dir(&self, env: &AppEnvironment) -> std::path::PathBuf {
        env.user_home.join(".claude")
    }

    fn paths_from_config_dir(&self, config_dir: std::path::PathBuf) -> Result<HarnessConfigPaths> {
        let mcp_file = config_dir
            .parent()
            .unwrap_or_else(|| std::path::Path::new(""))
            .join(".claude.json");
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

    fn paths_from_custom_config_dir(
        &self,
        config_dir: std::path::PathBuf,
    ) -> Result<HarnessConfigPaths> {
        let mut paths = self.paths_from_config_dir(config_dir.clone())?;
        paths.mcp_file = config_dir.join(".claude.json");
        Ok(paths)
    }

    fn artifacts(&self) -> Vec<Box<dyn HarnessArtifact>> {
        vec![
            Box::new(InstructionFile::new(|paths| &paths.instruction_target)),
            Box::new(SkillsDirectory::new(|paths| &paths.skills_dir)),
            Box::new(CommandsDirectory::new(
                |paths| &paths.commands_dir,
                CommandMode::RecursiveCopy,
            )),
            Box::new(SubagentsDirectory::new(
                |paths| &paths.agents_dir,
                ClaudeSubagentCodec,
            )),
            Box::new(McpConfig::new(
                JsonConfigFile::new(|paths| &paths.mcp_file).label("Claude MCP JSON"),
                ClaudeMcpCodec,
            )),
            Box::new(
                SettingsPreferences::new(
                    JsonConfigFile::new(|paths| &paths.settings_file).label("Claude settings JSON"),
                )
                .model(PreferenceBinding::JsonStringPointer { pointer: "/model" })
                .permission(PreferenceBinding::Custom(Box::new(ClaudePermissionCodec))),
            ),
        ]
    }
}

struct ClaudeSubagentCodec;

impl SubagentCodec for ClaudeSubagentCodec {
    fn native_file_name(&self, agent: &SubAgent) -> String {
        format!("{}.md", agent.name)
    }

    fn render(&self, agent: &SubAgent) -> Result<String> {
        render_claude_agent(agent)
    }

    fn should_import(&self, path: &Path) -> bool {
        path.extension().is_some_and(|ext| ext == "md")
    }

    fn parse(&self, path: &Path, contents: &str) -> Result<SubAgent> {
        let fallback_name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid agent path {}", path.display()))?;
        let agent = parse_native_markdown_agent(
            contents,
            fallback_name,
            "claude",
            NativeAgentParseOptions {
                require_name: true,
                permission_key: Some("permissionMode"),
                max_turns_key: Some("maxTurns"),
                role: None,
            },
            normalize_tool_permissions,
        )?;
        validate_lowercase_agent_slug(&agent.name, "Claude", &[])?;
        Ok(agent)
    }
}

struct ClaudeMcpCodec;

impl McpCodec for ClaudeMcpCodec {
    fn import(&self, config: &NativeConfig) -> Result<Vec<McpDefinition>> {
        import_claude_mcps(config.json_object("Claude JSON config")?)
    }

    fn apply(&self, config: &mut NativeConfig, definitions: &[McpDefinition]) -> Result<()> {
        let document = config.json_object_mut("Claude JSON config")?;
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

    fn preflight_apply(&self, config: &NativeConfig, definitions: &[McpDefinition]) -> Result<()> {
        if let Some(definition) = definitions.iter().find(|definition| !definition.enabled) {
            anyhow::bail!(
                "Claude cannot apply disabled MCP server {} to global configuration; Claude only supports disabledMcpServers in project settings",
                definition.name
            );
        }
        crate::profile::mcp::reject_native_reference_literals(definitions, "Claude", |value| {
            contains_claude_expansion(value)
        })?;
        reject_claude_untyped_expansions(definitions)?;
        import_claude_mcps(config.json_object("Claude JSON config")?)?;
        Ok(())
    }
}

struct ClaudePermissionCodec;

impl PreferenceCodec for ClaudePermissionCodec {
    fn import(&self, config: &NativeConfig) -> Result<ImportedPreference> {
        Ok(ImportedPreference::new(
            config
                .json_object("Claude JSON config")?
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
            patch_claude_permissions(config.json_object_mut("Claude JSON config")?, permission)?;
        }
        Ok(())
    }

    fn verify(&self, config: &NativeConfig, expected: Value) -> Result<()> {
        let Some(expected) = non_default_value(expected) else {
            return Ok(());
        };
        let permissions = config
            .json_object("Claude JSON config")?
            .get("permissions")
            .cloned()
            .unwrap_or(Value::Null);
        let actual = match &expected {
            Value::String(_) => permissions
                .get("defaultMode")
                .cloned()
                .unwrap_or(Value::Null),
            _ => permissions,
        };
        if actual != expected {
            anyhow::bail!("applied Claude permission preference does not match the profile");
        }
        Ok(())
    }
}

fn render_claude_agent(agent: &SubAgent) -> Result<String> {
    validate_lowercase_agent_slug(&agent.name, "Claude", &[])?;
    let mut map = crate::yaml::Mapping::new();
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
            crate::yaml::Value::Number(max_turns.into()),
        );
    }
    merge_harness_override(&mut map, agent, "claude")?;
    validate_merged_markdown_agent(&map, "Claude", true, "maxTurns")?;
    render_native_markdown(map, &agent.body)
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
        let mcp_type = match table.get("type") {
            Some(Value::String(value)) => value.as_str(),
            Some(_) => anyhow::bail!("Claude MCP server {name} type must be a string"),
            None => "stdio",
        };
        let allowed: &[&str] = if mcp_type == "stdio" {
            &["type", "command", "args", "env"]
        } else {
            &["type", "url", "headers"]
        };
        if let Some(field) = table
            .keys()
            .find(|field| !allowed.contains(&field.as_str()))
        {
            anyhow::bail!(
                "Claude MCP server {name} uses unsupported field {field}; import or replacement would lose native security settings"
            );
        }

        if mcp_type == "stdio" {
            let command = table.get("command").and_then(Value::as_str).unwrap_or("");
            if contains_claude_expansion(command) {
                anyhow::bail!(
                    "Claude MCP {name} command contains an unrepresentable environment expansion"
                );
            }
            let args = match table.get("args") {
                None => Vec::new(),
                Some(Value::Array(array)) => array
                    .iter()
                    .map(|value| {
                        value.as_str().map(str::to_string).ok_or_else(|| {
                            anyhow::anyhow!("Claude MCP {name} args must be strings")
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
                Some(_) => anyhow::bail!("Claude MCP {name} args must be an array"),
            };
            if args.iter().any(|value| contains_claude_expansion(value)) {
                anyhow::bail!(
                    "Claude MCP {name} args contain an unrepresentable environment expansion"
                );
            }

            let env = if let Some(env) = table.get("env") {
                let Some(env_obj) = env.as_object() else {
                    anyhow::bail!("Claude MCP env must be an object");
                };
                let mut map = BTreeMap::new();
                for (k, v) in env_obj {
                    if let Some(s) = v.as_str() {
                        map.insert(k.clone(), parse_braced_env(s)?);
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
                "enabled": true,
                "transport": "stdio",
                "command": command,
                "args": args,
                "env": env,
            }));
        } else if mcp_type == "http" {
            let url = table.get("url").and_then(Value::as_str).unwrap_or("");
            if contains_claude_expansion(url) {
                anyhow::bail!(
                    "Claude MCP {name} URL contains an unrepresentable environment expansion"
                );
            }

            let headers = if let Some(headers) = table.get("headers") {
                let Some(headers_obj) = headers.as_object() else {
                    anyhow::bail!("Claude MCP headers must be an object");
                };
                let mut map = BTreeMap::new();
                for (k, v) in headers_obj {
                    if let Some(s) = v.as_str() {
                        map.insert(k.clone(), parse_braced_env(s)?);
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
                "enabled": true,
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
        match &self.transport {
            McpTransport::Stdio(stdio) => {
                map.insert("type".to_string(), json!("stdio"));
                map.insert("command".to_string(), json!(stdio.command));
                if !stdio.args.is_empty() {
                    map.insert("args".to_string(), json!(stdio.args));
                }
                if !stdio.env.is_empty() {
                    map.insert("env".to_string(), json!(render_braced_env(&stdio.env)));
                }
            }
            McpTransport::Http(http) => {
                map.insert("type".to_string(), json!("http"));
                map.insert("url".to_string(), json!(http.url));
                if !http.headers.is_empty() {
                    map.insert(
                        "headers".to_string(),
                        json!(render_braced_env(&http.headers)),
                    );
                }
            }
        }
        Ok(Value::Object(map))
    }
}

fn parse_braced_env(value: &str) -> Result<McpValue> {
    if let Some(name) = value
        .strip_prefix("${")
        .and_then(|value| value.strip_suffix('}'))
    {
        McpValue::env(name)
    } else if contains_claude_expansion(value) {
        anyhow::bail!("Claude MCP value contains an embedded or default environment expansion that the neutral schema cannot represent")
    } else {
        Ok(McpValue::literal(value))
    }
}

fn contains_claude_expansion(value: &str) -> bool {
    let mut remaining = value;
    while let Some(start) = remaining.find("${") {
        remaining = &remaining[start + 2..];
        let Some(end) = remaining.find('}') else {
            return false;
        };
        let expression = &remaining[..end];
        let name = expression
            .split_once(":-")
            .map_or(expression, |(name, _)| name);
        if !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return true;
        }
        remaining = &remaining[end + 1..];
    }
    false
}

fn reject_claude_untyped_expansions(definitions: &[McpDefinition]) -> Result<()> {
    for definition in definitions {
        let invalid = match &definition.transport {
            McpTransport::Stdio(stdio) => {
                contains_claude_expansion(&stdio.command)
                    || stdio
                        .args
                        .iter()
                        .any(|value| contains_claude_expansion(value))
            }
            McpTransport::Http(http) => contains_claude_expansion(&http.url),
        };
        if invalid {
            anyhow::bail!("Claude MCP server {} contains an environment expansion outside a typed env or header value", definition.name);
        }
    }
    Ok(())
}

fn render_braced_env(values: &BTreeMap<String, McpValue>) -> BTreeMap<String, String> {
    values
        .iter()
        .map(|(key, value)| {
            let value = match value {
                McpValue::Literal(value) => value.clone(),
                McpValue::Env(name) => format!("${{{name}}}"),
            };
            (key.clone(), value)
        })
        .collect()
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
        fn supports_disabled_mcp(&self) -> bool {
            false
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
                r#"{"model": "opus", "permissions": {"defaultMode": "acceptEdits"}}"#,
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
                r#"{"model": "drift-model", "permissions": {"defaultMode": "drift-perm"}}"#,
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
            assert!(!mcp.contains(r#""enabled""#));
        }
    }

    crate::define_standard_harness_tests!(ClaudeAdapter);

    #[test]
    fn default_and_custom_mcp_paths_are_distinct() {
        let temp = tempfile::tempdir().unwrap();
        let env = AppEnvironment {
            lazyagents_home: temp.path().join("lazyagents"),
            user_home: temp.path().join("user"),
            path_entries: Vec::new(),
        };
        assert_eq!(
            ClaudeIntegration.paths(&env).unwrap().mcp_file,
            env.user_home.join(".claude.json")
        );
        let custom = temp.path().join("claude.work.v2");
        assert_eq!(
            ClaudeIntegration
                .paths_from_custom_config_dir(custom.clone())
                .unwrap()
                .mcp_file,
            custom.join(".claude.json")
        );
    }

    #[test]
    fn disabled_and_unsupported_claude_mcps_are_rejected() {
        let disabled = crate::profile::mcp::parse_mcp_definitions(
            r#"[{"name":"off","enabled":false,"transport":"stdio","command":"x"}]"#,
        )
        .unwrap();
        assert!(ClaudeMcpCodec
            .preflight_apply(&NativeConfig::Json(json!({})), &disabled)
            .unwrap_err()
            .to_string()
            .contains("disabledMcpServers"));

        let error = import_claude_mcps(
            json!({"mcpServers":{"x":{"command":"x","oauth":{}}}})
                .as_object()
                .unwrap(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("unsupported field oauth"));
    }

    #[test]
    fn claude_mcp_environment_references_round_trip() {
        let definitions = crate::profile::mcp::parse_mcp_definitions(
            r#"[{"name":"x","transport":"stdio","command":"x","env":{"TOKEN":{"env":"TOKEN"},"LITERAL":"$TOKEN"}}]"#,
        )
        .unwrap();
        let mut config = NativeConfig::Json(json!({}));
        ClaudeMcpCodec.apply(&mut config, &definitions).unwrap();
        assert_eq!(ClaudeMcpCodec.import(&config).unwrap(), definitions);
        let NativeConfig::Json(config) = config else {
            unreachable!()
        };
        assert_eq!(config["mcpServers"]["x"]["env"]["TOKEN"], "${TOKEN}");
        assert_eq!(config["mcpServers"]["x"]["env"]["LITERAL"], "$TOKEN");
    }

    #[test]
    fn claude_rejects_malformed_mcp_types_and_ambiguous_literals() {
        assert!(import_claude_mcps(
            json!({"mcpServers":{"x":{"type":false}}})
                .as_object()
                .unwrap()
        )
        .is_err());
        let definitions = crate::profile::mcp::parse_mcp_definitions(
            r#"[{"name":"x","transport":"stdio","command":"x","env":{"TOKEN":"${TOKEN}"}}]"#,
        )
        .unwrap();
        assert!(ClaudeMcpCodec
            .preflight_apply(&NativeConfig::Json(json!({})), &definitions)
            .is_err());
        for value in ["Bearer ${TOKEN}", "${TOKEN:-fallback}"] {
            let native = json!({"mcpServers":{"x":{"command":"x","env":{"TOKEN":value}}}});
            assert!(import_claude_mcps(native.as_object().unwrap()).is_err());
        }
        for args in [json!("--flag"), json!({"flag":true}), json!(null)] {
            let native = json!({"mcpServers":{"x":{"command":"x","args":args}}});
            assert!(import_claude_mcps(native.as_object().unwrap()).is_err());
        }
    }

    #[test]
    fn claude_import_rejects_names_the_renderer_cannot_emit() {
        assert!(ClaudeSubagentCodec
            .parse(
                Path::new("Reviewer.md"),
                "---\nname: Reviewer\ndescription: Reviews\n---\nBody\n"
            )
            .is_err());
    }

    #[test]
    fn claude_custom_permission_verifies_effective_value() {
        let config = NativeConfig::Json(json!({"permissions":{"defaultMode":"deny"}}));
        assert!(ClaudePermissionCodec
            .verify(&config, json!("allow"))
            .is_err());
    }
}
