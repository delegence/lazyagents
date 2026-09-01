use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use toml_edit::{value, DocumentMut, Item};

use crate::file_system::write_text_atomic;
use crate::harness::agents::{
    merge_harness_override, normalize_tool_permissions_with_allow_list,
    parse_native_markdown_agent, render_native_markdown, select_harness_value, tools_to_allow_list,
    validate_lowercase_agent_slug, validate_merged_markdown_agent, yaml_key,
    NativeAgentParseOptions, NativeAgentRole, SubAgent,
};
use crate::harness::artifact::{
    CommandCodec, CommandMode, CommandsDirectory, HarnessArtifact, InstructionFile, JsonConfigFile,
    McpCodec, McpConfig, NativeConfig, PreferenceBinding, SettingsPreferences, SkillsDirectory,
    SubagentCodec, SubagentsDirectory,
};
use crate::harness::commands::profile_commands_recursive;
use crate::harness::drift::DriftItem;
use crate::harness::fs::import_files_recursive_filtered;
use crate::harness::integration::{
    AppEnvironment, HarnessConfigPaths, HarnessIntegration, ImportedFile, ProfileRef,
};
use crate::harness::kind::HarnessKind;
use crate::profile::mcp::{McpDefinition, McpTransport, McpValue};

pub struct GeminiIntegration;

impl HarnessIntegration for GeminiIntegration {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Gemini
    }

    fn default_config_dir(&self, env: &AppEnvironment) -> std::path::PathBuf {
        env.user_home.join(".gemini")
    }

    fn paths_from_config_dir(&self, config_dir: std::path::PathBuf) -> Result<HarnessConfigPaths> {
        Ok(HarnessConfigPaths {
            instruction_target: config_dir.join("GEMINI.md"),
            skills_dir: config_dir.join("skills"),
            commands_dir: config_dir.join("commands"),
            agents_dir: config_dir.join("agents"),
            settings_file: config_dir.join("settings.json"),
            mcp_file: config_dir.join("settings.json"),
            config_dir,
        })
    }

    fn artifacts(&self) -> Vec<Box<dyn HarnessArtifact>> {
        vec![
            Box::new(InstructionFile::new(|paths| &paths.instruction_target)),
            Box::new(SkillsDirectory::new(|paths| &paths.skills_dir)),
            Box::new(CommandsDirectory::new(
                |paths| &paths.commands_dir,
                CommandMode::Rendered(Box::new(GeminiCommandCodec)),
            )),
            Box::new(SubagentsDirectory::new(
                |paths| &paths.agents_dir,
                GeminiSubagentCodec,
            )),
            Box::new(McpConfig::new(
                JsonConfigFile::new(|paths| &paths.settings_file).label("Gemini settings JSON"),
                GeminiMcpCodec,
            )),
            Box::new(
                SettingsPreferences::new(
                    JsonConfigFile::new(|paths| &paths.settings_file).label("Gemini settings JSON"),
                )
                .model(PreferenceBinding::JsonStringPointer {
                    pointer: "/model/name",
                })
                .permission(PreferenceBinding::JsonStringPointer {
                    pointer: "/general/defaultApprovalMode",
                }),
            ),
        ]
    }
}

struct GeminiCommandCodec;

impl CommandCodec for GeminiCommandCodec {
    fn import(&self, path: &Path) -> Result<Vec<ImportedFile>> {
        import_gemini_commands(path)
    }

    fn apply(&self, profile: &ProfileRef, target_dir: &Path) -> Result<()> {
        write_gemini_commands_to(profile, target_dir)
    }

    fn detect_drift(&self, profile: &ProfileRef, target_dir: &Path) -> Result<Vec<DriftItem>> {
        let mut items = Vec::new();
        collect_gemini_command_drift_in(profile, target_dir, &mut items)?;
        Ok(items)
    }

    fn verify(&self, profile: &ProfileRef, target_dir: &Path, _display_name: &str) -> Result<()> {
        verify_gemini_commands_in(profile, target_dir)
    }
}

