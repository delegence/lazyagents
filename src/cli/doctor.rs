use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use crate::core::{self, utils, ConfigFile, McpServer, Profile};
use crate::error::{Error, Result};

pub fn run() -> Result<()> {
    run_internal(true)
}

pub fn sync() -> Result<()> {
    run_internal(false)
}

fn run_internal(show_summary: bool) -> Result<()> {
    let mut config = ConfigFile::load_or_create()?;
    let mut issues: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for agent in &mut config.agents {
        let installed = match run_step(
            show_summary,
            &format!("detecting {} installation", agent.id),
            || detect_agent_installation(&agent.id, &agent.global_config_dir),
        ) {
            Ok(value) => value,
            Err(err) => {
                record_issue(&mut issues, &agent.id, err);
                false
            }
        };
        agent.installed = installed;
        issues.entry(agent.id.clone()).or_default();

        if !installed {
            continue;
        }

        let base_dir = utils::expand_home(&agent.global_config_dir)?;
        run_check(
            &mut issues,
            &agent.id,
            &format!("checking {}", base_dir.display()),
            show_summary,
            || ensure_writable_dir(&base_dir),
        );

        let config_path = base_dir.join(&agent.config_file);
        run_check(
            &mut issues,
            &agent.id,
            &format!("validating {}", config_path.display()),
            show_summary,
            || validate_config_file(&agent.id, &config_path),
        );

        let skills_dir = base_dir.join(&agent.skills_dir);
        run_check(
            &mut issues,
            &agent.id,
            &format!("checking {}", skills_dir.display()),
            show_summary,
            || ensure_writable_dir(&skills_dir),
        );

        let commands_dir = base_dir.join(&agent.commands_dir);
        run_check(
            &mut issues,
            &agent.id,
            &format!("checking {}", commands_dir.display()),
            show_summary,
            || ensure_writable_dir(&commands_dir),
        );

        let rules_path = base_dir.join(&agent.rules_file);
        run_check(
            &mut issues,
            &agent.id,
            &format!("checking {}", rules_path.display()),
            show_summary,
            || ensure_writable_file(&rules_path),
        );
    }

    run_step(show_summary, "validating profile references", || {
        let profile_issues = collect_profile_issues(&config);
        for (agent_id, entries) in profile_issues {
            issues.entry(agent_id).or_default().extend(entries);
        }
        Ok(())
    })?;

    run_step(show_summary, "ensuring default profiles", || {
        ensure_default_profiles(&mut config)
    })?;

    sync_active_profiles(&mut config, show_summary, &mut issues)?;
    config.save()?;
    if show_summary {
        render_summary(&config, &issues);
    }
    Ok(())
}

fn render_summary(config: &ConfigFile, issues: &BTreeMap<String, Vec<String>>) {
    println!("Doctor summary");
    for agent in &config.agents {
        let display = agent_display_name(&agent.id);
        if !agent.installed {
            println!("[x] {}: not installed", display);
            continue;
        }

        let agent_profiles: Vec<&Profile> = config
            .profiles
            .iter()
            .filter(|profile| profile.agents.iter().any(|id| id == &agent.id))
            .collect();
        let active = config
            .active_profiles
            .get(&agent.id)
            .cloned()
            .unwrap_or_else(|| "(none)".to_string());
        let profile_count = agent_profiles.len();
        let profile_label = if profile_count == 1 {
            "profile"
        } else {
            "profiles"
        };
        let profile_summary = format!("{} {} ({})", profile_count, profile_label, active);

        let agent_issues = issues
            .get(&agent.id)
            .map(|entries| entries.iter().filter(|entry| !entry.is_empty()).count())
            .unwrap_or(0);

        if agent_issues == 0 {
            println!("[✓] {}: {}", display, profile_summary);
        } else {
            let details = issues
                .get(&agent.id)
                .map(|entries| entries.join("; "))
                .unwrap_or_else(|| "unknown issues".to_string());
            println!("[!] {}: found issues ({})", display, details);
        }
    }
}

fn record_issue(issues: &mut BTreeMap<String, Vec<String>>, agent_id: &str, err: Error) {
    issues
        .entry(agent_id.to_string())
        .or_default()
        .push(err.to_string());
}

fn run_step<T>(show_summary: bool, label: &str, action: impl FnOnce() -> Result<T>) -> Result<T> {
    if show_summary {
        step(label, action)
    } else {
        action()
    }
}

