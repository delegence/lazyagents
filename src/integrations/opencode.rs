use std::collections::BTreeMap;
use std::path::Path;

#[cfg(test)]
use anyhow::Context;
use anyhow::Result;
use serde_json::{json, Map, Value};

#[cfg(test)]
use crate::harness::agents::sub_agent_import_file;
use crate::harness::agents::{
    merge_harness_override, normalize_tool_permissions_with_allow_list,
    parse_native_markdown_agent, render_native_markdown, select_harness_value,
    validate_merged_markdown_agent, yaml_key, NativeAgentParseOptions, NativeAgentRole, SubAgent,
};
use crate::harness::artifact::{
    CommandMode, CommandsDirectory, HarnessArtifact, InstructionFile, JsonConfigFile, McpCodec,
    McpConfig, NativeConfig, PreferenceBinding, PreferenceCodec, PreferenceKind,
    SettingsPreferences, SkillsDirectory, SubagentCodec, SubagentsDirectory,
};
#[cfg(test)]
use crate::harness::integration::ImportedFile;
use crate::harness::integration::{
    AppEnvironment, HarnessConfigPaths, HarnessIntegration, ImportedPreference, ProfileRef,
};
use crate::harness::kind::HarnessKind;
use crate::profile::mcp::{McpDefinition, McpTransport, McpValue};

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
                CommandMode::RecursiveCopy,
            )),
            Box::new(SubagentsDirectory::new(
                |paths| &paths.agents_dir,
                OpenCodeSubagentCodec,
            )),
            Box::new(McpConfig::new(
                JsonConfigFile::new(|paths| &paths.settings_file).label("OpenCode settings JSON"),
                OpenCodeMcpCodec,
            )),
            Box::new(
                SettingsPreferences::new(
                    JsonConfigFile::new(|paths| &paths.settings_file)
                        .label("OpenCode settings JSON"),
                )
                .model(PreferenceBinding::JsonStringPointer { pointer: "/model" })
                .permission(PreferenceBinding::Custom(Box::new(OpenCodePermissionCodec))),
            ),
        ]
    }
}

struct OpenCodePermissionCodec;

impl PreferenceCodec for OpenCodePermissionCodec {
    fn import(&self, config: &NativeConfig) -> Result<ImportedPreference> {
        let permission = config
            .json_object("OpenCode JSON config")?
            .get("permission")
            .cloned()
            .unwrap_or_else(|| json!("default"));
        validate_opencode_permission(&permission)?;
        Ok(ImportedPreference::new(permission))
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
        validate_opencode_permission(&permission)?;
        config
            .json_object_mut("OpenCode JSON config")?
            .insert("permission".to_string(), permission);
        Ok(())
    }
}

fn validate_opencode_permission(permission: &Value) -> Result<()> {
    if permission == "default" || permission.is_object() {
        Ok(())
    } else {
        anyhow::bail!("OpenCode permission preference must be an object or \"default\"")
    }
}

struct OpenCodeSubagentCodec;

impl SubagentCodec for OpenCodeSubagentCodec {
    fn native_file_name(&self, agent: &SubAgent) -> String {
        format!("{}.md", agent.name)
    }

    fn render(&self, agent: &SubAgent) -> Result<String> {
        render_opencode_agent(agent)
    }

    fn should_import(&self, path: &Path) -> bool {
        path.extension().is_some_and(|ext| ext == "md")
    }

    fn parse(&self, path: &Path, contents: &str) -> Result<SubAgent> {
        let fallback_name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid agent path {}", path.display()))?;
        parse_native_markdown_agent(
            contents,
            fallback_name,
            "opencode",
            NativeAgentParseOptions {
                require_name: false,
                permission_key: Some("permission"),
                max_turns_key: Some("steps"),
                role: Some(NativeAgentRole {
                    key: "mode",
                    accepted: "subagent",
                    required: true,
                }),
            },
            normalize_tool_permissions_with_allow_list,
        )
    }
}

struct OpenCodeMcpCodec;

impl McpCodec for OpenCodeMcpCodec {
    fn import(&self, config: &NativeConfig) -> Result<Vec<McpDefinition>> {
        let document = config.json_object("OpenCode JSON config")?;
        let definitions = import_opencode_mcps(document)?;
        reject_opencode_mcp_tool_rules(
            document,
            definitions
                .iter()
                .map(|definition| definition.name.as_str()),
        )?;
        Ok(definitions)
    }