struct GeminiSubagentCodec;

impl SubagentCodec for GeminiSubagentCodec {
    fn native_file_name(&self, agent: &SubAgent) -> String {
        format!("{}.md", agent.name)
    }

    fn render(&self, agent: &SubAgent) -> Result<String> {
        render_gemini_agent(agent)
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
            "gemini",
            NativeAgentParseOptions {
                require_name: false,
                permission_key: None,
                max_turns_key: Some("max_turns"),
                role: Some(NativeAgentRole {
                    key: "kind",
                    accepted: "local",
                    required: false,
                }),
            },
            normalize_tool_permissions_with_allow_list,
        )?;
        validate_lowercase_agent_slug(&agent.name, "Gemini", b"0123456789_")?;
        Ok(agent)
    }
}

struct GeminiMcpCodec;

impl McpCodec for GeminiMcpCodec {
    fn import(&self, config: &NativeConfig) -> Result<Vec<McpDefinition>> {
        import_gemini_mcps(config.json_object("Gemini JSON config")?)
    }

    fn apply(&self, config: &mut NativeConfig, definitions: &[McpDefinition]) -> Result<()> {
        patch_gemini_mcps(config.json_object_mut("Gemini JSON config")?, definitions)
    }

    fn preflight_apply(&self, config: &NativeConfig, definitions: &[McpDefinition]) -> Result<()> {
        crate::profile::mcp::reject_native_reference_literals(definitions, "Gemini", |value| {
            contains_gemini_substitution(value)
        })?;
        reject_gemini_direct_substitutions(definitions)?;
        import_gemini_mcps(config.json_object("Gemini JSON config")?)?;
        Ok(())
    }
}