fn run_check(
    issues: &mut BTreeMap<String, Vec<String>>,
    agent_id: &str,
    label: &str,
    show_summary: bool,
    check: impl FnOnce() -> Result<()>,
) {
    let result = if show_summary {
        step(label, check)
    } else {
        check()
    };
    if let Err(err) = result {
        record_issue(issues, agent_id, err);
    }
}

fn detect_agent_installation(agent_id: &str, global_dir: &str) -> Result<bool> {
    if which_exists(agent_id) {
        return Ok(true);
    }

    let dir = utils::expand_home(global_dir)?;
    Ok(dir.exists())
}

fn which_exists(binary: &str) -> bool {
    Command::new("which")
        .arg(binary)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn validate_config_file(agent_id: &str, path: &Path) -> Result<()> {
    match agent_id {
        "codex" => {
            let raw = read_or_create_file(path, default_config_contents(agent_id))?;
            let mut doc = raw
                .parse::<toml_edit::DocumentMut>()
                .map_err(|err| Error::InvalidInput(format!("{}: {}", path.display(), err)))?;
            if !doc.contains_key("mcp_servers") {
                doc["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            fs::write(path, format!("{}\n", doc.to_string()))
                .map_err(|err| Error::io(path, err))?;
        }
        "claude" => {
            let raw = read_or_create_file(path, default_config_contents(agent_id))?;
            let mut value = serde_json::from_str::<serde_json::Value>(&raw)
                .map_err(|err| Error::InvalidInput(format!("{}: {}", path.display(), err)))?;
            ensure_json_object(&mut value, "mcpServers")?;
            write_json(path, &value)?;
        }
        "opencode" => {
            let raw = read_or_create_file(path, default_config_contents(agent_id))?;
            let stripped = core::jsonc::strip_jsonc(&raw);
            let mut value = serde_json::from_str::<serde_json::Value>(&stripped)
                .map_err(|err| Error::InvalidInput(format!("{}: {}", path.display(), err)))?;
            ensure_json_object(&mut value, "mcp")?;
            write_json(path, &value)?;
        }
        _ => {}
    }

    Ok(())
}

fn ensure_json_object(value: &mut serde_json::Value, key: &str) -> Result<()> {
    let obj = value
        .as_object_mut()
        .ok_or_else(|| Error::InvalidInput("expected JSON object at root".to_string()))?;
    let entry = obj
        .entry(key.to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if !entry.is_object() {
        *entry = serde_json::Value::Object(serde_json::Map::new());
    }
    Ok(())
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    let contents =
        serde_json::to_string_pretty(value).map_err(|err| Error::serde_json(path, err))?;
    fs::write(path, format!("{}\n", contents)).map_err(|err| Error::io(path, err))?;
    Ok(())
}

fn ensure_writable_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir_all(path).map_err(|err| Error::io(path, err))?;
    }

    let file_path = path.join(".mews-write-check");
    fs::write(&file_path, "check").map_err(|err| Error::io(&file_path, err))?;
    fs::remove_file(&file_path).map_err(|err| Error::io(&file_path, err))?;
    Ok(())
}

fn ensure_writable_file(path: &Path) -> Result<()> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| Error::io(parent, err))?;
        }
        fs::write(path, "").map_err(|err| Error::io(path, err))?;
        return Ok(());
    }
    let file_path = path.with_extension("mews-check");
    fs::write(&file_path, "check").map_err(|err| Error::io(&file_path, err))?;
    fs::remove_file(&file_path).map_err(|err| Error::io(&file_path, err))?;
    Ok(())
}

fn collect_profile_issues(config: &ConfigFile) -> BTreeMap<String, Vec<String>> {
    let agent_ids: BTreeSet<&str> = config
        .agents
        .iter()
        .map(|agent| agent.id.as_str())
        .collect();
    let skills: BTreeSet<&str> = config
        .skills
        .iter()
        .map(|skill| skill.name.as_str())
        .collect();
    let commands: BTreeSet<&str> = config
        .commands
        .iter()
        .map(|cmd| cmd.name.as_str())
        .collect();
    let mcps: BTreeSet<&str> = config.mcp_servers.keys().map(String::as_str).collect();

    let mut issues: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for profile in &config.profiles {
        let profile_agents: Vec<&str> = profile
            .agents
            .iter()
            .map(|agent| agent.as_str())
            .filter(|agent| agent_ids.contains(agent))
            .collect();
        if profile_agents.is_empty() {
            continue;
        }

        for skill in &profile.skills {
            if !skills.contains(skill.as_str()) {
                for agent in &profile_agents {
                    issues
                        .entry((*agent).to_string())
                        .or_default()
                        .push(format!(
                            "profile '{}' missing skill '{}'",
                            profile.id, skill
                        ));
                }
            }
        }
        for command in &profile.commands {
            if !commands.contains(command.as_str()) {
                for agent in &profile_agents {
                    issues
                        .entry((*agent).to_string())
                        .or_default()
                        .push(format!(
                            "profile '{}' missing command '{}'",
                            profile.id, command
                        ));
                }
            }
        }
        for mcp in &profile.mcps {
            if !mcps.contains(mcp.as_str()) {
                for agent in &profile_agents {
                    issues
                        .entry((*agent).to_string())
                        .or_default()
                        .push(format!("profile '{}' missing MCP '{}'", profile.id, mcp));
                }
            }
        }
    }

    issues
}

