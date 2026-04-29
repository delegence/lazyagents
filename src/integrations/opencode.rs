use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use crate::harness::drift::{DriftItem, DriftReport};
use crate::harness::integration::{
    Detection, HarnessIntegration, HarnessPaths, ImportedDirectory, ImportedFile, LoadedProfile,
    PreferenceImport, ProfileImport, RuntimeEnv,
};
use crate::harness::kind::HarnessKind;
use crate::harness::managed::{write_text_atomic, ManagedSurface};
use crate::profile::mcp::{read_mcp_definitions, McpDefinition, McpTransport};
use crate::profile::ProfileConfig;

pub struct OpenCodeIntegration;

impl HarnessIntegration for OpenCodeIntegration {
    fn kind(&self) -> HarnessKind {
        HarnessKind::OpenCode
    }

    fn detect(&self, env: &RuntimeEnv) -> Result<Detection> {
        Ok(detect_binary(env, self.kind().binary_name()))
    }

    fn paths(&self, env: &RuntimeEnv) -> Result<HarnessPaths> {
        let config_dir = env.user_home.join(".config").join("opencode");
        Ok(HarnessPaths {
            instruction_target: config_dir.join("AGENTS.md"),
            skills_dir: config_dir.join("skills"),
            commands_dir: config_dir.join("commands"),
            settings_file: config_dir.join("opencode.json"),
            mcp_file: config_dir.join("opencode.json"),
            config_dir,
        })
    }

    fn managed_surfaces(&self, paths: &HarnessPaths) -> Vec<ManagedSurface> {
        vec![
            ManagedSurface::file(&paths.instruction_target),
            ManagedSurface::directory(&paths.skills_dir),
            ManagedSurface::directory(&paths.commands_dir),
            ManagedSurface::preserved_file(&paths.settings_file),
        ]
    }

    fn preflight(&self, _profile: &LoadedProfile) -> Result<()> {
        Ok(())
    }

    fn detect_drift(&self, active: &LoadedProfile, paths: &HarnessPaths) -> Result<DriftReport> {
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
        let native_mcps = import_opencode_mcps(&read_json(&paths.mcp_file)?)?;
        let profile_mcps = fs::read_to_string(active.path.join("mcps.json")).unwrap_or_default();
        if normalize_json_text(&native_mcps) != normalize_json_text(&profile_mcps) {
            items.push(DriftItem {
                surface: "mcp".to_string(),
                detail: "OpenCode MCP list differs from active profile".to_string(),
            });
        }
        Ok(DriftReport { items })
    }

    fn import_from_harness(&self, paths: &HarnessPaths) -> Result<ProfileImport> {
        let settings = read_json(&paths.settings_file)?;
        Ok(ProfileImport {
            instruction: read_optional_string(&paths.instruction_target)?,
            skills: import_skills(&paths.skills_dir)?,
            commands: import_commands(&paths.commands_dir)?,
            mcp_definitions: Some(import_opencode_mcps(&settings)?),
            model_preference: PreferenceImport::new(
                settings
                    .get("model")
                    .cloned()
                    .unwrap_or_else(|| json!("default")),
            ),
            permission_preference: PreferenceImport::new(
                settings
                    .get("permissions")
                    .cloned()
                    .unwrap_or_else(|| json!("default")),
            ),
        })
    }

    fn apply(&self, profile: &LoadedProfile, paths: &HarnessPaths) -> Result<()> {
        fs::create_dir_all(&paths.config_dir)
            .with_context(|| format!("failed to create {}", paths.config_dir.display()))?;
        fs::create_dir_all(&paths.skills_dir)
            .with_context(|| format!("failed to create {}", paths.skills_dir.display()))?;
        fs::create_dir_all(&paths.commands_dir)
            .with_context(|| format!("failed to create {}", paths.commands_dir.display()))?;

        symlink_file(profile.path.join("AGENTS.md"), &paths.instruction_target)?;
        link_skills(profile, paths)?;
        link_commands(profile, paths)?;
        patch_opencode_config(profile, paths)?;
        Ok(())
    }