fn collect_gemini_command_drift_in(
    active: &ProfileRef,
    commands_dir: &Path,
    items: &mut Vec<DriftItem>,
) -> Result<()> {
    let expected = profile_commands_recursive(&active.path)?
        .into_iter()
        .map(|command| {
            let relative = command
                .strip_prefix(active.path.join("commands"))
                .unwrap()
                .to_path_buf();
            let contents = fs::read(&command)
                .with_context(|| format!("failed to read {}", command.display()))?;
            Ok((relative, contents))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;

    let actual = import_gemini_commands(commands_dir)?
        .into_iter()
        .map(|file| (file.relative_path, file.contents))
        .collect::<BTreeMap<_, _>>();

    for (relative, contents) in &expected {
        if actual.get(relative) != Some(contents) {
            items.push(DriftItem {
                surface: "commands".to_string(),
                detail: format!(
                    "{} does not match active profile",
                    commands_dir
                        .join(markdown_command_to_toml(relative))
                        .display()
                ),
            });
        }
    }
    for relative in actual.keys() {
        if !expected.contains_key(relative) {
            items.push(DriftItem {
                surface: "commands".to_string(),
                detail: format!(
                    "unexpected harness entry {}",
                    commands_dir
                        .join(markdown_command_to_toml(relative))
                        .display()
                ),
            });
        }
    }
    Ok(())
}

fn write_gemini_commands_to(profile: &ProfileRef, commands_dir: &Path) -> Result<()> {
    for command in profile_commands_recursive(&profile.path)? {
        let relative = command.strip_prefix(profile.path.join("commands")).unwrap();
        let target = commands_dir.join(markdown_command_to_toml(relative));
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let prompt = fs::read_to_string(&command)
            .with_context(|| format!("failed to read {}", command.display()))?;
        let mut document = DocumentMut::new();
        document["prompt"] = value(prompt);
        write_text_atomic(&target, &document.to_string())
            .with_context(|| format!("failed to write {}", target.display()))?;
    }
    Ok(())
}

fn verify_gemini_commands_in(profile: &ProfileRef, commands_dir: &Path) -> Result<()> {
    for command in profile_commands_recursive(&profile.path)? {
        let relative = command.strip_prefix(profile.path.join("commands")).unwrap();
        let target = commands_dir.join(markdown_command_to_toml(relative));
        let actual = read_gemini_command_prompt(&target)?;
        let expected = fs::read_to_string(&command)
            .with_context(|| format!("failed to read {}", command.display()))?;
        if actual != expected {
            anyhow::bail!("Gemini command {} was not applied", target.display());
        }
    }
    Ok(())
}

fn import_gemini_commands(path: &Path) -> Result<Vec<ImportedFile>> {
    let mut commands = Vec::new();
    if !path.exists() {
        return Ok(commands);
    }
    for file in import_files_recursive_filtered(path, path, &|relative| {
        relative
            .extension()
            .is_some_and(|extension| extension == "toml")
    })? {
        let command_path = path.join(&file.relative_path);
        commands.push(ImportedFile {
            relative_path: toml_command_to_markdown(&file.relative_path),
            contents: read_gemini_command_prompt(&command_path)?.into_bytes(),
            unix_mode: None,
        });
    }
    commands.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(commands)
}

fn read_gemini_command_prompt(path: &Path) -> Result<String> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let document = text
        .parse::<DocumentMut>()
        .with_context(|| format!("invalid Gemini command TOML at {}", path.display()))?;
    document
        .get("prompt")
        .and_then(Item::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("Gemini command {} requires prompt", path.display()))
}

fn markdown_command_to_toml(relative: &Path) -> PathBuf {
    let mut path = relative.to_path_buf();
    path.set_extension("toml");
    path
}

fn toml_command_to_markdown(relative: &Path) -> PathBuf {
    let mut path = relative.to_path_buf();
    path.set_extension("md");
    path
}

fn render_gemini_agent(agent: &SubAgent) -> Result<String> {
    validate_lowercase_agent_slug(&agent.name, "Gemini", b"0123456789_")?;
    let mut map = crate::yaml::Mapping::new();
    map.insert(yaml_key("name"), agent.name.clone().into());
    map.insert(yaml_key("description"), agent.description.clone().into());
    map.insert(yaml_key("kind"), "local".into());
    if let Some(model) = select_harness_value(agent.model.as_ref(), "gemini") {
        map.insert(yaml_key("model"), model.clone());
    }
    if let Some(tools) = agent.tools.as_ref() {
        map.insert(yaml_key("tools"), tools_to_allow_list(tools));
    }
    if let Some(max_turns) = agent.max_turns {
        map.insert(
            yaml_key("max_turns"),
            crate::yaml::Value::Number(max_turns.into()),
        );
    }
    merge_harness_override(&mut map, agent, "gemini")?;
    if map
        .get(yaml_key("kind"))
        .and_then(crate::yaml::Value::as_str)
        != Some("local")
    {
        anyhow::bail!("Gemini agent {} must have kind local", agent.name);
    }
    validate_merged_markdown_agent(&map, "Gemini", true, "max_turns")?;
    render_native_markdown(map, &agent.body)
}

fn set_nested_value(document: &mut Map<String, Value>, path: &[&str], value: Value) -> Result<()> {
    let mut current = document;
    for key in &path[..path.len() - 1] {
        let entry = current
            .entry((*key).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        current = entry
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("failed to create Gemini settings object {key}"))?;
    }
    current.insert(path[path.len() - 1].to_string(), value);
    Ok(())
}

fn import_gemini_mcps(document: &Map<String, Value>) -> Result<Vec<McpDefinition>> {
    let mut servers = Vec::new();
    let allowed = gemini_allowed_mcps(document)?;
    let excluded = gemini_excluded_mcps(document)?;
    let Some(mcp_servers) = document.get("mcpServers") else {
        return Ok(Vec::new());
    };
    let Some(mcp_table) = mcp_servers.as_object() else {
        anyhow::bail!("Gemini mcpServers must be an object");
    };

    for (name, item) in mcp_table {
        let Some(table) = item.as_object() else {
            anyhow::bail!("Gemini MCP server {name} must be an object");
        };
        let enabled = allowed
            .as_ref()
            .is_none_or(|allowed| allowed.contains(name))
            && !excluded.contains(name);
        let allowed_fields = ["command", "args", "env", "httpUrl", "url", "headers"];
        if let Some(field) = table
            .keys()
            .find(|field| !allowed_fields.contains(&field.as_str()))
        {
            anyhow::bail!(
                "Gemini MCP server {name} uses unsupported field {field}; import or replacement would lose native security settings"
            );
        }
        let command = table.get("command");
        let http_url = table.get("httpUrl");
        let sse_url = table.get("url");
        if sse_url.is_some() {
            anyhow::bail!(
                "Gemini MCP server {name} uses SSE field url, which the neutral MCP schema cannot represent"
            );
        }
        if command.is_some() && http_url.is_some() {
            anyhow::bail!("Gemini MCP server {name} defines more than one transport");
        }
        if let Some(command) = command {
            let command = command
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Gemini MCP {name} command must be a string"))?;
            if contains_gemini_substitution(command) {
                anyhow::bail!("Gemini MCP {name} command contains native substitution syntax");
            }
            let args = string_array(table.get("args"), "Gemini MCP args")?;
            if args.iter().any(|value| contains_gemini_substitution(value)) {
                anyhow::bail!("Gemini MCP {name} args contain native substitution syntax");
            }
            servers.push(json!({
                "name": name,
                "enabled": enabled,
                "transport": "stdio",
                "command": command,
                "args": args,
                "env": string_object(table.get("env"), "Gemini MCP env")?,
            }));
        } else if let Some(url) = http_url {
            let url = url
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Gemini MCP {name} httpUrl must be a string"))?;
            if contains_gemini_substitution(url) {
                anyhow::bail!("Gemini MCP {name} httpUrl contains native substitution syntax");
            }
            servers.push(json!({
                "name": name,
                "enabled": enabled,
                "transport": "http",
                "url": url,
                "headers": string_object(table.get("headers"), "Gemini MCP headers")?,
            }));
        } else {
            anyhow::bail!("Gemini MCP server {name} must define command, httpUrl, or url");
        }
    }

    crate::profile::mcp::parse_mcp_definitions(&serde_json::to_string(&servers)?)
}

fn patch_gemini_mcps(
    document: &mut Map<String, Value>,
    definitions: &[McpDefinition],
) -> Result<()> {
    let previous_names = document
        .get("mcpServers")
        .and_then(Value::as_object)
        .map(|servers| servers.keys().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    document.remove("mcpServers");
    if !definitions.is_empty() {
        let mut servers = Map::new();
        for definition in definitions {
            servers.insert(definition.name.clone(), definition.to_gemini_value()?);
        }
        document.insert("mcpServers".to_string(), Value::Object(servers));
    }

    let mut managed_names = definitions
        .iter()
        .map(|definition| definition.name.clone())
        .collect::<BTreeSet<_>>();
    managed_names.extend(previous_names);

    if let Some(previous_allowed) = gemini_allowed_mcps(document)? {
        let mut allowed = previous_allowed
            .into_iter()
            .filter(|name| !managed_names.contains(name))
            .collect::<Vec<_>>();
        allowed.extend(
            definitions
                .iter()
                .filter(|definition| definition.enabled)
                .map(|definition| definition.name.clone()),
        );
        allowed.sort();
        allowed.dedup();
        set_nested_value(document, &["mcp", "allowed"], json!(allowed))?;
    }
    let mut excluded = gemini_excluded_mcps(document)?
        .into_iter()
        .filter(|name| !managed_names.contains(name))
        .collect::<Vec<_>>();
    for definition in definitions {
        if !definition.enabled {
            excluded.push(definition.name.clone());
        }
    }
    excluded.sort();
    excluded.dedup();

    if !excluded.is_empty() {
        set_nested_value(document, &["mcp", "excluded"], json!(excluded))?;
    } else if let Some(mcp) = document.get_mut("mcp").and_then(Value::as_object_mut) {
        mcp.remove("excluded");
        if mcp.is_empty() {
            document.remove("mcp");
        }
    }
    Ok(())
}

fn gemini_allowed_mcps(document: &Map<String, Value>) -> Result<Option<BTreeSet<String>>> {
    let mcp = gemini_mcp_settings(document)?;
    let Some(allowed) = mcp.and_then(|mcp| mcp.get("allowed")) else {
        return Ok(None);
    };
    Ok(Some(
        string_array(Some(allowed), "Gemini mcp.allowed")?
            .into_iter()
            .collect(),
    ))
}

fn gemini_excluded_mcps(document: &Map<String, Value>) -> Result<BTreeSet<String>> {
    let mcp = gemini_mcp_settings(document)?;
    let Some(excluded) = mcp.and_then(|mcp| mcp.get("excluded")) else {
        return Ok(BTreeSet::new());
    };
    Ok(string_array(Some(excluded), "Gemini mcp.excluded")?
        .into_iter()
        .collect())
}

fn gemini_mcp_settings(document: &Map<String, Value>) -> Result<Option<&Map<String, Value>>> {
    match document.get("mcp") {
        Some(Value::Object(value)) => Ok(Some(value)),
        Some(_) => anyhow::bail!("Gemini mcp setting must be an object"),
        None => Ok(None),
    }
}

fn string_array(value: Option<&Value>, label: &str) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(array) = value.as_array() else {
        anyhow::bail!("{label} must be an array");
    };
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("{label} values must be strings"))
        })
        .collect()
}

