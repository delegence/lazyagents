use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use crate::harness::artifacts::{
    collect_directory_link_drift, collect_directory_link_drift_recursive, import_commands,
    import_skills, link_commands, link_skills, profile_commands_recursive, valid_skills,
};
use crate::harness::drift::{DriftItem, DriftReport};
use crate::harness::fs::{
    detect_binary, read_json, read_optional_string, symlink_file, symlink_points_to,
};
use crate::harness::integration::{
    AppEnvironment, HarnessConfigPaths, HarnessDetection, HarnessIntegration, ImportedPreference,
    ProfileImport, ProfileRef,
};
use crate::harness::kind::HarnessKind;
use crate::harness::managed::{write_text_atomic, ManagedSurface};
use crate::profile::mcp::{
    canonical_mcp_json, parse_mcp_definitions, read_mcp_definitions, McpDefinition, McpTransport,
};
use crate::profile::ProfileConfig;

pub struct ClaudeIntegration;

impl HarnessIntegration for ClaudeIntegration {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Claude
    }

    fn detect(&self, env: &AppEnvironment) -> Result<HarnessDetection> {
        Ok(detect_binary(env, self.kind().binary_name()))
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
            settings_file: config_dir.join("settings.json"),
            mcp_file,
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
            ManagedSurface::preserved_file(&paths.mcp_file),
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
        collect_directory_link_drift_recursive(
            "commands",
            profile_commands_recursive(&active.path)?,
            &paths.commands_dir,
            &active.path.join("commands"),
            &mut items,
        )?;
        let native_mcps =
            parse_mcp_definitions(&import_claude_mcps(&read_json(&paths.mcp_file)?)?)?;
        let profile_mcps = read_mcp_definitions(&active.path)?;
        if canonical_mcp_json(&native_mcps)? != canonical_mcp_json(&profile_mcps)? {
            items.push(DriftItem {
                surface: "mcp".to_string(),
                detail: "Claude MCP list differs from active profile".to_string(),
            });
        }
        Ok(DriftReport { items })
    }

    fn import_from_harness(&self, paths: &HarnessConfigPaths) -> Result<ProfileImport> {
        let settings = read_json(&paths.settings_file)?;
        let mcps_doc = read_json(&paths.mcp_file)?;
        Ok(ProfileImport {
            instruction: read_optional_string(&paths.instruction_target)?,
            skills: import_skills(&paths.skills_dir)?,
            commands: import_commands(&paths.commands_dir)?,
            mcp_definitions: Some(import_claude_mcps(&mcps_doc)?),
            model_preference: ImportedPreference::new(
                settings
                    .get("primaryModel")
                    .cloned()
                    .unwrap_or_else(|| json!("default")),
            ),
            permission_preference: ImportedPreference::new(
                settings
                    .get("permissions")
                    .cloned()
                    .unwrap_or_else(|| json!("default")),
            ),
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
        link_commands(profile, paths)?;
        patch_claude_config(profile, paths)?;
        patch_claude_mcps(profile, paths)?;
        Ok(())
    }

    fn verify(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
        let instruction_source = profile.path.join("AGENTS.md");
        if !symlink_points_to(&paths.instruction_target, &instruction_source) {
            anyhow::bail!(
                "Claude instruction target {} does not point to {}",
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
                anyhow::bail!("Claude skill link {} was not applied", target.display());
            }
        }

        for command in profile_commands_recursive(&profile.path)? {
            let relative = command.strip_prefix(profile.path.join("commands")).unwrap();
            let target = paths.commands_dir.join(relative);
            if !symlink_points_to(&target, &command) {
                anyhow::bail!("Claude command link {} was not applied", target.display());
            }
        }

        let _ = read_json(&paths.settings_file)?;
        let _ = read_json(&paths.mcp_file)?;
        Ok(())
    }
}

fn patch_claude_config(profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
    let profile_config = read_profile_config(&profile.path)?;
    let mut document = read_json(&paths.settings_file)?;

    if let Some(model) = non_default_value(profile_config.model_preference(&profile.harness_id)) {
        document.insert("primaryModel".to_string(), model);
    }
    if let Some(permission) =
        non_default_value(profile_config.permission_preference(&profile.harness_id))
    {
        patch_claude_permissions(&mut document, permission)?;
    }

    write_text_atomic(
        &paths.settings_file,
        &serde_json::to_string_pretty(&document)?,
    )
    .with_context(|| format!("failed to write {}", paths.settings_file.display()))
}

fn patch_claude_mcps(profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
    let mcp_definitions = read_mcp_definitions(&profile.path)?;
    let mut document = read_json(&paths.mcp_file)?;

    document.remove("mcpServers");
    if !mcp_definitions.is_empty() {
        let mut servers = Map::new();
        for definition in mcp_definitions {
            servers.insert(definition.name.clone(), definition.to_claude_value()?);
        }
        document.insert("mcpServers".to_string(), Value::Object(servers));
    }

    write_text_atomic(&paths.mcp_file, &serde_json::to_string_pretty(&document)?)
        .with_context(|| format!("failed to write {}", paths.mcp_file.display()))
}

fn read_profile_config(profile_path: &Path) -> Result<ProfileConfig> {
    let path = profile_path.join("config.json");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("missing or unreadable profile config at {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("invalid profile config at {}", path.display()))
}

fn import_claude_mcps(document: &Map<String, Value>) -> Result<String> {
    let mut servers = Vec::new();
    let Some(mcp_servers) = document.get("mcpServers") else {
        return Ok("".to_string());
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

    if servers.is_empty() {
        Ok("".to_string())
    } else {
        Ok(format!("{}\n", serde_json::to_string_pretty(&servers)?))
    }
}

fn non_default_value(value: Value) -> Option<Value> {
    match value {
        Value::String(ref string_val) if string_val == "default" => None,
        other => Some(other),
    }
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