    fn verify(&self, profile: &LoadedProfile, paths: &HarnessPaths) -> Result<()> {
        let instruction_source = profile.path.join("AGENTS.md");
        if !symlink_points_to(&paths.instruction_target, &instruction_source) {
            anyhow::bail!(
                "OpenCode instruction target {} does not point to {}",
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
                anyhow::bail!("OpenCode skill link {} was not applied", target.display());
            }
        }

        for command in profile_commands_recursive(&profile.path)? {
            let relative = command
                .strip_prefix(&profile.path.join("commands"))
                .unwrap();
            let target = paths.commands_dir.join(relative);
            if !symlink_points_to(&target, &command) {
                anyhow::bail!("OpenCode command link {} was not applied", target.display());
            }
        }

        let _ = read_json(&paths.settings_file)?;
        Ok(())
    }
}

fn link_skills(profile: &LoadedProfile, paths: &HarnessPaths) -> Result<()> {
    for skill in valid_skills(&profile.path)? {
        let target = paths.skills_dir.join(
            skill
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("invalid skill path {}", skill.display()))?,
        );
        symlink_dir(skill, target)?;
    }
    Ok(())
}

fn link_commands(profile: &LoadedProfile, paths: &HarnessPaths) -> Result<()> {
    for command in profile_commands_recursive(&profile.path)? {
        let relative = command
            .strip_prefix(&profile.path.join("commands"))
            .unwrap();
        let target = paths.commands_dir.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        symlink_file(command, target)?;
    }
    Ok(())
}

fn collect_directory_link_drift(
    surface: &str,
    expected_sources: Vec<PathBuf>,
    target_dir: &Path,
    items: &mut Vec<DriftItem>,
) -> Result<()> {
    let mut expected_names = BTreeSet::new();
    for source in expected_sources {
        let name = source
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("invalid source path {}", source.display()))?
            .to_string_lossy()
            .into_owned();
        expected_names.insert(name.clone());
        if !symlink_points_to(&target_dir.join(&name), &source) {
            items.push(DriftItem {
                surface: surface.to_string(),
                detail: format!(
                    "{} is not linked to active profile",
                    target_dir.join(&name).display()
                ),
            });
        }
    }
    if target_dir.exists() {
        for entry in fs::read_dir(target_dir)
            .with_context(|| format!("failed to read {}", target_dir.display()))?
        {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if !expected_names.contains(&name) {
                items.push(DriftItem {
                    surface: surface.to_string(),
                    detail: format!("unexpected managed entry {}", entry.path().display()),
                });
            }
        }
    }
    Ok(())
}

fn collect_directory_link_drift_recursive(
    surface: &str,
    expected_sources: Vec<PathBuf>,
    target_dir: &Path,
    profile_cmd_dir: &Path,
    items: &mut Vec<DriftItem>,
) -> Result<()> {
    let mut expected_rel_paths = BTreeSet::new();
    for source in expected_sources {
        let rel_path = source.strip_prefix(profile_cmd_dir).unwrap().to_path_buf();
        expected_rel_paths.insert(rel_path.clone());
        let target = target_dir.join(&rel_path);
        if !symlink_points_to(&target, &source) {
            items.push(DriftItem {
                surface: surface.to_string(),
                detail: format!("{} is not linked to active profile", target.display()),
            });
        }
    }
    if target_dir.exists() {
        let actual_files = import_files_recursive(target_dir, target_dir)?;
        for file in actual_files {
            if !expected_rel_paths.contains(&file.relative_path) {
                items.push(DriftItem {
                    surface: surface.to_string(),
                    detail: format!(
                        "unexpected managed entry {}",
                        target_dir.join(&file.relative_path).display()
                    ),
                });
            }
        }
    }
    Ok(())
}

fn normalize_json_text(text: &str) -> Value {
    if text.trim().is_empty() {
        json!([])
    } else {
        serde_json::from_str(text).unwrap_or_else(|_| json!(text.trim()))
    }
}

fn read_optional_string(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn import_skills(path: &Path) -> Result<Vec<ImportedDirectory>> {
    let mut skills = Vec::new();
    if !path.exists() {
        return Ok(skills);
    }
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        if !entry.path().metadata()?.is_dir() || !entry.path().join("SKILL.md").is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        skills.push(ImportedDirectory {
            name,
            files: import_files_recursive(&entry.path(), &entry.path())?,
        });
    }
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(skills)
}