fn sync_active_profiles(
    config: &mut ConfigFile,
    show_summary: bool,
    issues: &mut BTreeMap<String, Vec<String>>,
) -> Result<()> {
    let active_profiles = config.active_profiles.clone();
    let agents = config.agents.clone();

    for (agent_id, profile_id) in active_profiles {
        let Some(agent) = agents.iter().find(|agent| agent.id == agent_id) else {
            continue;
        };
        if !agent.installed {
            continue;
        }
        let profile_index = match config
            .profiles
            .iter()
            .position(|profile| profile.id == profile_id)
        {
            Some(index) => index,
            None => continue,
        };

        let base_dir = match utils::expand_home(&agent.global_config_dir) {
            Ok(dir) => dir,
            Err(err) => {
                record_issue(issues, &agent_id, err);
                continue;
            }
        };

        let exclusive = {
            let profile = &config.profiles[profile_index];
            profile.agents.len() == 1 && profile.agents[0] == agent_id
        };

        run_check(
            issues,
            &agent_id,
            &format!("syncing {}", agent.id),
            show_summary,
            || sync_profile_from_harness(config, agent, &base_dir, profile_index, exclusive),
        );
    }

    Ok(())
}

fn default_config_contents(agent_id: &str) -> String {
    match agent_id {
        "claude" => "{\n  \"mcpServers\": {}\n}\n".to_string(),
        "opencode" => {
            "{\n  \"$schema\": \"https://opencode.ai/config.json\",\n  \"mcp\": {}\n}\n".to_string()
        }
        _ => String::new(),
    }
}

fn read_or_create_file(path: &Path, contents: String) -> Result<String> {
    if path.exists() {
        fs::read_to_string(path).map_err(|err| Error::io(path, err))
    } else {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| Error::io(parent, err))?;
        }
        fs::write(path, contents.as_bytes()).map_err(|err| Error::io(path, err))?;
        fs::read_to_string(path).map_err(|err| Error::io(path, err))
    }
}

fn ensure_default_profiles(config: &mut ConfigFile) -> Result<()> {
    if !config.profiles.is_empty() {
        return Ok(());
    }

    core::ensure_cache_dirs()?;

    let agents = config.agents.clone();
    for agent in &agents {
        if !agent.installed {
            continue;
        }
        let base_dir = utils::expand_home(&agent.global_config_dir)?;
        let profile_id = format!("current-{}", agent.id);

        let rules_dir = core::rules_profile_dir(&profile_id)?;
        fs::create_dir_all(&rules_dir).map_err(|err| Error::io(&rules_dir, err))?;
        let rules_file = rules_dir.join("AGENTS.md");
        let agent_rules = base_dir.join(&agent.rules_file);
        if agent_rules.exists() {
            let contents =
                fs::read_to_string(&agent_rules).map_err(|err| Error::io(&agent_rules, err))?;
            fs::write(&rules_file, contents).map_err(|err| Error::io(&rules_file, err))?;
        } else {
            fs::write(&rules_file, "").map_err(|err| Error::io(&rules_file, err))?;
        }

        let settings_dir = core::settings_profile_dir(&profile_id)?;
        fs::create_dir_all(&settings_dir).map_err(|err| Error::io(&settings_dir, err))?;
        let config_file = settings_dir.join(&agent.config_file);
        if base_dir.join(&agent.config_file).exists() {
            let contents = fs::read_to_string(base_dir.join(&agent.config_file))
                .map_err(|err| Error::io(&base_dir, err))?;
            fs::write(&config_file, contents).map_err(|err| Error::io(&config_file, err))?;
        } else {
            fs::write(&config_file, default_config_contents(&agent.id))
                .map_err(|err| Error::io(&config_file, err))?;
        }

        let mut profile = core::Profile {
            id: profile_id.clone(),
            agents: vec![agent.id.clone()],
            skills: Vec::new(),
            commands: Vec::new(),
            mcps: Vec::new(),
            models: std::collections::BTreeMap::new(),
            extra: std::collections::BTreeMap::new(),
        };

        capture_agent_components(config, agent, &base_dir, &mut profile)?;
        capture_agent_mcps(config, agent, &base_dir, &mut profile)?;

        config.profiles.push(profile);
        config
            .active_profiles
            .entry(agent.id.clone())
            .or_insert(profile_id);
    }

    Ok(())
}

