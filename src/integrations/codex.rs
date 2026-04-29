use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use toml_edit::{value, Array, DocumentMut, Item, Table};

use crate::harness::drift::{DriftItem, DriftReport};
use crate::harness::integration::{
    Detection, HarnessIntegration, HarnessPaths, ImportedDirectory, ImportedFile, LoadedProfile,
    PreferenceImport, ProfileImport, RuntimeEnv,
};
use crate::harness::kind::HarnessKind;
use crate::harness::managed::{write_text_atomic, ManagedSurface};
use crate::profile::mcp::{read_mcp_definitions, McpDefinition, McpTransport};
use crate::profile::ProfileConfig;

pub struct CodexIntegration;

impl HarnessIntegration for CodexIntegration {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Codex
    }

    fn detect(&self, env: &RuntimeEnv) -> Result<Detection> {
        Ok(detect_binary(env, self.kind().binary_name()))
    }

    fn paths(&self, env: &RuntimeEnv) -> Result<HarnessPaths> {
        let config_dir = env.user_home.join(".codex");
        Ok(HarnessPaths {
            instruction_target: config_dir.join("AGENTS.md"),
            skills_dir: config_dir.join("skills"),
            commands_dir: config_dir.join("prompts"),
            settings_file: config_dir.join("config.toml"),
            mcp_file: config_dir.join("config.toml"),
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

    fn preflight(&self, profile: &LoadedProfile) -> Result<()> {
        flat_profile_commands(&profile.path).map(|_| ())
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
        collect_directory_link_drift(
            "commands",
            flat_profile_commands(&active.path)?,
            &paths.commands_dir,
            &mut items,
        )?;
        let native_mcps = import_codex_mcps(&read_config(&paths.settings_file)?)?;
        let profile_mcps = fs::read_to_string(active.path.join("mcps.json")).unwrap_or_default();
        if normalize_json_text(&native_mcps) != normalize_json_text(&profile_mcps) {
            items.push(DriftItem {
                surface: "mcp".to_string(),
                detail: "Codex MCP list differs from active profile".to_string(),
            });
        }
        Ok(DriftReport { items })
    }

    fn import_from_harness(&self, paths: &HarnessPaths) -> Result<ProfileImport> {
        let document = read_config(&paths.settings_file)?;
        Ok(ProfileImport {
            instruction: read_optional_string(&paths.instruction_target)?,
            skills: import_skills(&paths.skills_dir)?,
            commands: import_commands(&paths.commands_dir)?,
            mcp_definitions: Some(import_codex_mcps(&document)?),
            model_preference: PreferenceImport::new(
                document["model"]
                    .as_str()
                    .map(|value| json!(value))
                    .unwrap_or_else(|| json!("default")),
            ),
            permission_preference: PreferenceImport::new(
                document["approval_policy"]
                    .as_str()
                    .map(|value| json!(value))
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
        patch_codex_config(profile, paths)?;
        Ok(())
    }

    fn verify(&self, profile: &LoadedProfile, paths: &HarnessPaths) -> Result<()> {
        let instruction_source = profile.path.join("AGENTS.md");
        if !symlink_points_to(&paths.instruction_target, &instruction_source) {
            anyhow::bail!(
                "Codex instruction target {} does not point to {}",
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
                anyhow::bail!("Codex skill link {} was not applied", target.display());
            }
        }

        for command in flat_profile_commands(&profile.path)? {
            let target =
                paths.commands_dir.join(command.file_name().ok_or_else(|| {
                    anyhow::anyhow!("invalid command path {}", command.display())
                })?);
            if !symlink_points_to(&target, &command) {
                anyhow::bail!("Codex command link {} was not applied", target.display());
            }
        }

        let _ = read_config(&paths.settings_file)?;
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
    for command in flat_profile_commands(&profile.path)? {
        let target = paths.commands_dir.join(
            command
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("invalid command path {}", command.display()))?,
        );
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
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        if entry.path().metadata()?.is_dir() {
            if contains_markdown_file(&entry.path())? {
                anyhow::bail!(
                    "Codex command import does not support nested commands: {}",
                    entry.path().display()
                );
            }
            continue;
        }
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "md")
        {
            commands.push(ImportedFile {
                relative_path: PathBuf::from(entry.file_name()),
                contents: fs::read(entry.path())
                    .with_context(|| format!("failed to read {}", entry.path().display()))?,
            });
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

fn flat_profile_commands(profile_path: &Path) -> Result<Vec<PathBuf>> {
    let commands_dir = profile_path.join("commands");
    let mut commands = Vec::new();
    if !commands_dir.exists() {
        return Ok(commands);
    }
    for entry in fs::read_dir(&commands_dir)
        .with_context(|| format!("failed to read {}", commands_dir.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let has_markdown = contains_markdown_file(&entry.path())?;
            if has_markdown {
                anyhow::bail!(
                    "Codex does not support nested profile commands: {}",
                    entry.path().display()
                );
            }
            continue;
        }
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "md")
        {
            commands.push(entry.path());
        }
    }
    commands.sort();
    Ok(commands)
}

fn contains_markdown_file(path: &Path) -> Result<bool> {
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if contains_markdown_file(&entry.path())? {
                return Ok(true);
            }
        } else if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "md")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn patch_codex_config(profile: &LoadedProfile, paths: &HarnessPaths) -> Result<()> {
    let profile_config = read_profile_config(&profile.path)?;
    let mcp_definitions = read_mcp_definitions(&profile.path)?;
    let mut document = read_config(&paths.settings_file)?;

    if let Some(model) =
        non_default_string(profile_config.model_preference("codex"), "Model Preference")?
    {
        document["model"] = value(model);
    }
    if let Some(permission) = non_default_string(
        profile_config.permission_preference("codex"),
        "Permission Preference",
    )? {
        document["approval_policy"] = value(permission);
    }

    document.as_table_mut().remove("mcp_servers");
    if !mcp_definitions.is_empty() {
        let mut servers = Table::new();
        for definition in mcp_definitions
            .into_iter()
            .filter(|definition| definition.enabled)
        {
            servers[&definition.name] = Item::Table(definition.to_codex_table()?);
        }
        document["mcp_servers"] = Item::Table(servers);
    }

    write_text_atomic(&paths.settings_file, &document.to_string())
        .with_context(|| format!("failed to write {}", paths.settings_file.display()))
}

fn read_profile_config(profile_path: &Path) -> Result<ProfileConfig> {
    let path = profile_path.join("config.json");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("missing or unreadable profile config at {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("invalid profile config at {}", path.display()))
}

fn read_config(path: &Path) -> Result<DocumentMut> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()))
        }
    };
    text.parse::<DocumentMut>()
        .with_context(|| format!("invalid Codex config TOML at {}", path.display()))
}

fn import_codex_mcps(document: &DocumentMut) -> Result<String> {
    let mut servers = Vec::new();
    let Some(mcp_item) = document.as_table().get("mcp_servers") else {
        return Ok("".to_string());
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

    if servers.is_empty() {
        Ok("".to_string())
    } else {
        Ok(format!("{}\n", serde_json::to_string_pretty(&servers)?))
    }
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

fn non_default_string(value: Value, label: &str) -> Result<Option<String>> {
    match value {
        Value::String(value) if value == "default" => Ok(None),
        Value::String(value) => Ok(Some(value)),
        other => anyhow::bail!("Codex {label} must be a string or \"default\", got {other}"),
    }
}

impl McpDefinition {
    fn to_codex_table(&self) -> Result<Table> {
        let mut table = Table::new();
        table["enabled"] = value(true);
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
    fn codex_use_applies_profile_artifacts_preferences_mcp_and_state() {
        let fixture = CodexFixture::new();
        let profile = fixture.profile("work");
        add_skill(&profile, "writer");
        add_command(&profile, "plan.md");
        write_config(
            &profile,
            r#"{
  "name": "work",
  "description": "",
  "models": {"codex": "gpt-5.2"},
  "permissions": {"codex": "on-request"}
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
        fs::create_dir_all(fixture.codex_dir()).unwrap();
        fs::write(fixture.codex_dir().join("config.toml"), "other = true\n").unwrap();

        use_profile(
            &CodexIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("work").unwrap(),
            DriftPolicy::Discard,
        )
        .unwrap();

        assert_symlink_to(
            fixture.codex_dir().join("AGENTS.md"),
            profile.join("AGENTS.md"),
        );
        assert_symlink_to(
            fixture.codex_dir().join("skills").join("writer"),
            profile.join("skills").join("writer"),
        );
        assert_symlink_to(
            fixture.codex_dir().join("prompts").join("plan.md"),
            profile.join("commands").join("plan.md"),
        );
        let config = fs::read_to_string(fixture.codex_dir().join("config.toml")).unwrap();
        assert!(config.contains("other = true"));
        assert!(config.contains("model = \"gpt-5.2\""));
        assert!(config.contains("approval_policy = \"on-request\""));
        assert!(config.contains("[mcp_servers.local]"));
        assert!(config.contains("command = \"server\""));
        assert!(config.contains("[mcp_servers.remote.env_http_headers]"));
        assert!(!config.contains("disabled"));
        assert_eq!(
            fs::read_to_string(fixture.home.join("state.json")).unwrap(),
            "{\n  \"active_profiles\": {\n    \"codex\": \"work\"\n  }\n}\n"
        );
    }

    #[test]
    fn codex_use_normalizes_missing_optional_artifacts() {
        let fixture = CodexFixture::new();
        let profile = fixture.profile("work");
        fs::remove_file(profile.join("AGENTS.md")).unwrap();
        fs::remove_file(profile.join("mcps.json")).unwrap();
        fs::remove_dir_all(profile.join("skills")).unwrap();
        fs::remove_dir_all(profile.join("commands")).unwrap();

        use_profile(
            &CodexIntegration,
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
            fixture.codex_dir().join("AGENTS.md"),
            profile.join("AGENTS.md"),
        );
    }

    #[test]
    fn codex_use_removes_stale_surfaces_and_clears_mcp_list() {
        let fixture = CodexFixture::new();
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
            &CodexIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("full").unwrap(),
            DriftPolicy::Discard,
        )
        .unwrap();
        use_profile(
            &CodexIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("empty").unwrap(),
            DriftPolicy::Discard,
        )
        .unwrap();

        assert!(fs::read_dir(fixture.codex_dir().join("skills"))
            .unwrap()
            .next()
            .is_none());
        assert!(fs::read_dir(fixture.codex_dir().join("prompts"))
            .unwrap()
            .next()
            .is_none());
        let config = fs::read_to_string(fixture.codex_dir().join("config.toml")).unwrap();
        assert!(!config.contains("mcp_servers"));
    }

    #[test]
    fn codex_use_default_preferences_do_not_modify_existing_native_settings() {
        let fixture = CodexFixture::new();
        fixture.profile("work");
        fs::create_dir_all(fixture.codex_dir()).unwrap();
        fs::write(
            fixture.codex_dir().join("config.toml"),
            "model = \"existing\"\napproval_policy = \"never\"\n",
        )
        .unwrap();

        use_profile(
            &CodexIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("work").unwrap(),
            DriftPolicy::Discard,
        )
        .unwrap();

        let config = fs::read_to_string(fixture.codex_dir().join("config.toml")).unwrap();
        assert!(config.contains("model = \"existing\""));
        assert!(config.contains("approval_policy = \"never\""));
    }

    #[test]
    fn codex_use_rejects_nested_commands_before_partial_writes() {
        let fixture = CodexFixture::new();
        let profile = fixture.profile("work");
        fs::create_dir_all(profile.join("commands").join("nested")).unwrap();
        fs::write(profile.join("commands").join("nested").join("bad.md"), "").unwrap();

        let error = use_profile(
            &CodexIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("work").unwrap(),
            DriftPolicy::Discard,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("Codex does not support nested profile commands"));
        assert!(!fixture.codex_dir().join("AGENTS.md").exists());
        assert!(!fixture.home.join("state.json").exists());
    }

    #[test]
    fn codex_use_rejects_invalid_disabled_mcp_without_state_update() {
        let fixture = CodexFixture::new();
        let profile = fixture.profile("work");
        fs::write(
            profile.join("mcps.json"),
            r#"[{"name":"draft","enabled":false}]"#,
        )
        .unwrap();

        let error = use_profile(
            &CodexIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("work").unwrap(),
            DriftPolicy::Discard,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("MCP draft requires transport"));
        assert!(!fixture.home.join("state.json").exists());
        assert!(!fixture.codex_dir().join("config.toml").exists());
    }

    #[test]
    fn codex_use_rolls_back_and_dereferences_symlink_backup_on_failure() {
        let fixture = CodexFixture::new();
        let profile = fixture.profile("work");
        fs::write(
            profile.join("mcps.json"),
            r#"[{"name":"bad","transport":"stdio"}]"#,
        )
        .unwrap();
        fs::create_dir_all(fixture.codex_dir()).unwrap();
        let old_source = fixture.temp.path().join("old-source.md");
        fs::write(&old_source, "previous instructions").unwrap();
        symlink_file(&old_source, fixture.codex_dir().join("AGENTS.md")).unwrap();
        fs::create_dir_all(fixture.codex_dir().join("skills")).unwrap();
        fs::write(fixture.codex_dir().join("skills").join("old.txt"), "old").unwrap();
        fs::write(fixture.codex_dir().join("config.toml"), "model = \"old\"\n").unwrap();

        let error = use_profile(
            &CodexIntegration,
            &fixture.env,
            &fixture.store,
            &ProfileName::parse("work").unwrap(),
            DriftPolicy::Discard,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("requires command"));
        assert_eq!(
            fs::read_to_string(fixture.codex_dir().join("AGENTS.md")).unwrap(),
            "previous instructions"
        );
        assert!(!fs::symlink_metadata(fixture.codex_dir().join("AGENTS.md"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_to_string(fixture.codex_dir().join("skills").join("old.txt")).unwrap(),
            "old"
        );
        assert_eq!(
            fs::read_to_string(fixture.codex_dir().join("config.toml")).unwrap(),
            "model = \"old\"\n"
        );
        assert!(!fixture.home.join("state.json").exists());
    }

    #[test]
    fn codex_import_reads_managed_state_and_dereferences_symlinks() {
        let fixture = CodexFixture::new();
        fs::create_dir_all(fixture.codex_dir().join("skills")).unwrap();
        fs::create_dir_all(fixture.codex_dir().join("prompts")).unwrap();
        let instruction_source = fixture.temp.path().join("instruction-source.md");
        fs::write(&instruction_source, "imported instructions").unwrap();
        symlink_file(&instruction_source, fixture.codex_dir().join("AGENTS.md")).unwrap();
        let skill_source = fixture.temp.path().join("skill-source");
        fs::create_dir_all(&skill_source).unwrap();
        fs::write(skill_source.join("SKILL.md"), "skill body").unwrap();
        symlink_dir(
            &skill_source,
            fixture.codex_dir().join("skills").join("linked"),
        )
        .unwrap();
        fs::write(
            fixture.codex_dir().join("prompts").join("cmd.md"),
            "command",
        )
        .unwrap();
        fs::write(
            fixture.codex_dir().join("config.toml"),
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
        let paths = CodexIntegration.paths(&fixture.env).unwrap();

        let imported = CodexIntegration.import_from_harness(&paths).unwrap();

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
        assert!(imported
            .mcp_definitions
            .unwrap()
            .contains("\"Authorization\": \"$TOKEN\""));
    }

    #[test]
    fn codex_import_fails_on_malformed_native_config() {
        let fixture = CodexFixture::new();
        fs::create_dir_all(fixture.codex_dir()).unwrap();
        fs::write(fixture.codex_dir().join("config.toml"), "not = [").unwrap();
        let paths = CodexIntegration.paths(&fixture.env).unwrap();

        let error = CodexIntegration.import_from_harness(&paths).unwrap_err();

        assert!(error.to_string().contains("invalid Codex config TOML"));
    }

    #[test]
    fn codex_save_changes_imports_drift_into_active_profile_before_switching() {
        let fixture = CodexFixture::new();
        let active = fixture.profile("active");
        let target = fixture.profile("target");
        fs::write(
            fixture.home.join("state.json"),
            r#"{"active_profiles":{"codex":"active"}}"#,
        )
        .unwrap();
        fs::create_dir_all(fixture.codex_dir().join("skills").join("newskill")).unwrap();
        fs::write(
            fixture
                .codex_dir()
                .join("skills")
                .join("newskill")
                .join("SKILL.md"),
            "new skill",
        )
        .unwrap();
        fs::create_dir_all(fixture.codex_dir().join("prompts")).unwrap();
        fs::write(
            fixture.codex_dir().join("prompts").join("new.md"),
            "new command",
        )
        .unwrap();
        fs::write(fixture.codex_dir().join("AGENTS.md"), "drifted").unwrap();
        fs::write(
            fixture.codex_dir().join("config.toml"),
            "model = \"drift-model\"\napproval_policy = \"drift-perm\"\n",
        )
        .unwrap();

        use_profile(
            &CodexIntegration,
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
        assert_eq!(active_config.model_preference("codex"), "drift-model");
        assert_eq!(active_config.permission_preference("codex"), "drift-perm");
        assert_eq!(active_config.model_preference("claude"), "default");
        assert_symlink_to(
            fixture.codex_dir().join("AGENTS.md"),
            target.join("AGENTS.md"),
        );
    }

    #[test]
    fn codex_discard_changes_switches_without_updating_active_profile() {
        let fixture = CodexFixture::new();
        let active = fixture.profile("active");
        let target = fixture.profile("target");
        fs::write(active.join("AGENTS.md"), "original").unwrap();
        fs::write(
            fixture.home.join("state.json"),
            r#"{"active_profiles":{"codex":"active"}}"#,
        )
        .unwrap();
        fs::create_dir_all(fixture.codex_dir()).unwrap();
        fs::write(fixture.codex_dir().join("AGENTS.md"), "drifted").unwrap();

        use_profile(
            &CodexIntegration,
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
            fixture.codex_dir().join("AGENTS.md"),
            target.join("AGENTS.md"),
        );
    }

    struct CodexFixture {
        temp: tempfile::TempDir,
        home: PathBuf,
        env: RuntimeEnv,
        store: ProfileStore,
    }

    impl CodexFixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("lazyagents");
            let user_home = temp.path().join("user");
            let bin = temp.path().join("bin");
            fs::create_dir_all(&bin).unwrap();
            fs::write(bin.join("codex"), "").unwrap();
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

        fn codex_dir(&self) -> PathBuf {
            self.env.user_home.join(".codex")
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