    fn apply(&self, config: &mut NativeConfig, definitions: &[McpDefinition]) -> Result<()> {
        let document = config.json_object_mut("OpenCode JSON config")?;
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

    fn preflight_apply(&self, config: &NativeConfig, definitions: &[McpDefinition]) -> Result<()> {
        crate::profile::mcp::reject_native_reference_literals(definitions, "OpenCode", |value| {
            contains_opencode_substitution(value)
        })?;
        reject_opencode_direct_substitutions(definitions)?;
        let document = config.json_object("OpenCode JSON config")?;
        let native = import_opencode_mcps(document)?;
        let names = native
            .iter()
            .chain(definitions)
            .map(|definition| definition.name.as_str());
        reject_opencode_mcp_tool_rules(document, names)?;
        Ok(())
    }
}

fn render_opencode_agent(agent: &SubAgent) -> Result<String> {
    let mut map = crate::yaml::Mapping::new();
    map.insert(yaml_key("description"), agent.description.clone().into());
    map.insert(yaml_key("mode"), "subagent".into());
    if let Some(model) = select_harness_value(agent.model.as_ref(), "opencode") {
        map.insert(yaml_key("model"), model.clone());
    }
    if let Some(tools) = agent.tools.as_ref() {
        map.insert(yaml_key("permission"), tools.clone());
    }
    if let Some(permission) = select_harness_value(agent.permission.as_ref(), "opencode") {
        map.insert(yaml_key("permission"), permission.clone());
    }
    if let Some(max_turns) = agent.max_turns {
        map.insert(
            yaml_key("steps"),
            crate::yaml::Value::Number(max_turns.into()),
        );
    }
    merge_harness_override(&mut map, agent, "opencode")?;
    if map
        .get(yaml_key("mode"))
        .and_then(crate::yaml::Value::as_str)
        != Some("subagent")
    {
        anyhow::bail!("OpenCode agent {} must have mode subagent", agent.name);
    }
    validate_merged_markdown_agent(&map, "OpenCode", false, "steps")?;
    render_native_markdown(map, &agent.body)
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
        let neutral = parse_native_markdown_agent(
            &text,
            fallback_name,
            "opencode",
            NativeAgentParseOptions {
                require_name: false,
                permission_key: Some("permission"),
                max_turns_key: Some("steps"),
                role: Some(NativeAgentRole {
                    key: "mode",
                    accepted: "subagent",
                    required: true,
                }),
            },
            normalize_tool_permissions_with_allow_list,
        )?;
        imported.push(sub_agent_import_file(&neutral));
    }
    imported.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(Some(imported))
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
        let enabled = match table.get("enabled") {
            Some(Value::Bool(value)) => *value,
            Some(_) => anyhow::bail!("OpenCode MCP server {name} enabled must be a boolean"),
            None => true,
        };

        let mcp_type = match table.get("type") {
            Some(Value::String(value)) => value.as_str(),
            Some(_) => anyhow::bail!("OpenCode MCP server {name} type must be a string"),
            None => "local",
        };
        let allowed: &[&str] = if mcp_type == "local" {
            &["type", "command", "environment", "enabled"]
        } else {
            &["type", "url", "headers", "enabled"]
        };
        if let Some(field) = table
            .keys()
            .find(|field| !allowed.contains(&field.as_str()))
        {
            anyhow::bail!(
                "OpenCode MCP server {name} uses unsupported field {field}; import or replacement would lose native security settings"
            );
        }

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
            if command_array
                .iter()
                .any(|value| contains_opencode_substitution(value))
            {
                anyhow::bail!("OpenCode MCP {name} command contains native substitution syntax");
            }