fn sync_profile_from_harness(
    config: &mut ConfigFile,
    agent: &core::AgentConfig,
    base_dir: &Path,
    profile_index: usize,
    exclusive: bool,
) -> Result<()> {
    let mut profile = config.profiles[profile_index].clone();
    let profile_id = profile.id.clone();

    sync_rules_cache(&profile_id, agent, base_dir)?;
    sync_settings_cache(&profile_id, agent, base_dir)?;

    if exclusive {
        profile.skills.clear();
        profile.commands.clear();
        profile.mcps.clear();
    }

    capture_agent_components(config, agent, base_dir, &mut profile)?;
    capture_agent_mcps(config, agent, base_dir, &mut profile)?;
    update_model_from_config(&mut profile, agent, base_dir)?;

    config.profiles[profile_index] = profile;

    Ok(())
}

fn sync_rules_cache(profile_id: &str, agent: &core::AgentConfig, base_dir: &Path) -> Result<()> {
    let rules_dir = core::rules_profile_dir(profile_id)?;
    fs::create_dir_all(&rules_dir).map_err(|err| Error::io(&rules_dir, err))?;
    let cache_file = rules_dir.join("AGENTS.md");
    let agent_rules = base_dir.join(&agent.rules_file);
    if agent_rules.exists() {
        let contents =
            fs::read_to_string(&agent_rules).map_err(|err| Error::io(&agent_rules, err))?;
        fs::write(&cache_file, contents).map_err(|err| Error::io(&cache_file, err))?;
    } else {
        fs::write(&cache_file, "").map_err(|err| Error::io(&cache_file, err))?;
    }
    Ok(())
}

fn sync_settings_cache(profile_id: &str, agent: &core::AgentConfig, base_dir: &Path) -> Result<()> {
    let settings_dir = core::settings_profile_dir(profile_id)?;
    fs::create_dir_all(&settings_dir).map_err(|err| Error::io(&settings_dir, err))?;
    let cache_file = settings_dir.join(&agent.config_file);
    let agent_config = base_dir.join(&agent.config_file);
    if agent_config.exists() {
        let contents =
            fs::read_to_string(&agent_config).map_err(|err| Error::io(&agent_config, err))?;
        fs::write(&cache_file, contents).map_err(|err| Error::io(&cache_file, err))?;
    } else {
        fs::write(&cache_file, default_config_contents(&agent.id))
            .map_err(|err| Error::io(&cache_file, err))?;
    }
    Ok(())
}

fn update_model_from_config(
    profile: &mut Profile,
    agent: &core::AgentConfig,
    base_dir: &Path,
) -> Result<()> {
    let config_path = base_dir.join(&agent.config_file);
    let model = match agent.id.as_str() {
        "codex" => extract_model_from_toml(&config_path)?,
        "claude" => extract_model_from_json(&config_path)?,
        "opencode" => extract_model_from_jsonc(&config_path)?,
        _ => None,
    };

    if let Some(model) = model {
        profile.models.insert(agent.id.clone(), model);
    } else {
        profile.models.remove(&agent.id);
    }

    Ok(())
}

fn extract_model_from_toml(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|err| Error::io(path, err))?;
    let doc = raw
        .parse::<toml_edit::DocumentMut>()
        .map_err(|err| Error::InvalidInput(format!("{}: {}", path.display(), err)))?;
    Ok(doc
        .get("model")
        .and_then(|item| item.as_str())
        .map(|val| val.to_string()))
}

fn extract_model_from_json(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|err| Error::io(path, err))?;
    let value = serde_json::from_str::<serde_json::Value>(&raw)
        .map_err(|err| Error::InvalidInput(format!("{}: {}", path.display(), err)))?;
    Ok(value
        .get("model")
        .and_then(|entry| entry.as_str())
        .map(|val| val.to_string()))
}

