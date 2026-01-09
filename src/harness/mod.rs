mod claude;
mod codex;
mod opencode;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::core::{self, utils, AgentConfig, CatalogEntry, ConfigFile, McpServer, Profile};
use crate::error::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct ApplyReport {
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AgentPaths {
    pub base_dir: PathBuf,
    pub rules_file: PathBuf,
    pub skills_dir: PathBuf,
    pub commands_dir: PathBuf,
    pub config_file: PathBuf,
}

pub fn apply_profile_for_agent(
    config: &ConfigFile,
    profile: &Profile,
    agent_id: &str,
) -> Result<ApplyReport> {
    if !profile.agents.iter().any(|agent| agent == agent_id) {
        return Err(Error::InvalidInput(format!(
            "profile '{}' does not include agent '{}'",
            profile.id, agent_id
        )));
    }

    let agent = config
        .agents
        .iter()
        .find(|agent| agent.id == agent_id)
        .ok_or_else(|| Error::InvalidInput(format!("agent '{}' is not configured", agent_id)))?;

    validate_catalog(config, profile)?;
    let paths = resolve_agent_paths(agent)?;

    let mut report = ApplyReport::default();
    backup_agent_state(agent_id, &paths, &mut report)?;

    core::ensure_cache_dirs()?;
    let rules_cache = ensure_rules_cache(&profile.id, &paths.rules_file)?;
    let settings_cache = ensure_settings_cache(
        &profile.id,
        agent_id,
        &agent.config_file,
        &paths.config_file,
    )?;

    utils::clear_dir_contents(&paths.skills_dir)?;
    utils::clear_dir_contents(&paths.commands_dir)?;
    utils::remove_file_if_exists(&paths.rules_file)?;
    utils::remove_file_if_exists(&paths.config_file)?;

    write_skills_for_agent(agent_id, profile, &paths)?;
    write_commands(profile, &paths)?;
    write_rules(&paths.rules_file, &rules_cache)?;
    write_settings(&paths.config_file, &settings_cache)?;

    let mcp_servers = collect_mcp_servers(config, profile, &mut report);
    let model = profile.models.get(agent_id).cloned();

    match agent_id {
        "claude" => claude::apply(profile, &paths, &mcp_servers, model.as_deref(), &mut report)?,
        "codex" => codex::apply(profile, &paths, &mcp_servers, model.as_deref(), &mut report)?,
        "opencode" => {
            opencode::apply(profile, &paths, &mcp_servers, model.as_deref(), &mut report)?
        }
        _ => {
            return Err(Error::InvalidInput(format!(
                "agent '{}' is not supported",
                agent_id
            )))
        }
    }

    Ok(report)
}

fn resolve_agent_paths(agent: &AgentConfig) -> Result<AgentPaths> {
    let base_dir = utils::expand_home(&agent.global_config_dir)?;
    let rules_file = base_dir.join(&agent.rules_file);
    let skills_dir = base_dir.join(&agent.skills_dir);
    let commands_dir = base_dir.join(&agent.commands_dir);
    let config_file = base_dir.join(&agent.config_file);

    Ok(AgentPaths {
        base_dir,
        rules_file,
        skills_dir,
        commands_dir,
        config_file,
    })
}

fn validate_catalog(config: &ConfigFile, profile: &Profile) -> Result<()> {
    let skill_names = collect_catalog(&config.skills);
    for skill in &profile.skills {
        if !skill_names.contains(skill.as_str()) {
            return Err(Error::InvalidInput(format!(
                "profile '{}' references unknown skill '{}'",
                profile.id, skill
            )));
        }
    }

    let command_names = collect_catalog(&config.commands);
    for command in &profile.commands {
        if !command_names.contains(command.as_str()) {
            return Err(Error::InvalidInput(format!(
                "profile '{}' references unknown command '{}'",
                profile.id, command
            )));
        }
    }

    Ok(())
}

fn collect_catalog(entries: &[CatalogEntry]) -> BTreeSet<&str> {
    entries.iter().map(|entry| entry.name.as_str()).collect()
}

fn collect_mcp_servers(
    config: &ConfigFile,
    profile: &Profile,
    report: &mut ApplyReport,
) -> BTreeMap<String, McpServer> {
    let mut selected = BTreeMap::new();

    for name in &profile.mcps {
        if let Some(server) = config.mcp_servers.get(name) {
            selected.insert(name.clone(), server.clone());
        } else {
            report.warnings.push(format!(
                "profile '{}' references missing MCP '{}'",
                profile.id, name
            ));
        }
    }

    selected
}

fn write_skills_for_agent(agent_id: &str, profile: &Profile, paths: &AgentPaths) -> Result<()> {
    let ensure_frontmatter = agent_id == "opencode";
    let cache_dir = core::skills_dir()?;

    for skill in &profile.skills {
        let source_dir = cache_dir.join(skill);
        let source_file = source_dir.join("SKILL.md");
        if !utils::exists(&source_file) {
            return Err(Error::NotFound(format!(
                "missing skill '{}' in cache",
                skill
            )));
        }
        let target_dir = paths.skills_dir.join(slugify(skill));
        utils::ensure_dir(&target_dir)?;
        let content = utils::read_to_string(&source_file)?;
        let content = if ensure_frontmatter {
            ensure_frontmatter_with_name(&content, skill)
        } else {
            content
        };
        let target_file = target_dir.join("SKILL.md");
        utils::write_string(&target_file, &content)?;
    }

    Ok(())
}

fn write_commands(profile: &Profile, paths: &AgentPaths) -> Result<()> {
    let cache_dir = core::commands_dir()?;

    for command in &profile.commands {
        let source_file = cache_dir.join(format!("{}.md", command));
        if !utils::exists(&source_file) {
            return Err(Error::NotFound(format!(
                "missing command '{}' in cache",
                command
            )));
        }
        let content = utils::read_to_string(&source_file)?;
        let target_file = paths.commands_dir.join(format!("{}.md", slugify(command)));
        utils::write_string(&target_file, &content)?;
    }

    Ok(())
}

fn write_rules(target_file: &Path, cache_file: &Path) -> Result<()> {
    let contents = utils::read_to_string(cache_file)?;
    utils::write_string(target_file, &contents)
}

fn write_settings(target_file: &Path, cache_file: &Path) -> Result<()> {
    let contents = utils::read_to_string(cache_file)?;
    utils::write_string(target_file, &contents)
}

fn backup_agent_state(agent_id: &str, paths: &AgentPaths, report: &mut ApplyReport) -> Result<()> {
    let backup_dir = core::backups_dir()?.join(agent_id);
    utils::clear_dir_contents(&backup_dir)?;

    if utils::exists(&paths.rules_file) {
        let target = backup_dir.join(
            paths
                .rules_file
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("rules")),
        );
        utils::copy_file(&paths.rules_file, &target)?;
    } else {
        report
            .warnings
            .push(format!("missing rules file {}", paths.rules_file.display()));
    }

    if utils::exists(&paths.config_file) {
        let target = backup_dir.join(
            paths
                .config_file
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("config")),
        );
        utils::copy_file(&paths.config_file, &target)?;
    } else {
        report.warnings.push(format!(
            "missing config file {}",
            paths.config_file.display()
        ));
    }

    if utils::exists(&paths.skills_dir) {
        utils::copy_dir_recursive(&paths.skills_dir, &backup_dir.join("skills"))?;
    } else {
        report.warnings.push(format!(
            "missing skills directory {}",
            paths.skills_dir.display()
        ));
    }

    if utils::exists(&paths.commands_dir) {
        utils::copy_dir_recursive(&paths.commands_dir, &backup_dir.join("commands"))?;
    } else {
        report.warnings.push(format!(
            "missing commands directory {}",
            paths.commands_dir.display()
        ));
    }

    Ok(())
}

