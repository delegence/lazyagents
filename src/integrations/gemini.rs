use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use toml_edit::{value, DocumentMut, Item};

use crate::harness::artifacts::{
    collect_directory_link_drift, import_files_recursive, import_skills, link_skills,
    profile_commands_recursive, valid_skills,
};
use crate::harness::drift::{DriftItem, DriftReport};
use crate::harness::fs::{
    detect_binary, read_json, read_optional_string, symlink_file, symlink_points_to,
};
use crate::harness::integration::{
    AppEnvironment, HarnessConfigPaths, HarnessDetection, HarnessIntegration, ImportedFile,
    ImportedPreference, ProfileImport, ProfileRef,
};
use crate::harness::kind::HarnessKind;
use crate::harness::managed::{write_text_atomic, ManagedSurface};
use crate::profile::mcp::{
    canonical_mcp_json, parse_mcp_definitions, read_mcp_definitions, McpDefinition, McpTransport,
};
use crate::profile::ProfileConfig;

pub struct GeminiIntegration;

impl HarnessIntegration for GeminiIntegration {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Gemini
    }

    fn detect(&self, env: &AppEnvironment) -> Result<HarnessDetection> {
        Ok(detect_binary(env, self.kind().binary_name()))
    }

    fn default_config_dir(&self, env: &AppEnvironment) -> std::path::PathBuf {
        env.user_home.join(".gemini")
    }

    fn paths_from_config_dir(&self, config_dir: std::path::PathBuf) -> Result<HarnessConfigPaths> {
        Ok(HarnessConfigPaths {
            instruction_target: config_dir.join("GEMINI.md"),
            skills_dir: config_dir.join("skills"),
            commands_dir: config_dir.join("commands"),
            settings_file: config_dir.join("settings.json"),
            mcp_file: config_dir.join("settings.json"),
            config_dir,
        })
    }

    fn paths(&self, env: &AppEnvironment) -> Result<HarnessConfigPaths> {
        self.paths_from_config_dir(self.default_config_dir(env))
    }

    fn managed_surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface> {
        vec![
            ManagedSurface::file(&paths.instruction_target),
            ManagedSurface::directory(&paths.skills_dir),
            ManagedSurface::directory(&paths.commands_dir),
            ManagedSurface::preserved_file(&paths.settings_file),
        ]
    }

    fn preflight(&self, _profile: &ProfileRef) -> Result<()> {
        Ok(())
    }

    fn detect_drift(&self, active: &ProfileRef, paths: &HarnessConfigPaths) -> Result<DriftReport> {
        let mut items = Vec::new();
        let instruction_source = active.path.join("AGENTS.md");
        if !symlink_points_to(&paths.instruction_target, &instruction_source) {
            items.push(DriftItem {
                surface: "instructions".to_string(),
                detail: format!(
                    "{} is not linked to active profile",
                    paths.instruction_target.display()
                ),
            });
        }
        collect_directory_link_drift(
            "skills",
            valid_skills(&active.path)?,
            &paths.skills_dir,
            &mut items,
        )?;
        collect_gemini_command_drift(active, paths, &mut items)?;

        let native_mcps =
            parse_mcp_definitions(&import_gemini_mcps(&read_json(&paths.mcp_file)?)?)?;
        let profile_mcps = read_mcp_definitions(&active.path)?;
        if canonical_mcp_json(&native_mcps)? != canonical_mcp_json(&profile_mcps)? {
            items.push(DriftItem {
                surface: "mcp".to_string(),
                detail: "Gemini MCP list differs from active profile".to_string(),
            });
        }
        Ok(DriftReport { items })
    }

    fn import_from_harness(&self, paths: &HarnessConfigPaths) -> Result<ProfileImport> {
        let settings = read_json(&paths.settings_file)?;
        Ok(ProfileImport {
            instruction: read_optional_string(&paths.instruction_target)?,
            skills: import_skills(&paths.skills_dir)?,
            commands: import_gemini_commands(&paths.commands_dir)?,
            mcp_definitions: Some(import_gemini_mcps(&settings)?),
            model_preference: ImportedPreference::new(import_nested_setting(
                &settings,
                &["model", "name"],
            )),
            permission_preference: ImportedPreference::new(import_nested_setting(
                &settings,
                &["general", "defaultApprovalMode"],
            )),
        })
    }

    fn apply(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
        fs::create_dir_all(&paths.config_dir)
            .with_context(|| format!("failed to create {}", paths.config_dir.display()))?;
        fs::create_dir_all(&paths.skills_dir)
            .with_context(|| format!("failed to create {}", paths.skills_dir.display()))?;
        fs::create_dir_all(&paths.commands_dir)
            .with_context(|| format!("failed to create {}", paths.commands_dir.display()))?;

        symlink_file(profile.path.join("AGENTS.md"), &paths.instruction_target)?;
        link_skills(profile, paths)?;
        write_gemini_commands(profile, paths)?;
        patch_gemini_settings(profile, paths)?;
        Ok(())
    }

    fn verify(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
        let instruction_source = profile.path.join("AGENTS.md");
        if !symlink_points_to(&paths.instruction_target, &instruction_source) {
            anyhow::bail!(
                "Gemini instruction target {} does not point to {}",
                paths.instruction_target.display(),
                instruction_source.display()
            );
        }

        for skill in valid_skills(&profile.path)? {
            let target = paths.skills_dir.join(
                skill
                    .file_name()
                    .ok_or_else(|| anyhow::anyhow!("invalid skill path {}", skill.display()))?,
            );
            if !symlink_points_to(&target, &skill) {
                anyhow::bail!("Gemini skill link {} was not applied", target.display());
            }
        }

        for command in profile_commands_recursive(&profile.path)? {
            let relative = command.strip_prefix(profile.path.join("commands")).unwrap();
            let target = paths.commands_dir.join(markdown_command_to_toml(relative));
            let actual = read_gemini_command_prompt(&target)?;
            let expected = fs::read_to_string(&command)
                .with_context(|| format!("failed to read {}", command.display()))?;
            if actual != expected {
                anyhow::bail!("Gemini command {} was not applied", target.display());
            }
        }

        let _ = read_json(&paths.settings_file)?;
        Ok(())
    }
}