fn string_object(value: Option<&Value>, label: &str) -> Result<BTreeMap<String, McpValue>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let Some(object) = value.as_object() else {
        anyhow::bail!("{label} must be an object");
    };
    object
        .iter()
        .map(|(key, value)| {
            let value = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("{label} values must be strings"))?;
            let parsed = parse_gemini_env(value)?;
            Ok((key.clone(), parsed))
        })
        .collect()
}

fn parse_gemini_env(value: &str) -> Result<McpValue> {
    let exact = value
        .strip_prefix("${")
        .and_then(|value| value.strip_suffix('}'))
        .filter(|name| !name.contains(":-"))
        .or_else(|| value.strip_prefix('$'));
    if let Some(name) = exact {
        return McpValue::env(name);
    }
    #[cfg(windows)]
    if let Some(name) = value
        .strip_prefix('%')
        .and_then(|value| value.strip_suffix('%'))
    {
        return McpValue::env(name);
    }
    if contains_gemini_substitution(value) {
        anyhow::bail!("Gemini MCP value contains unrepresentable native substitution syntax");
    }
    Ok(McpValue::literal(value))
}

fn contains_gemini_substitution(value: &str) -> bool {
    let bytes = value.as_bytes();
    for index in 0..bytes.len() {
        if bytes[index] == b'$'
            && bytes
                .get(index + 1)
                .is_some_and(|next| *next == b'{' || next.is_ascii_alphanumeric() || *next == b'_')
        {
            return true;
        }
        #[cfg(windows)]
        if bytes[index] == b'%' {
            if let Some(end) = value[index + 1..].find('%') {
                let name = &value[index + 1..index + 1 + end];
                if !name.is_empty()
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                {
                    return true;
                }
            }
        }
    }
    false
}