fn ensure_rules_cache(profile_id: &str, agent_rules_file: &Path) -> Result<PathBuf> {
    let rules_dir = core::rules_profile_dir(profile_id)?;
    utils::ensure_dir(&rules_dir)?;
    let cache_file = rules_dir.join("AGENTS.md");
    if !utils::exists(&cache_file) {
        if utils::exists(agent_rules_file) {
            let contents = utils::read_to_string(agent_rules_file)?;
            utils::write_string(&cache_file, &contents)?;
        } else {
            utils::write_string(&cache_file, "")?;
        }
    }
    Ok(cache_file)
}

fn ensure_settings_cache(
    profile_id: &str,
    agent_id: &str,
    config_file_name: &str,
    agent_config_file: &Path,
) -> Result<PathBuf> {
    let settings_dir = core::settings_profile_dir(profile_id)?;
    utils::ensure_dir(&settings_dir)?;
    let cache_file = settings_dir.join(config_file_name);
    if !utils::exists(&cache_file) {
        if utils::exists(agent_config_file) {
            let contents = utils::read_to_string(agent_config_file)?;
            utils::write_string(&cache_file, &contents)?;
        } else {
            let contents = default_settings_contents(agent_id);
            utils::write_string(&cache_file, &contents)?;
        }
    }
    Ok(cache_file)
}