            let env = if let Some(environment) = table.get("environment") {
                let Some(env_obj) = environment.as_object() else {
                    anyhow::bail!("OpenCode MCP environment must be an object");
                };
                let mut map = BTreeMap::new();
                for (k, v) in env_obj {
                    let Some(s) = v.as_str() else {
                        anyhow::bail!("OpenCode MCP environment values must be strings");
                    };
                    map.insert(k.clone(), parse_opencode_env(s)?);
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
            if contains_opencode_substitution(url) {
                anyhow::bail!("OpenCode MCP {name} URL contains native substitution syntax");
            }

            let headers = if let Some(headers) = table.get("headers") {
                let Some(headers_obj) = headers.as_object() else {
                    anyhow::bail!("OpenCode MCP headers must be an object");
                };
                let mut map = BTreeMap::new();
                for (k, v) in headers_obj {
                    if let Some(s) = v.as_str() {
                        map.insert(k.clone(), parse_opencode_env(s)?);
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

fn reject_opencode_mcp_tool_rules<'a>(
    document: &Map<String, Value>,
    names: impl Iterator<Item = &'a str>,
) -> Result<()> {
    let names = names.collect::<Vec<_>>();
    let mut rule_sets = Vec::new();
    for key in ["tools", "permission"] {
        if let Some(value) = document.get(key) {
            rule_sets.push((key.to_string(), value));
        }
    }
    if let Some(agents) = document.get("agent") {
        let agents = agents
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("OpenCode agent setting must be an object"))?;
        for (agent, value) in agents {
            let agent = value
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("OpenCode agent {agent} must be an object"))?;
            for key in ["tools", "permission"] {
                if let Some(value) = agent.get(key) {
                    rule_sets.push((format!("agent.{key}"), value));
                }
            }
        }
    }
    for (location, rules) in rule_sets {
        let rules = rules
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("OpenCode {location} rules must be an object"))?;
        for pattern in rules.keys() {
            let affects_managed = names
                .iter()
                .any(|name| glob_can_match_prefix(pattern, &format!("{name}_")));
            if affects_managed {
                anyhow::bail!(
                    "OpenCode MCP tool rule {pattern} in {location} cannot be represented safely"
                );
            }
        }
    }
    Ok(())
}

fn glob_can_match_prefix(pattern: &str, prefix: &str) -> bool {
    let pattern = pattern.as_bytes();
    let mut states = std::collections::BTreeSet::from([0usize]);
    add_glob_epsilon_states(pattern, &mut states);
    for byte in prefix.bytes() {
        let mut next = std::collections::BTreeSet::new();
        for state in &states {
            match pattern.get(*state) {
                Some(b'*') => {
                    next.insert(*state);
                }
                Some(b'?') => {
                    next.insert(state + 1);
                }
                Some(expected) if *expected == byte => {
                    next.insert(state + 1);
                }
                _ => {}
            }
        }
        add_glob_epsilon_states(pattern, &mut next);
        states = next;
        if states.is_empty() {
            return false;
        }
    }
    !states.is_empty()
}

fn add_glob_epsilon_states(pattern: &[u8], states: &mut std::collections::BTreeSet<usize>) {
    let mut pending = states.iter().copied().collect::<Vec<_>>();
    while let Some(state) = pending.pop() {
        if pattern.get(state) == Some(&b'*') && states.insert(state + 1) {
            pending.push(state + 1);
        }
    }
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
                    map.insert(
                        "environment".to_string(),
                        json!(render_opencode_env(&stdio.env)),
                    );
                }
            }
            McpTransport::Http(http) => {
                map.insert("type".to_string(), json!("remote"));
                map.insert("url".to_string(), json!(http.url));
                if !http.headers.is_empty() {
                    map.insert(
                        "headers".to_string(),
                        json!(render_opencode_env(&http.headers)),
                    );
                }
            }
        }
        Ok(Value::Object(map))
    }
}

fn parse_opencode_env(value: &str) -> Result<McpValue> {
    if let Some(name) = value
        .strip_prefix("{env:")
        .and_then(|value| value.strip_suffix('}'))
    {
        McpValue::env(name)
    } else if contains_opencode_substitution(value) {
        anyhow::bail!("OpenCode MCP value contains unrepresentable native substitution syntax")
    } else {
        Ok(McpValue::literal(value))
    }
}

fn contains_opencode_substitution(value: &str) -> bool {
    value.contains("{env:") || value.contains("{file:")
}

fn reject_opencode_direct_substitutions(definitions: &[McpDefinition]) -> Result<()> {
    for definition in definitions {
        let values: Vec<&str> = match &definition.transport {
            McpTransport::Stdio(stdio) => std::iter::once(stdio.command.as_str())
                .chain(stdio.args.iter().map(String::as_str))
                .collect(),
            McpTransport::Http(http) => vec![http.url.as_str()],
        };
        if values.into_iter().any(contains_opencode_substitution) {
            anyhow::bail!(
                "OpenCode MCP server {} contains a literal that matches native substitution syntax",
                definition.name
            );
        }
    }
    Ok(())
}