fn reject_gemini_direct_substitutions(definitions: &[McpDefinition]) -> Result<()> {
    for definition in definitions {
        let values: Vec<&str> = match &definition.transport {
            McpTransport::Stdio(stdio) => std::iter::once(stdio.command.as_str())
                .chain(stdio.args.iter().map(String::as_str))
                .collect(),
            McpTransport::Http(http) => vec![http.url.as_str()],
        };
        if values.into_iter().any(contains_gemini_substitution) {
            anyhow::bail!(
                "Gemini MCP server {} contains a literal that matches native substitution syntax",
                definition.name
            );
        }
    }
    Ok(())
}

impl McpDefinition {
    fn to_gemini_value(&self) -> Result<Value> {
        let mut map = Map::new();
        match &self.transport {
            McpTransport::Stdio(stdio) => {
                map.insert("command".to_string(), json!(stdio.command));
                if !stdio.args.is_empty() {
                    map.insert("args".to_string(), json!(stdio.args));
                }
                if !stdio.env.is_empty() {
                    map.insert("env".to_string(), json!(render_gemini_env(&stdio.env)));
                }
            }
            McpTransport::Http(http) => {
                map.insert("httpUrl".to_string(), json!(http.url));
                if !http.headers.is_empty() {
                    map.insert(
                        "headers".to_string(),
                        json!(render_gemini_env(&http.headers)),
                    );
                }
            }
        }
        Ok(Value::Object(map))
    }
}