fn default_settings_contents(agent_id: &str) -> String {
    match agent_id {
        "codex" => String::new(),
        _ => "{}\n".to_string(),
    }
}

fn ensure_frontmatter_with_name(content: &str, name: &str) -> String {
    let mut lines = content.lines();
    let Some(first) = lines.next() else {
        return format!("---\nname: {}\n---\n", name);
    };

    if first.trim() != "---" {
        return format!("---\nname: {}\n---\n{}", name, content);
    }

    let mut frontmatter_lines = Vec::new();
    let mut rest_lines = Vec::new();
    let mut in_frontmatter = true;

    for line in lines {
        if in_frontmatter {
            if line.trim() == "---" {
                in_frontmatter = false;
            } else {
                frontmatter_lines.push(line.to_string());
            }
        } else {
            rest_lines.push(line);
        }
    }

    if in_frontmatter {
        return format!("---\nname: {}\n---\n{}", name, content);
    }

    if !frontmatter_lines
        .iter()
        .any(|line| line.trim_start().starts_with("name:"))
    {
        frontmatter_lines.push(format!("name: {}", name));
    }

    let mut output = String::new();
    output.push_str("---\n");
    if !frontmatter_lines.is_empty() {
        output.push_str(&frontmatter_lines.join("\n"));
        output.push('\n');
    }
    output.push_str("---");
    if !rest_lines.is_empty() {
        output.push('\n');
        output.push_str(&rest_lines.join("\n"));
    }
    output
}

pub(crate) fn slugify(input: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;

    for ch in input.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if ch == '-' || ch == '_' {
            slug.push(ch);
            last_dash = false;
        } else if ch.is_whitespace() {
            if !last_dash && !slug.is_empty() {
                slug.push('-');
                last_dash = true;
            }
        }
    }

    if slug.is_empty() {
        "item".to_string()
    } else {
        slug.trim_matches('-').to_string()
    }
}

pub(crate) fn load_jsonc_or_empty(path: &Path) -> Result<serde_json::Value> {
    if utils::exists(path) {
        let raw = utils::read_to_string(path)?;
        let stripped = core::jsonc::strip_jsonc(&raw);
        let value = serde_json::from_str(&stripped).map_err(|err| Error::serde_json(path, err))?;
        Ok(value)
    } else {
        Ok(serde_json::Value::Object(serde_json::Map::new()))
    }
}

pub(crate) fn write_json_pretty(path: &Path, value: &serde_json::Value) -> Result<()> {
    let contents =
        serde_json::to_string_pretty(value).map_err(|err| Error::serde_json(path, err))?;
    utils::write_string(path, &format!("{}\n", contents))
}