fn import_commands(path: &Path) -> Result<Vec<ImportedFile>> {
    let mut commands = Vec::new();
    if !path.exists() {
        return Ok(commands);
    }
    let files = import_files_recursive(path, path)?;
    for file in files {
        if file
            .relative_path
            .extension()
            .is_some_and(|ext| ext == "md")
        {
            commands.push(file);
        }
    }
    commands.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(commands)
}

fn import_files_recursive(root: &Path, path: &Path) -> Result<Vec<ImportedFile>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.metadata()?.is_dir() {
            files.extend(import_files_recursive(root, &path)?);
        } else if path.metadata()?.is_file() {
            files.push(ImportedFile {
                relative_path: path
                    .strip_prefix(root)
                    .with_context(|| format!("{} is not under {}", path.display(), root.display()))?
                    .to_path_buf(),
                contents: fs::read(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?,
            });
        }
    }
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

fn valid_skills(profile_path: &Path) -> Result<Vec<PathBuf>> {
    let skills_dir = profile_path.join("skills");
    let mut skills = Vec::new();
    if !skills_dir.exists() {
        return Ok(skills);
    }
    for entry in fs::read_dir(&skills_dir)
        .with_context(|| format!("failed to read {}", skills_dir.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("SKILL.md").is_file() {
            skills.push(entry.path());
        }
    }
    skills.sort();
    Ok(skills)
}

fn profile_commands_recursive(profile_path: &Path) -> Result<Vec<PathBuf>> {
    let commands_dir = profile_path.join("commands");
    let mut commands = Vec::new();
    if !commands_dir.exists() {
        return Ok(commands);
    }
    collect_commands_recursive(&commands_dir, &mut commands)?;
    commands.sort();
    Ok(commands)
}

fn collect_commands_recursive(dir: &Path, commands: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect_commands_recursive(&entry.path(), commands)?;
        } else if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "md")
        {
            commands.push(entry.path());
        }
    }
    Ok(())
}

fn patch_opencode_config(profile: &LoadedProfile, paths: &HarnessPaths) -> Result<()> {
    let profile_config = read_profile_config(&profile.path)?;
    let mut document = read_json(&paths.settings_file)?;

    if let Some(model) = non_default_value(profile_config.model_preference("opencode")) {
        document.insert("model".to_string(), model);
    }
    if let Some(permission) = non_default_value(profile_config.permission_preference("opencode")) {
        document.insert("permissions".to_string(), permission);
    }

    let mcp_definitions = read_mcp_definitions(&profile.path)?;
    document.remove("mcp");
    if !mcp_definitions.is_empty() {
        let mut servers = Map::new();
        for definition in mcp_definitions
            .into_iter()
            .filter(|definition| definition.enabled)
        {
            servers.insert(definition.name.clone(), definition.to_opencode_value()?);
        }
        document.insert("mcp".to_string(), Value::Object(servers));
    }

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

fn read_json(path: &Path) -> Result<Map<String, Value>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()))
        }
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("invalid JSON at {}", path.display()))?;
    if let Value::Object(map) = value {
        Ok(map)
    } else {
        anyhow::bail!("JSON at {} is not an object", path.display())
    }
}