fn extract_model_from_jsonc(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|err| Error::io(path, err))?;
    let stripped = core::jsonc::strip_jsonc(&raw);
    let value = serde_json::from_str::<serde_json::Value>(&stripped)
        .map_err(|err| Error::InvalidInput(format!("{}: {}", path.display(), err)))?;
    if let Some(model) = value.get("model").and_then(|entry| entry.as_str()) {
        return Ok(Some(model.to_string()));
    }
    if let Some(agent_obj) = value.get("agent").and_then(|entry| entry.as_object()) {
        if let Some(general) = agent_obj.get("general").and_then(|entry| entry.as_object()) {
            if let Some(model) = general.get("model").and_then(|entry| entry.as_str()) {
                return Ok(Some(model.to_string()));
            }
        }
    }
    Ok(None)
}

fn capture_agent_components(
    config: &mut ConfigFile,
    agent: &core::AgentConfig,
    base_dir: &Path,
    profile: &mut core::Profile,
) -> Result<()> {
    let skills_dir = base_dir.join(&agent.skills_dir);
    if skills_dir.exists() {
        for entry in fs::read_dir(&skills_dir).map_err(|err| Error::io(&skills_dir, err))? {
            let entry = entry.map_err(|err| Error::io(&skills_dir, err))?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let source = path.join("SKILL.md");
            if !source.exists() {
                continue;
            }
            let dest_dir = core::skills_dir()?.join(name);
            if !dest_dir.exists() {
                fs::create_dir_all(&dest_dir).map_err(|err| Error::io(&dest_dir, err))?;
                let contents =
                    fs::read_to_string(&source).map_err(|err| Error::io(&source, err))?;
                fs::write(dest_dir.join("SKILL.md"), contents)
                    .map_err(|err| Error::io(&dest_dir, err))?;
            }
            upsert_catalog_entry(&mut config.skills, name, None);
            if !profile.skills.iter().any(|entry| entry == name) {
                profile.skills.push(name.to_string());
            }
        }
    }

    let commands_dir = base_dir.join(&agent.commands_dir);
    if commands_dir.exists() {
        for entry in fs::read_dir(&commands_dir).map_err(|err| Error::io(&commands_dir, err))? {
            let entry = entry.map_err(|err| Error::io(&commands_dir, err))?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            let dest = core::commands_dir()?.join(format!("{}.md", stem));
            if !dest.exists() {
                let contents = fs::read_to_string(&path).map_err(|err| Error::io(&path, err))?;
                fs::write(&dest, contents).map_err(|err| Error::io(&dest, err))?;
            }
            upsert_catalog_entry(&mut config.commands, stem, None);
            if !profile.commands.iter().any(|entry| entry == stem) {
                profile.commands.push(stem.to_string());
            }
        }
    }

    Ok(())
}

fn capture_agent_mcps(
    config: &mut ConfigFile,
    agent: &core::AgentConfig,
    base_dir: &Path,
    profile: &mut core::Profile,
) -> Result<()> {
    let config_path = base_dir.join(&agent.config_file);
    if !config_path.exists() {
        return Ok(());
    }

    let servers = match agent.id.as_str() {
        "codex" => parse_mcp_servers_toml(&config_path)?,
        "claude" => parse_mcp_servers_json(&config_path, "mcpServers")?,
        "opencode" => parse_mcp_servers_jsonc(&config_path, "mcp")?,
        _ => BTreeSet::new(),
    };

    for name in servers {
        if config.mcp_servers.get(&name).is_some() {
            if !profile.mcps.iter().any(|entry| entry == &name) {
                profile.mcps.push(name);
            }
            continue;
        }
        config
            .mcp_servers
            .insert(name.clone(), McpServer::default());
        if !profile.mcps.iter().any(|entry| entry == &name) {
            profile.mcps.push(name);
        }
    }

    Ok(())
}

fn parse_mcp_servers_json(path: &Path, key: &str) -> Result<BTreeSet<String>> {
    let raw = fs::read_to_string(path).map_err(|err| Error::io(path, err))?;
    let value = serde_json::from_str::<serde_json::Value>(&raw)
        .map_err(|err| Error::InvalidInput(format!("{}: {}", path.display(), err)))?;
    Ok(extract_mcp_names(&value, key))
}