pub(crate) fn mcp_servers_to_json(
    servers: &BTreeMap<String, McpServer>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();

    for (name, server) in servers {
        let mut server_map = serde_json::Map::new();
        if let Some(command) = &server.command {
            server_map.insert(
                "command".to_string(),
                serde_json::Value::String(command.clone()),
            );
        }
        if let Some(args) = &server.args {
            let args_value = args
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect();
            server_map.insert("args".to_string(), serde_json::Value::Array(args_value));
        }
        if let Some(env) = &server.env {
            let env_map = env
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect();
            server_map.insert("env".to_string(), serde_json::Value::Object(env_map));
        }
        if let Some(url) = &server.url {
            server_map.insert("url".to_string(), serde_json::Value::String(url.clone()));
        }
        if let Some(headers) = &server.headers {
            let headers_map = headers
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect();
            server_map.insert(
                "headers".to_string(),
                serde_json::Value::Object(headers_map),
            );
        }
        if let Some(enabled) = server.enabled {
            server_map.insert("enabled".to_string(), serde_json::Value::Bool(enabled));
        }

        map.insert(name.clone(), serde_json::Value::Object(server_map));
    }

    map
}

#[cfg(test)]
mod tests {
    use super::{apply_profile_for_agent, ensure_frontmatter_with_name, slugify};
    use crate::cli::test_setup::{EnvGuard, TEST_LOCK};
    use crate::core::{self, utils, CatalogEntry, ConfigFile, McpServer, Profile};
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("My Skill"), "my-skill");
        assert_eq!(slugify("Already_ok"), "already_ok");
        assert_eq!(slugify("  Weird   Name  "), "weird-name");
    }

    #[test]
    fn ensures_frontmatter_name() {
        let input = "Body";
        let output = ensure_frontmatter_with_name(input, "Alpha");
        assert!(output.starts_with("---\nname: Alpha\n---\n"));

        let input = "---\nname: Beta\n---\nBody";
        let output = ensure_frontmatter_with_name(input, "Alpha");
        assert!(output.contains("\nname: Beta\n"));

        let input = "---\ndescription: Test\n---\nBody";
        let output = ensure_frontmatter_with_name(input, "Alpha");
        assert!(output.contains("\nname: Alpha\n"));
    }

    #[test]
    fn apply_profile_writes_codex_paths_only() {
        let _guard = TEST_LOCK.lock().unwrap();
        let dir = tempdir().expect("tempdir");
        let _env = EnvGuard::new(dir.path());

        let mut config = ConfigFile::minimal();
        config.skills.push(CatalogEntry {
            name: "alpha".to_string(),
            source: None,
            extra: BTreeMap::new(),
        });
        config.commands.push(CatalogEntry {
            name: "build".to_string(),
            source: None,
            extra: BTreeMap::new(),
        });
        let mut server = McpServer::default();
        server.command = Some("run".to_string());
        config.mcp_servers.insert("local".to_string(), server);

        let profile = Profile {
            id: "work".to_string(),
            agents: vec!["codex".to_string()],
            skills: vec!["alpha".to_string()],
            commands: vec!["build".to_string()],
            mcps: vec!["local".to_string()],
            models: BTreeMap::new(),
            extra: BTreeMap::new(),
        };

        let cache_dirs = core::ensure_cache_dirs().expect("cache dirs");
        let skill_dir = cache_dirs.skills_dir.join("alpha");
        utils::ensure_dir(&skill_dir).expect("skill dir");
        utils::write_string(&skill_dir.join("SKILL.md"), "Skill body").expect("skill file");
        utils::write_string(&cache_dirs.commands_dir.join("build.md"), "Command body")
            .expect("command file");

        let codex_base = utils::expand_home("~/.codex").expect("codex base");
        utils::ensure_dir(&codex_base.join("skills")).expect("codex skills dir");
        utils::ensure_dir(&codex_base.join("prompts")).expect("codex commands dir");
        utils::write_string(&codex_base.join("AGENTS.md"), "Rules").expect("rules file");
        utils::write_string(&codex_base.join("config.toml"), "").expect("config file");

        let report = apply_profile_for_agent(&config, &profile, "codex").expect("apply");
        assert!(report.warnings.is_empty());

        assert!(codex_base.join("skills/alpha/SKILL.md").exists());
        assert!(codex_base.join("prompts/build.md").exists());
        let config_contents =
            std::fs::read_to_string(codex_base.join("config.toml")).expect("read config");
        assert!(config_contents.contains("mcp_servers"));

        let claude_base = utils::expand_home("~/.claude").expect("claude base");
        assert!(!claude_base.exists());
    }
}
