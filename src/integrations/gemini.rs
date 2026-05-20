use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use toml_edit::{value, DocumentMut, Item};

use crate::harness::agents::{
    harness_scoped_value, remove_string, remove_value, select_harness_value,
    split_markdown_frontmatter, RenderedAgent, SubAgent,
};
use crate::harness::artifact::{
    CommandCodec, CommandMode, CommandsDirectory, HarnessArtifact, InstructionFile, JsonConfigFile,
    McpCodec, McpConfig, NativeConfig, PreferenceBinding, SettingsPreferences, SkillsDirectory,
    SubagentCodec, SubagentsDirectory,
};
use crate::harness::commands::profile_commands_recursive;
use crate::harness::drift::DriftItem;
use crate::harness::fs::import_files_recursive;
use crate::harness::integration::{
    AppEnvironment, HarnessConfigPaths, HarnessIntegration, ImportedFile, ProfileRef,
};
use crate::harness::kind::HarnessKind;
use crate::harness::managed::write_text_atomic;
use crate::profile::mcp::{McpDefinition, McpTransport};

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
        ]
    }

    fn settings(&self) -> Option<Box<dyn crate::harness::artifact::HarnessSettings>> {
        Some(Box::new(
            SettingsPreferences::new(
                JsonConfigFile::new(|paths| &paths.settings_file).label("Gemini settings JSON"),
            )
            .model(PreferenceBinding::JsonPointer {
                pointer: "/model/name",
            })
            .permission(PreferenceBinding::JsonPointer {
                pointer: "/general/defaultApprovalMode",
            }),
        ))
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
        Ok(render_gemini_agent(agent)?.contents)
    }

    fn should_import(&self, path: &Path) -> bool {
        path.extension().is_some_and(|ext| ext == "md")
    }

    fn parse(&self, path: &Path, contents: &str) -> Result<SubAgent> {
        let fallback_name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid agent path {}", path.display()))?;
        native_markdown_to_neutral(contents, fallback_name, "gemini")
    }
}

struct GeminiMcpCodec;

impl McpCodec for GeminiMcpCodec {
    fn import(&self, config: &NativeConfig) -> Result<Vec<McpDefinition>> {
        import_gemini_mcps(json_config_object(config)?)
    }

    fn apply(&self, config: &mut NativeConfig, definitions: &[McpDefinition]) -> Result<()> {
        patch_gemini_mcps(json_config_object_mut(config)?, definitions)
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
    for file in import_files_recursive(path, path)? {
        if file
            .relative_path
            .extension()
            .is_some_and(|extension| extension == "toml")
        {
            let command_path = path.join(&file.relative_path);
            commands.push(ImportedFile {
                relative_path: toml_command_to_markdown(&file.relative_path),
                contents: read_gemini_command_prompt(&command_path)?.into_bytes(),
            });
        }
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

fn render_gemini_agent(agent: &SubAgent) -> Result<RenderedAgent> {
    let mut map = serde_yaml::Mapping::new();
    map.insert(yaml_key("name"), agent.name.clone().into());
    map.insert(yaml_key("description"), agent.description.clone().into());
    map.insert(yaml_key("kind"), "local".into());
    if let Some(model) = select_harness_value(agent.model.as_ref(), "gemini") {
        map.insert(yaml_key("model"), model.clone());
    }
    if let Some(tools) = agent.tools.as_ref() {
        map.insert(yaml_key("tools"), tools_to_allow_list(tools));
    }
    if let Some(permission) = select_harness_value(agent.permission.as_ref(), "gemini") {
        map.insert(yaml_key("permission"), permission.clone());
    }
    if let Some(max_turns) = agent.max_turns {
        map.insert(
            yaml_key("maxTurns"),
            serde_yaml::Value::Number(max_turns.into()),
        );
    }
    merge_harness_override(&mut map, agent, "gemini")?;
    Ok(RenderedAgent {
        relative_path: PathBuf::from(format!("{}.md", agent.name)),
        contents: render_native_markdown(map, &agent.body)?,
    })
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
    remove_value(&mut map, "kind");
    remove_value(&mut map, "mode");
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
        let enabled = !excluded.contains(name);
        if let Some(command) = table.get("command").and_then(Value::as_str) {
            servers.push(json!({
                "name": name,
                "enabled": enabled,
                "transport": "stdio",
                "command": command,
                "args": string_array(table.get("args"), "Gemini MCP args")?,
                "env": string_object(table.get("env"), "Gemini MCP env")?,
            }));
        } else if let Some(url) = table
            .get("httpUrl")
            .or_else(|| table.get("url"))
            .and_then(Value::as_str)
        {
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
    document.remove("mcpServers");
    if !definitions.is_empty() {
        let mut servers = Map::new();
        for definition in definitions {
            servers.insert(definition.name.clone(), definition.to_gemini_value()?);
        }
        document.insert("mcpServers".to_string(), Value::Object(servers));
    }

    let managed_names = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut excluded = gemini_excluded_mcps(document)?
        .into_iter()
        .filter(|name| !managed_names.contains(name.as_str()))
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

fn json_config_object(config: &NativeConfig) -> Result<&Map<String, Value>> {
    let NativeConfig::Json(Value::Object(document)) = config else {
        anyhow::bail!("Gemini JSON config must be an object");
    };
    Ok(document)
}

fn json_config_object_mut(config: &mut NativeConfig) -> Result<&mut Map<String, Value>> {
    let NativeConfig::Json(value) = config else {
        anyhow::bail!("Gemini JSON config must be an object");
    };
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Gemini JSON config must be an object"))
}

fn gemini_excluded_mcps(document: &Map<String, Value>) -> Result<BTreeSet<String>> {
    let Some(excluded) = document
        .get("mcp")
        .and_then(Value::as_object)
        .and_then(|mcp| mcp.get("excluded"))
    else {
        return Ok(BTreeSet::new());
    };
    Ok(string_array(Some(excluded), "Gemini mcp.excluded")?
        .into_iter()
        .collect())
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

fn string_object(value: Option<&Value>, label: &str) -> Result<BTreeMap<String, String>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let Some(object) = value.as_object() else {
        anyhow::bail!("{label} must be an object");
    };
    object
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_string()))
                .ok_or_else(|| anyhow::anyhow!("{label} values must be strings"))
        })
        .collect()
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
                    map.insert("env".to_string(), json!(stdio.env));
                }
            }
            McpTransport::Http(http) => {
                map.insert("httpUrl".to_string(), json!(http.url));
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
            assert!(mcp.contains("\"Authorization\": \"$TOKEN\""));
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
}