fn render_gemini_env(values: &BTreeMap<String, McpValue>) -> BTreeMap<String, String> {
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
    struct GeminiAdapter;

    impl HarnessTestAdapter for GeminiAdapter {
        fn integration(&self) -> Box<dyn HarnessIntegration> {
            Box::new(GeminiIntegration)
        }
        fn bin_name(&self) -> &'static str {
            "gemini"
        }
        fn assert_mcp_cleared(&self, paths: &HarnessConfigPaths) {
            let config = fs::read_to_string(&paths.mcp_file).unwrap_or_else(|_| "{}".to_string());
            assert!(!config.contains("mcpServers"));
        }
        fn write_malformed_native_config(&self, paths: &HarnessConfigPaths) {
            fs::write(&paths.settings_file, "{ malformed }").unwrap();
        }
        fn supports_nested_commands(&self) -> bool {
            true
        }
        fn write_existing_native_settings(&self, paths: &HarnessConfigPaths) {
            fs::write(
                &paths.settings_file,
                r#"{"ui":{"theme":"Default"},"model":{"name":"old"}}"#,
            )
            .unwrap();
        }
        fn assert_native_settings_preserved(&self, paths: &HarnessConfigPaths) {
            let config = fs::read_to_string(&paths.settings_file).unwrap();
            assert!(config.contains("Default"));
            assert!(config.contains("old"));
        }
        fn setup_native_config_for_import(&self, paths: &HarnessConfigPaths) {
            fs::write(
                &paths.settings_file,
                r#"{
  "model": {"name": "gemini-imported"},
  "general": {"defaultApprovalMode": "auto_edit"},
  "mcp": {"excluded": ["disabled"]},
  "mcpServers": {
    "local": {"command":"server","args":["--flag"],"env":{"TOKEN":"$TOKEN"}},
    "remote": {"httpUrl":"https://mcp.example","headers":{"Authorization":"$TOKEN"}},
    "disabled": {"command":"draft-server"}
  }
}"#,
            )
            .unwrap();
        }
        fn assert_imported_native_config(&self, import: &ProfileImport) {
            assert_eq!(
                import.model_preference.clone().into_value(),
                serde_json::json!("gemini-imported")
            );
            assert_eq!(
                import.permission_preference.clone().into_value(),
                serde_json::json!("auto_edit")
            );
            let mcp = import.mcp_definitions.as_ref().unwrap();
            assert!(mcp.contains("\"env\": \"TOKEN\""));
            assert!(mcp.contains("\"enabled\": false"));
        }
        fn setup_native_command_for_import(&self, paths: &HarnessConfigPaths) {
            fs::write(
                paths.commands_dir.join("cmd.toml"),
                "prompt = \"command\"\n",
            )
            .unwrap();
        }
        fn assert_imported_command(&self, import: &ProfileImport) {
            assert_eq!(import.commands[0].relative_path, PathBuf::from("cmd.md"));
            assert_eq!(import.commands[0].contents, b"command");
        }
        fn setup_drift_native_config(&self, paths: &HarnessConfigPaths) {
            fs::write(
                &paths.settings_file,
                r#"{"model":{"name":"drift-model"},"general":{"defaultApprovalMode":"plan"}}"#,
            )
            .unwrap();
        }
        fn assert_drift_saved(&self, config: &ProfileConfig) {
            assert_eq!(config.model_preference("gemini"), "drift-model");
            assert_eq!(config.permission_preference("gemini"), "plan");
        }
        fn setup_drift_command(&self, paths: &HarnessConfigPaths) {
            fs::write(
                paths.commands_dir.join("new.toml"),
                "prompt = \"new command\"\n",
            )
            .unwrap();
        }
        fn assert_drift_command_saved(&self, active: &Path) {
            assert_eq!(
                fs::read_to_string(active.join("commands").join("new.md")).unwrap(),
                "new command"
            );
        }
        fn write_profile_config(&self, profile: &Path) {
            crate::integrations::test_suite::template::write_config(
                profile,
                r#"{
  "name": "work",
  "description": "",
  "models": {"gemini": "gemini-2.5-flash"},
  "permissions": {"gemini": "auto_edit"}
}"#,
            );
        }
        fn assert_applied_native_config(&self, paths: &HarnessConfigPaths) {
            let config = fs::read_to_string(&paths.settings_file).unwrap();
            assert!(config.contains("gemini-2.5-flash"));
            assert!(config.contains("auto_edit"));
            assert!(config.contains("mcpServers"));
            assert!(config.contains("local"));
            assert!(config.contains("server"));
            assert!(config.contains("disabled"));
            assert!(config.contains("excluded"));
        }
        fn assert_command_applied(
            &self,
            paths: &HarnessConfigPaths,
            profile: &Path,
            relative: &str,
        ) {
            let source = profile.join("commands").join(relative);
            let target = paths
                .commands_dir
                .join(markdown_command_to_toml(Path::new(relative)));
            assert_eq!(
                read_gemini_command_prompt(&target).unwrap(),
                fs::read_to_string(source).unwrap()
            );
        }
    }

    crate::define_standard_harness_tests!(GeminiAdapter);

    #[test]
    fn gemini_allowlist_and_exclusion_define_effective_enabled_state() {
        let document = json!({
            "mcpServers": {
                "enabled": {"command": "x"},
                "conflict": {"command": "x"},
                "not-allowed": {"command": "x"}
            },
            "mcp": {
                "allowed": ["enabled", "conflict"],
                "excluded": ["conflict"]
            }
        });
        let imported = import_gemini_mcps(document.as_object().unwrap()).unwrap();
        assert!(
            imported
                .iter()
                .find(|mcp| mcp.name == "enabled")
                .unwrap()
                .enabled
        );
        assert!(
            !imported
                .iter()
                .find(|mcp| mcp.name == "conflict")
                .unwrap()
                .enabled
        );
        assert!(
            !imported
                .iter()
                .find(|mcp| mcp.name == "not-allowed")
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn gemini_rejects_sse_mixed_transports_and_active_literal_substitutions() {
        for server in [
            json!({"url":"https://example.com"}),
            json!({"command":"x","httpUrl":"https://example.com"}),
            json!({"url":"https://sse","httpUrl":"https://http"}),
        ] {
            let native = json!({"mcpServers":{"x":server}});
            assert!(import_gemini_mcps(native.as_object().unwrap()).is_err());
        }
        for value in ["$TOKEN", "Bearer $TOKEN", "${TOKEN:-fallback}"] {
            let native = json!({"mcpServers":{"x":{"command":"x","env":{"TOKEN":value}}}});
            if matches!(value, "$TOKEN") {
                let imported = import_gemini_mcps(native.as_object().unwrap()).unwrap();
                assert!(matches!(
                    &imported[0].transport,
                    McpTransport::Stdio(stdio) if matches!(stdio.env["TOKEN"], McpValue::Env(_))
                ));
            } else {
                assert!(import_gemini_mcps(native.as_object().unwrap()).is_err());
            }
        }
        let windows = json!({"mcpServers":{"x":{"command":"x","env":{"TOKEN":"%TOKEN%"}}}});
        let imported = import_gemini_mcps(windows.as_object().unwrap()).unwrap();
        #[cfg(windows)]
        assert!(matches!(
            &imported[0].transport,
            McpTransport::Stdio(stdio) if matches!(stdio.env["TOKEN"], McpValue::Env(_))
        ));
        #[cfg(not(windows))]
        assert!(matches!(
            &imported[0].transport,
            McpTransport::Stdio(stdio) if matches!(stdio.env["TOKEN"], McpValue::Literal(_))
        ));
    }

    #[test]
    fn imports_nested_toml_commands_as_markdown_commands() {
        let temp = tempfile::tempdir().unwrap();
        let commands = temp.path().join("commands");
        fs::create_dir_all(commands.join("git")).unwrap();
        fs::write(
            commands.join("git").join("commit.toml"),
            "description = \"Commit\"\nprompt = \"write commit\"\n",
        )
        .unwrap();

        let imported = import_gemini_commands(&commands).unwrap();

        assert_eq!(imported[0].relative_path, PathBuf::from("git/commit.md"));
        assert_eq!(imported[0].contents, b"write commit");
    }

    #[test]
    fn renders_current_gemini_agent_fields() {
        let agent = SubAgent {
            name: "reviewer_2".to_string(),
            description: "Reviews code".to_string(),
            model: None,
            tools: None,
            permission: Some(crate::yaml::Value::String("ignored".to_string())),
            max_turns: Some(12),
            harness: BTreeMap::new(),
            body: "Review carefully.".to_string(),
        };

        let rendered = render_gemini_agent(&agent).unwrap();

        assert!(rendered.contains("max_turns: 12"));
        assert!(!rendered.contains("maxTurns"));
        assert!(!rendered.contains("permission:"));
    }

    #[test]
    fn gemini_rejects_malformed_agent_override_after_merge() {
        let mut harness = BTreeMap::new();
        harness.insert(
            "gemini".to_string(),
            crate::yaml::from_str("max_turns: bad").unwrap(),
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
        assert!(render_gemini_agent(&agent).is_err());
    }

    #[test]
    fn gemini_import_rejects_names_the_renderer_cannot_emit() {
        assert!(GeminiSubagentCodec
            .parse(
                Path::new("Reviewer.md"),
                "---\ndescription: Reviews\nkind: local\n---\nBody\n"
            )
            .is_err());
    }

    #[test]
    fn gemini_mcp_environment_references_round_trip() {
        let definitions = crate::profile::mcp::parse_mcp_definitions(
            r#"[{"name":"x","transport":"stdio","command":"x","env":{"TOKEN":{"env":"TOKEN"}}}]"#,
        )
        .unwrap();
        let mut config = NativeConfig::Json(json!({}));
        GeminiMcpCodec.apply(&mut config, &definitions).unwrap();
        assert_eq!(GeminiMcpCodec.import(&config).unwrap(), definitions);
    }

    #[test]
    fn gemini_rejects_malformed_mcp_root_and_ambiguous_literals() {
        for value in [json!("bad"), json!([]), json!(false), Value::Null] {
            assert!(GeminiMcpCodec
                .import(&NativeConfig::Json(json!({"mcp":value})))
                .is_err());
        }
        let definitions = crate::profile::mcp::parse_mcp_definitions(
            r#"[{"name":"x","transport":"stdio","command":"x","env":{"TOKEN":"${TOKEN}"}}]"#,
        )
        .unwrap();
        assert!(GeminiMcpCodec
            .preflight_apply(&NativeConfig::Json(json!({})), &definitions)
            .is_err());
    }
}