fn collect_gemini_command_drift(
    active: &ProfileRef,
    paths: &HarnessConfigPaths,
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

    let actual = import_gemini_commands(&paths.commands_dir)?
        .into_iter()
        .map(|file| (file.relative_path, file.contents))
        .collect::<BTreeMap<_, _>>();

    for (relative, contents) in &expected {
        if actual.get(relative) != Some(contents) {
            items.push(DriftItem {
                surface: "commands".to_string(),
                detail: format!(
                    "{} does not match active profile",
                    paths
                        .commands_dir
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
                    "unexpected managed entry {}",
                    paths
                        .commands_dir
                        .join(markdown_command_to_toml(relative))
                        .display()
                ),
            });
        }
    }
    Ok(())
}

fn write_gemini_commands(profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
    for command in profile_commands_recursive(&profile.path)? {
        let relative = command.strip_prefix(profile.path.join("commands")).unwrap();
        let target = paths.commands_dir.join(markdown_command_to_toml(relative));
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

fn patch_gemini_settings(profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
    let profile_config = read_profile_config(&profile.path)?;
    let mcp_definitions = read_mcp_definitions(&profile.path)?;
    let mut document = read_json(&paths.settings_file)?;

    if let Some(model) = non_default_string(
        profile_config.model_preference(&profile.harness_id),
        "model preference",
    )? {
        set_nested_value(&mut document, &["model", "name"], json!(model))?;
    }
    if let Some(permission) = non_default_string(
        profile_config.permission_preference(&profile.harness_id),
        "permission preference",
    )? {
        set_nested_value(
            &mut document,
            &["general", "defaultApprovalMode"],
            json!(permission),
        )?;
    }

    patch_gemini_mcps(&mut document, &mcp_definitions)?;

    write_text_atomic(
        &paths.settings_file,
        &serde_json::to_string_pretty(&document)?,
    )
    .with_context(|| format!("failed to write {}", paths.settings_file.display()))
}

fn read_profile_config(profile_path: &Path) -> Result<ProfileConfig> {
    let path = profile_path.join("config.json");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("missing or unreadable profile config at {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("invalid profile config at {}", path.display()))
}

fn import_nested_setting(settings: &Map<String, Value>, path: &[&str]) -> Value {
    let mut current = Value::Object(settings.clone());
    for key in path {
        let Some(next) = current.get(*key).cloned() else {
            return json!("default");
        };
        current = next;
    }
    current
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

fn non_default_string(value: Value, label: &str) -> Result<Option<String>> {
    match value {
        Value::String(value) if value == "default" => Ok(None),
        Value::String(value) => Ok(Some(value)),
        other => anyhow::bail!("Gemini {label} must be a string or \"default\", got {other}"),
    }
}

fn import_gemini_mcps(document: &Map<String, Value>) -> Result<String> {
    let mut servers = Vec::new();
    let excluded = gemini_excluded_mcps(document)?;
    let Some(mcp_servers) = document.get("mcpServers") else {
        return Ok(String::new());
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

    if servers.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!("{}\n", serde_json::to_string_pretty(&servers)?))
    }
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