fn render_opencode_env(values: &BTreeMap<String, McpValue>) -> BTreeMap<String, String> {
    values
        .iter()
        .map(|(key, value)| {
            let value = match value {
                McpValue::Literal(value) => value.clone(),
                McpValue::Env(name) => format!("{{env:{name}}}"),
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
  "permission": {"edit": "ask"},
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
                r#"{"model": "drift-model", "permission": {"edit": "deny"}}"#,
            )
            .unwrap();
        }
        fn assert_drift_saved(&self, config: &ProfileConfig) {
            assert_eq!(config.model_preference("opencode"), "drift-model");
            assert_eq!(
                config.permission_preference("opencode"),
                serde_json::json!({"edit": "deny"})
            );
        }
        fn write_profile_config(&self, profile: &Path) {
            crate::integrations::test_suite::template::write_config(
                profile,
                r#"{
  "name": "work",
  "description": "",
  "models": {"opencode": "gpt-5.2"},
  "permissions": {"opencode": {"edit": "ask"}}
}"#,
            );
        }
        fn assert_applied_native_config(&self, paths: &HarnessConfigPaths) {
            let config = fs::read_to_string(&paths.settings_file).unwrap();
            assert!(config.contains("gpt-5.2"));
            assert!(config.contains(r#""permission""#));
            assert!(config.contains("ask"));
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
                .and_then(crate::yaml::Value::as_str),
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
                .and_then(crate::yaml::Value::as_str),
            Some("allow")
        );
        assert_eq!(
            agent
                .tools
                .as_ref()
                .and_then(|tools| tools.get("shell"))
                .and_then(crate::yaml::Value::as_str),
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

    #[test]
    fn renders_current_opencode_agent_fields() {
        let agent = SubAgent {
            name: "reviewer".to_string(),
            description: "Reviews code".to_string(),
            model: None,
            tools: Some(crate::yaml::from_str("edit: deny\nbash: ask").unwrap()),
            permission: None,
            max_turns: Some(5),
            harness: BTreeMap::new(),
            body: "Review carefully.".to_string(),
        };

        let rendered = render_opencode_agent(&agent).unwrap();

        assert!(!rendered.contains("name:"));
        assert!(!rendered.contains("tools:"));
        assert!(rendered.contains("permission:"));
        assert!(rendered.contains("steps: 5"));
    }

    #[test]
    fn opencode_rejects_blank_description_override_after_merge() {
        let mut harness = BTreeMap::new();
        harness.insert(
            "opencode".to_string(),
            crate::yaml::from_str("description: ''").unwrap(),
        );
        let agent = SubAgent {
            name: "reviewer".to_string(),
            description: "Reviews".to_string(),
            model: None,
            tools: None,
            permission: None,
            max_turns: None,
            harness,
            body: String::new(),
        };
        assert!(render_opencode_agent(&agent).is_err());
    }

    #[test]
    fn opencode_mcp_environment_references_round_trip() {
        let definitions = crate::profile::mcp::parse_mcp_definitions(
            r#"[{"name":"x","transport":"stdio","command":"x","env":{"TOKEN":{"env":"TOKEN"}}}]"#,
        )
        .unwrap();
        let mut config = NativeConfig::Json(json!({}));
        OpenCodeMcpCodec.apply(&mut config, &definitions).unwrap();
        assert_eq!(OpenCodeMcpCodec.import(&config).unwrap(), definitions);
    }

    #[test]
    fn opencode_rejects_malformed_mcp_and_tool_security_rules() {
        let malformed = NativeConfig::Json(json!({"mcp":{"x":{"type":false}}}));
        assert!(OpenCodeMcpCodec.import(&malformed).is_err());
        let restricted = NativeConfig::Json(json!({
            "mcp":{"server":{"type":"local","command":["x"]}},
            "tools":{"server_*":false}
        }));
        assert!(OpenCodeMcpCodec.import(&restricted).is_err());
        let definitions = crate::profile::mcp::parse_mcp_definitions(
            r#"[{"name":"x","transport":"stdio","command":"x","env":{"TOKEN":"{env:TOKEN}"}}]"#,
        )
        .unwrap();
        assert!(OpenCodeMcpCodec
            .preflight_apply(&NativeConfig::Json(json!({})), &definitions)
            .is_err());
        for document in [
            json!({"mcp":{"server":{"type":"local","command":["x"]}},"permission":{"server_*":"deny"}}),
            json!({"mcp":{"server":{"type":"local","command":["x"]}},"tools":{"server_?":"deny"}}),
            json!({"mcp":{"server":{"type":"local","command":["x"]}},"agent":{"reviewer":{"permission":{"server_tool":"deny"}}}}),
        ] {
            assert!(OpenCodeMcpCodec
                .import(&NativeConfig::Json(document))
                .is_err());
        }
        for document in [
            json!({"mcp":{"server":{"type":"local","command":["x"]}},"permission":{"bash*":"ask"}}),
            json!({"mcp":{"server":{"type":"local","command":["x"]}},"agent":{"reviewer":{"tools":{"other_*":false}}}}),
        ] {
            assert!(OpenCodeMcpCodec
                .import(&NativeConfig::Json(document))
                .is_ok());
        }
        for value in ["Bearer {env:TOKEN}", "{file:/tmp/token}"] {
            let native =
                json!({"mcp":{"x":{"type":"local","command":["x"],"environment":{"TOKEN":value}}}});
            assert!(OpenCodeMcpCodec
                .import(&NativeConfig::Json(native))
                .is_err());
        }
    }

    #[test]
    fn opencode_custom_permission_verifies_effective_value() {
        let config = NativeConfig::Json(json!({"permission":{"edit":"deny"}}));
        assert!(OpenCodePermissionCodec
            .verify(&config, json!({"edit":"allow"}))
            .is_err());
    }
}