fn import_opencode_mcps(document: &Map<String, Value>) -> Result<String> {
    let mut servers = Vec::new();
    let Some(mcp_servers) = document.get("mcp") else {
        return Ok("".to_string());
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

impl McpDefinition {
    fn to_opencode_value(&self) -> Result<Value> {
        let mut map = Map::new();
        map.insert("enabled".to_string(), json!(true));
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

fn detect_binary(env: &RuntimeEnv, binary_name: &str) -> Detection {
    for path in &env.path_entries {
        let binary_path = path.join(binary_name);
        if binary_path.is_file() {
            return Detection::Detected { binary_path };
        }
    }
    Detection::NotDetected
}

fn symlink_points_to(link: &Path, source: &Path) -> bool {
    fs::read_link(link)
        .map(|target| target == source)
        .unwrap_or(false)
}

#[cfg(unix)]
fn symlink_file(source: impl AsRef<Path>, target: impl AsRef<Path>) -> Result<()> {
    std::os::unix::fs::symlink(source.as_ref(), target.as_ref())
        .with_context(|| format!("failed to link {}", target.as_ref().display()))
}

#[cfg(unix)]
fn symlink_dir(source: impl AsRef<Path>, target: impl AsRef<Path>) -> Result<()> {
    std::os::unix::fs::symlink(source.as_ref(), target.as_ref())
        .with_context(|| format!("failed to link {}", target.as_ref().display()))
}

#[cfg(windows)]
fn symlink_file(source: impl AsRef<Path>, target: impl AsRef<Path>) -> Result<()> {
    std::os::windows::fs::symlink_file(source.as_ref(), target.as_ref())
        .with_context(|| format!("failed to link {}", target.as_ref().display()))
}

#[cfg(windows)]
fn symlink_dir(source: impl AsRef<Path>, target: impl AsRef<Path>) -> Result<()> {
    std::os::windows::fs::symlink_dir(source.as_ref(), target.as_ref())
        .with_context(|| format!("failed to link {}", target.as_ref().display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::apply::{use_profile, DriftPolicy};
    use crate::profile::{LazyagentsHome, ProfileName, ProfileStore};

    #[test]
    fn opencode_use_applies_profile_artifacts_preferences_mcp_and_state() {
        let fixture = OpenCodeFixture::new();
        let profile = fixture.profile("work");
        add_skill(&profile, "writer");
        add_command(&profile, "plan.md");
        write_config(
            &profile,
            r#"{
  "name": "work",
  "description": "",
  "models": {"opencode": "gpt-4"},
  "permissions": {"opencode": {"*": "ask"}}
}"#,
        );
        fs::write(
            profile.join("mcps.json"),
            r#"[
  {"name":"local","transport":"stdio","command":"server","args":["--x"],"env":{"TOKEN":"$TOKEN"}},
  {"name":"remote","transport":"http","url":"https://mcp.example","headers":{"Authorization":"$TOKEN","X-Literal":"abc"}},
  {"name":"disabled","enabled":false,"transport":"stdio","command":"draft-server"}
]"#,
        )
        .unwrap();
        fs::create_dir_all(fixture.opencode_dir()).unwrap();
        fs::write(
            fixture.opencode_dir().join("opencode.json"),
            "{\"other\": true}",
        )
        .unwrap();

        use_profile(
            &OpenCodeIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("work").unwrap(),
            DriftPolicy::Discard,
        )
        .unwrap();

        assert_symlink_to(
            fixture.opencode_dir().join("AGENTS.md"),
            profile.join("AGENTS.md"),
        );
        assert_symlink_to(
            fixture.opencode_dir().join("skills").join("writer"),
            profile.join("skills").join("writer"),
        );
        assert_symlink_to(
            fixture.opencode_dir().join("commands").join("plan.md"),
            profile.join("commands").join("plan.md"),
        );
        let config = fs::read_to_string(fixture.opencode_dir().join("opencode.json")).unwrap();
        assert!(config.contains("\"other\": true"));
        assert!(config.contains("\"model\": \"gpt-4\""));
        assert!(config.contains("\"permissions\": {"));
        assert!(config.contains("\"*\": \"ask\""));

        assert!(config.contains("\"mcp\": {"));
        assert!(config.contains("\"local\""));
        assert!(config.contains("\"server\""));
        assert!(config.contains("\"remote\""));
        assert!(!config.contains("\"disabled\""));
        assert_eq!(
            fs::read_to_string(fixture.home.join("state.json")).unwrap(),
            "{\n  \"active_profiles\": {\n    \"opencode\": \"work\"\n  }\n}\n"
        );
    }

    #[test]
    fn opencode_use_normalizes_missing_optional_artifacts() {
        let fixture = OpenCodeFixture::new();
        let profile = fixture.profile("work");
        fs::remove_file(profile.join("AGENTS.md")).unwrap();
        fs::remove_file(profile.join("mcps.json")).unwrap();
        fs::remove_dir_all(profile.join("skills")).unwrap();
        fs::remove_dir_all(profile.join("commands")).unwrap();

        use_profile(
            &OpenCodeIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("work").unwrap(),
            DriftPolicy::Discard,
        )
        .unwrap();

        assert!(profile.join("AGENTS.md").is_file());
        assert!(profile.join("mcps.json").is_file());
        assert!(profile.join("skills").is_dir());
        assert!(profile.join("commands").is_dir());
        assert_symlink_to(
            fixture.opencode_dir().join("AGENTS.md"),
            profile.join("AGENTS.md"),
        );
    }

    #[test]
    fn opencode_use_removes_stale_surfaces_and_clears_mcp_list() {
        let fixture = OpenCodeFixture::new();
        let full = fixture.profile("full");
        add_skill(&full, "writer");
        add_command(&full, "plan.md");
        fs::write(
            full.join("mcps.json"),
            r#"[{"name":"local","transport":"stdio","command":"server"}]"#,
        )
        .unwrap();
        fixture.profile("empty");

        use_profile(
            &OpenCodeIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("full").unwrap(),
            DriftPolicy::Discard,
        )
        .unwrap();
        use_profile(
            &OpenCodeIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("empty").unwrap(),
            DriftPolicy::Discard,
        )
        .unwrap();

        assert!(fs::read_dir(fixture.opencode_dir().join("skills"))
            .unwrap()
            .next()
            .is_none());
        assert!(fs::read_dir(fixture.opencode_dir().join("commands"))
            .unwrap()
            .next()
            .is_none());
        let config = fs::read_to_string(fixture.opencode_dir().join("opencode.json")).unwrap();
        assert!(!config.contains("\"mcp\""));
    }

    #[test]
    fn opencode_use_default_preferences_do_not_modify_existing_native_settings() {
        let fixture = OpenCodeFixture::new();
        fixture.profile("work");
        fs::create_dir_all(fixture.opencode_dir()).unwrap();
        fs::write(
            fixture.opencode_dir().join("opencode.json"),
            "{\"model\": \"existing\", \"permissions\": \"never\"}",
        )
        .unwrap();

        use_profile(
            &OpenCodeIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("work").unwrap(),
            DriftPolicy::Discard,
        )
        .unwrap();

        let config = fs::read_to_string(fixture.opencode_dir().join("opencode.json")).unwrap();
        assert!(config.contains("\"model\": \"existing\""));
        assert!(config.contains("\"permissions\": \"never\""));
    }

    #[test]
    fn opencode_use_allows_nested_commands() {
        let fixture = OpenCodeFixture::new();
        let profile = fixture.profile("work");
        fs::create_dir_all(profile.join("commands").join("nested")).unwrap();
        fs::write(profile.join("commands").join("nested").join("good.md"), "").unwrap();

        use_profile(
            &OpenCodeIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("work").unwrap(),
            DriftPolicy::Discard,
        )
        .unwrap();

        assert!(fixture
            .opencode_dir()
            .join("commands")
            .join("nested")
            .join("good.md")
            .exists());
        assert_symlink_to(
            fixture
                .opencode_dir()
                .join("commands")
                .join("nested")
                .join("good.md"),
            profile.join("commands").join("nested").join("good.md"),
        );
    }

    #[test]
    fn opencode_use_rejects_invalid_disabled_mcp_without_state_update() {
        let fixture = OpenCodeFixture::new();
        let profile = fixture.profile("work");
        fs::write(
            profile.join("mcps.json"),
            r#"[{"name":"draft","enabled":false}]"#,
        )
        .unwrap();

        let error = use_profile(
            &OpenCodeIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("work").unwrap(),
            DriftPolicy::Discard,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("MCP draft requires transport"));
        assert!(!fixture.home.join("state.json").exists());
        assert!(!fixture.opencode_dir().join("opencode.json").exists());
    }

    #[test]
    fn opencode_use_rolls_back_and_dereferences_symlink_backup_on_failure() {
        let fixture = OpenCodeFixture::new();
        let profile = fixture.profile("work");
        fs::write(
            profile.join("mcps.json"),
            r#"[{"name":"bad","transport":"stdio"}]"#,
        )
        .unwrap();
        fs::create_dir_all(fixture.opencode_dir()).unwrap();
        let old_source = fixture.temp.path().join("old-source.md");
        fs::write(&old_source, "previous instructions").unwrap();
        symlink_file(&old_source, fixture.opencode_dir().join("AGENTS.md")).unwrap();
        fs::create_dir_all(fixture.opencode_dir().join("skills")).unwrap();
        fs::write(fixture.opencode_dir().join("skills").join("old.txt"), "old").unwrap();
        fs::write(
            fixture.opencode_dir().join("opencode.json"),
            "{\"model\": \"old\"}",
        )
        .unwrap();

        let error = use_profile(
            &OpenCodeIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("work").unwrap(),
            DriftPolicy::Discard,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("requires command"));
        assert_eq!(
            fs::read_to_string(fixture.opencode_dir().join("AGENTS.md")).unwrap(),
            "previous instructions"
        );
        assert!(
            !fs::symlink_metadata(fixture.opencode_dir().join("AGENTS.md"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_to_string(fixture.opencode_dir().join("skills").join("old.txt")).unwrap(),
            "old"
        );
        assert_eq!(
            fs::read_to_string(fixture.opencode_dir().join("opencode.json")).unwrap(),
            "{\"model\": \"old\"}"
        );
        assert!(!fixture.home.join("state.json").exists());
    }

    #[test]
    fn opencode_import_reads_managed_state_and_dereferences_symlinks() {
        let fixture = OpenCodeFixture::new();
        fs::create_dir_all(fixture.opencode_dir().join("skills")).unwrap();
        fs::create_dir_all(fixture.opencode_dir().join("commands")).unwrap();
        let instruction_source = fixture.temp.path().join("instruction-source.md");
        fs::write(&instruction_source, "imported instructions").unwrap();
        symlink_file(
            &instruction_source,
            fixture.opencode_dir().join("AGENTS.md"),
        )
        .unwrap();
        let skill_source = fixture.temp.path().join("skill-source");
        fs::create_dir_all(&skill_source).unwrap();
        fs::write(skill_source.join("SKILL.md"), "skill body").unwrap();
        symlink_dir(
            &skill_source,
            fixture.opencode_dir().join("skills").join("linked"),
        )
        .unwrap();
        fs::write(
            fixture.opencode_dir().join("commands").join("cmd.md"),
            "command",
        )
        .unwrap();
        fs::write(
            fixture.opencode_dir().join("opencode.json"),
            r#"{
              "model": "gpt-imported",
              "permissions": {"*": "ask"},
              "mcp": {
                "local": {
                  "type": "local",
                  "command": ["server", "--flag"],
                  "environment": {
                    "TOKEN": "$TOKEN"
                  },
                  "enabled": true
                },
                "remote": {
                  "type": "remote",
                  "url": "https://mcp.example",
                  "headers": {
                    "X-Literal": "abc"
                  },
                  "enabled": false
                }
              }
            }"#,
        )
        .unwrap();

        let paths = OpenCodeIntegration.paths(&fixture.env).unwrap();

        let imported = OpenCodeIntegration.import_from_harness(&paths).unwrap();

        assert_eq!(
            imported.instruction.as_deref(),
            Some("imported instructions")
        );
        assert_eq!(imported.skills[0].name, "linked");
        assert_eq!(imported.skills[0].files[0].contents, b"skill body");
        assert_eq!(imported.commands[0].contents, b"command");
        assert_eq!(
            imported.model_preference.into_value(),
            serde_json::json!("gpt-imported")
        );
        assert_eq!(
            imported.permission_preference.into_value(),
            serde_json::json!({"*": "ask"})
        );
        assert!(imported
            .mcp_definitions
            .as_ref()
            .unwrap()
            .contains("\"$TOKEN\""));
        assert!(imported
            .mcp_definitions
            .as_ref()
            .unwrap()
            .contains("\"enabled\": false"));
    }

    #[test]
    fn opencode_import_fails_on_malformed_native_config() {
        let fixture = OpenCodeFixture::new();
        fs::create_dir_all(fixture.opencode_dir()).unwrap();
        fs::write(fixture.opencode_dir().join("opencode.json"), "not = [").unwrap();
        let paths = OpenCodeIntegration.paths(&fixture.env).unwrap();

        let error = OpenCodeIntegration.import_from_harness(&paths).unwrap_err();

        assert!(error.to_string().contains("invalid JSON at"));
    }

    #[test]
    fn opencode_save_changes_imports_drift_into_active_profile_before_switching() {
        let fixture = OpenCodeFixture::new();
        let active = fixture.profile("active");
        let target = fixture.profile("target");
        fs::write(
            fixture.home.join("state.json"),
            r#"{"active_profiles":{"opencode":"active"}}"#,
        )
        .unwrap();
        fs::create_dir_all(fixture.opencode_dir().join("skills").join("newskill")).unwrap();
        fs::write(
            fixture
                .opencode_dir()
                .join("skills")
                .join("newskill")
                .join("SKILL.md"),
            "new skill",
        )
        .unwrap();
        fs::create_dir_all(fixture.opencode_dir().join("commands")).unwrap();
        fs::write(
            fixture.opencode_dir().join("commands").join("new.md"),
            "new command",
        )
        .unwrap();
        fs::write(fixture.opencode_dir().join("AGENTS.md"), "drifted").unwrap();
        fs::write(
            fixture.opencode_dir().join("opencode.json"),
            "{\"model\": \"drift-model\", \"permissions\": \"drift-perm\"}",
        )
        .unwrap();

        use_profile(
            &OpenCodeIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("target").unwrap(),
            DriftPolicy::SaveChanges,
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(active.join("AGENTS.md")).unwrap(),
            "drifted"
        );
        assert_eq!(
            fs::read_to_string(active.join("skills").join("newskill").join("SKILL.md")).unwrap(),
            "new skill"
        );
        assert_eq!(
            fs::read_to_string(active.join("commands").join("new.md")).unwrap(),
            "new command"
        );
        let active_config = fixture
            .store
            .load_config(&ProfileName::parse("active").unwrap())
            .unwrap();
        assert_eq!(active_config.model_preference("opencode"), "drift-model");
        assert_eq!(
            active_config.permission_preference("opencode"),
            "drift-perm"
        );
        assert_eq!(active_config.model_preference("codex"), "default");
        assert_symlink_to(
            fixture.opencode_dir().join("AGENTS.md"),
            target.join("AGENTS.md"),
        );
    }

    #[test]
    fn opencode_discard_changes_switches_without_updating_active_profile() {
        let fixture = OpenCodeFixture::new();
        let active = fixture.profile("active");
        let target = fixture.profile("target");
        fs::write(active.join("AGENTS.md"), "original").unwrap();
        fs::write(
            fixture.home.join("state.json"),
            r#"{"active_profiles":{"opencode":"active"}}"#,
        )
        .unwrap();
        fs::create_dir_all(fixture.opencode_dir()).unwrap();
        fs::write(fixture.opencode_dir().join("AGENTS.md"), "drifted").unwrap();

        use_profile(
            &OpenCodeIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("target").unwrap(),
            DriftPolicy::Discard,
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(active.join("AGENTS.md")).unwrap(),
            "original"
        );
        assert_symlink_to(
            fixture.opencode_dir().join("AGENTS.md"),
            target.join("AGENTS.md"),
        );
    }

    struct OpenCodeFixture {
        temp: tempfile::TempDir,
        home: PathBuf,
        env: RuntimeEnv,
        store: ProfileStore,
    }

    impl OpenCodeFixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("lazyagents");
            let user_home = temp.path().join("user");
            let bin = temp.path().join("bin");
            fs::create_dir_all(&bin).unwrap();
            fs::write(bin.join("opencode"), "").unwrap();
            let env = RuntimeEnv {
                lazyagents_home: home.clone(),
                user_home,
                path_entries: vec![bin],
            };
            let store = ProfileStore::new(LazyagentsHome::from_path(&home));
            Self {
                temp,
                home,
                env,
                store,
            }
        }

        fn profile(&self, name: &str) -> PathBuf {
            let name = ProfileName::parse(name).unwrap();
            self.store.create_skeleton(&name).unwrap()
        }

        fn opencode_dir(&self) -> PathBuf {
            self.env.user_home.join(".config").join("opencode")
        }
    }

    fn add_skill(profile: &Path, name: &str) {
        let path = profile.join("skills").join(name);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("SKILL.md"), "").unwrap();
    }

    fn add_command(profile: &Path, name: &str) {
        fs::write(profile.join("commands").join(name), "").unwrap();
    }

    fn write_config(profile: &Path, text: &str) {
        fs::write(profile.join("config.json"), text).unwrap();
    }

    fn assert_symlink_to(link: impl AsRef<Path>, source: impl AsRef<Path>) {
        assert_eq!(fs::read_link(link).unwrap(), source.as_ref());
    }
}