fn parse_mcp_servers_jsonc(path: &Path, key: &str) -> Result<BTreeSet<String>> {
    let raw = fs::read_to_string(path).map_err(|err| Error::io(path, err))?;
    let stripped = core::jsonc::strip_jsonc(&raw);
    let value = serde_json::from_str::<serde_json::Value>(&stripped)
        .map_err(|err| Error::InvalidInput(format!("{}: {}", path.display(), err)))?;
    Ok(extract_mcp_names(&value, key))
}

fn extract_mcp_names(value: &serde_json::Value, key: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let Some(obj) = value.as_object() else {
        return names;
    };
    let Some(mcp) = obj.get(key).and_then(|entry| entry.as_object()) else {
        return names;
    };
    for name in mcp.keys() {
        names.insert(name.clone());
    }
    names
}

fn parse_mcp_servers_toml(path: &Path) -> Result<BTreeSet<String>> {
    let raw = fs::read_to_string(path).map_err(|err| Error::io(path, err))?;
    let doc = raw
        .parse::<toml_edit::DocumentMut>()
        .map_err(|err| Error::InvalidInput(format!("{}: {}", path.display(), err)))?;
    let mut names = BTreeSet::new();
    if let Some(table) = doc.get("mcp_servers").and_then(|item| item.as_table()) {
        for (name, _) in table.iter() {
            names.insert(name.to_string());
        }
    }
    Ok(names)
}

fn upsert_catalog_entry(
    entries: &mut Vec<crate::core::CatalogEntry>,
    name: &str,
    source: Option<String>,
) {
    if let Some(entry) = entries.iter_mut().find(|entry| entry.name == name) {
        if entry.source.is_none() {
            entry.source = source;
        }
        return;
    }

    entries.push(crate::core::CatalogEntry {
        name: name.to_string(),
        source,
        extra: std::collections::BTreeMap::new(),
    });
}

fn step<T>(label: &str, action: impl FnOnce() -> Result<T>) -> Result<T> {
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let mut index = 0;
    print!("{} {}", spinner[index], label);
    io::stdout()
        .flush()
        .map_err(|err| Error::io("stdout", err))?;
    for _ in 0..3 {
        index = (index + 1) % spinner.len();
        print!("\r{} {}", spinner[index], label);
        io::stdout()
            .flush()
            .map_err(|err| Error::io("stdout", err))?;
        sleep(Duration::from_millis(40));
    }
    let result = action();
    let clear = " ".repeat(label.len() + 4);
    print!("\r{}\r", clear);
    io::stdout()
        .flush()
        .map_err(|err| Error::io("stdout", err))?;
    result
}

fn agent_display_name(id: &str) -> &str {
    match id {
        "codex" => "Codex",
        "claude" => "Claude",
        "opencode" => "Opencode",
        _ => id,
    }
}

#[cfg(test)]
mod tests {
    use super::run;
    use crate::cli::test_setup::{EnvGuard, TEST_LOCK};
    use crate::core::{utils, ConfigFile};
    use tempfile::tempdir;

    #[test]
    fn doctor_creates_missing_configs_and_profiles() {
        let _guard = TEST_LOCK.lock().unwrap();
        let dir = tempdir().expect("tempdir");
        let _env = EnvGuard::new(dir.path());

        let codex_dir = utils::expand_home("~/.codex").expect("codex dir");
        std::fs::create_dir_all(&codex_dir).expect("codex dir");
        let claude_dir = utils::expand_home("~/.claude").expect("claude dir");
        std::fs::create_dir_all(&claude_dir).expect("claude dir");
        let opencode_dir = utils::expand_home("~/.config/opencode").expect("opencode dir");
        std::fs::create_dir_all(&opencode_dir).expect("opencode dir");

        run().expect("doctor run");

        assert!(codex_dir.join("config.toml").exists());

        let claude_config = claude_dir.join("settings.local.json");
        assert!(claude_config.exists());
        let claude_contents = std::fs::read_to_string(&claude_config).expect("read claude config");
        assert!(claude_contents.contains("\"mcpServers\""));

        let opencode_config = opencode_dir.join("opencode.jsonc");
        assert!(opencode_config.exists());
        let opencode_contents =
            std::fs::read_to_string(&opencode_config).expect("read opencode config");
        assert!(opencode_contents.contains("\"$schema\""));

        let config = ConfigFile::load_or_create().expect("config");
        assert!(config.profiles.iter().any(|p| p.id == "current-codex"));
        assert!(config.profiles.iter().any(|p| p.id == "current-claude"));
        assert!(config.profiles.iter().any(|p| p.id == "current-opencode"));
    }
}
