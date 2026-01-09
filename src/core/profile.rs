use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use crate::core::{
    rules_profile_dir, settings_profile_dir, utils, CatalogEntry, ConfigFile, Profile,
};
use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct ProfileDraft {
    pub id: String,
    pub agents: Vec<String>,
    pub skills: Vec<String>,
    pub commands: Vec<String>,
    pub mcps: Vec<String>,
    pub models: BTreeMap<String, String>,
}

impl ProfileDraft {
    pub fn minimal(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            agents: Vec::new(),
            skills: Vec::new(),
            commands: Vec::new(),
            mcps: Vec::new(),
            models: BTreeMap::new(),
        }
    }

    pub fn into_profile(self) -> Profile {
        Profile {
            id: self.id,
            agents: self.agents,
            skills: self.skills,
            commands: self.commands,
            mcps: self.mcps,
            models: self.models,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProfilePatch {
    pub agents: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
    pub commands: Option<Vec<String>>,
    pub mcps: Option<Vec<String>>,
    pub models: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone)]
pub enum AgentScope {
    AllAgentsInProfile,
    OnlyAgent(String),
}

#[derive(Debug, Clone, Default)]
pub struct SwitchReport {
    pub warnings: Vec<String>,
}

pub fn list_profiles<'a>(config: &'a ConfigFile) -> &'a [Profile] {
    &config.profiles
}

pub fn get_profile<'a>(config: &'a ConfigFile, id: &str) -> Option<&'a Profile> {
    config.profiles.iter().find(|profile| profile.id == id)
}

pub fn create_profile(config: &mut ConfigFile, draft: ProfileDraft) -> Result<()> {
    if config.profiles.iter().any(|profile| profile.id == draft.id) {
        return Err(Error::InvalidInput(format!(
            "profile '{}' already exists",
            draft.id
        )));
    }

    let profile = draft.into_profile();
    validate_profile(config, &profile)?;
    config.profiles.push(profile);
    Ok(())
}

pub fn update_profile(config: &mut ConfigFile, id: &str, patch: ProfilePatch) -> Result<()> {
    let index = config
        .profiles
        .iter()
        .position(|profile| profile.id == id)
        .ok_or_else(|| Error::NotFound(format!("profile '{}' not found", id)))?;

    let mut updated = config.profiles[index].clone();

    if let Some(agents) = patch.agents {
        updated.agents = agents;
    }
    if let Some(skills) = patch.skills {
        updated.skills = skills;
    }
    if let Some(commands) = patch.commands {
        updated.commands = commands;
    }
    if let Some(mcps) = patch.mcps {
        updated.mcps = mcps;
    }
    if let Some(models) = patch.models {
        updated.models = models;
    }

    validate_profile(config, &updated)?;
    config.profiles[index] = updated;
    Ok(())
}

pub fn remove_profile(config: &mut ConfigFile, id: &str) -> Result<()> {
    let index = config
        .profiles
        .iter()
        .position(|profile| profile.id == id)
        .ok_or_else(|| Error::NotFound(format!("profile '{}' not found", id)))?;

    config.profiles.remove(index);
    config.active_profiles.retain(|_, active| active != id);
    Ok(())
}

pub fn rename_profile(config: &mut ConfigFile, id: &str, new_id: &str) -> Result<()> {
    if id == new_id {
        return Ok(());
    }
    if config.profiles.iter().any(|profile| profile.id == new_id) {
        return Err(Error::InvalidInput(format!(
            "profile '{}' already exists",
            new_id
        )));
    }

    let index = config
        .profiles
        .iter()
        .position(|profile| profile.id == id)
        .ok_or_else(|| Error::NotFound(format!("profile '{}' not found", id)))?;

    rename_cache_dir(&rules_profile_dir(id)?, &rules_profile_dir(new_id)?)?;
    rename_cache_dir(&settings_profile_dir(id)?, &settings_profile_dir(new_id)?)?;

    config.profiles[index].id = new_id.to_string();
    for active in config.active_profiles.values_mut() {
        if active == id {
            *active = new_id.to_string();
        }
    }
    Ok(())
}

fn rename_cache_dir(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    if !utils::exists(from) {
        return Ok(());
    }
    if utils::exists(to) {
        return Err(Error::InvalidInput(format!(
            "cache directory already exists at {}",
            to.display()
        )));
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|err| Error::io(parent, err))?;
    }
    fs::rename(from, to).map_err(|err| Error::io(from, err))?;
    Ok(())
}

pub fn switch_profile(
    config: &mut ConfigFile,
    profile_id: &str,
    scope: AgentScope,
) -> Result<SwitchReport> {
    let profile = config
        .profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| Error::NotFound(format!("profile '{}' not found", profile_id)))?;

    let mut report = SwitchReport::default();
    let agent_ids = collect_agent_ids(config);

    match scope {
        AgentScope::AllAgentsInProfile => {
            if profile.agents.is_empty() {
                return Err(Error::InvalidInput(format!(
                    "profile '{}' has no agents assigned",
                    profile.id
                )));
            }

            for agent_id in &profile.agents {
                if !agent_ids.contains(agent_id) {
                    report.warnings.push(format!(
                        "profile '{}' references unknown agent '{}'",
                        profile.id, agent_id
                    ));
                    continue;
                }
                config
                    .active_profiles
                    .insert(agent_id.clone(), profile.id.clone());
            }
        }
        AgentScope::OnlyAgent(agent_id) => {
            if !agent_ids.contains(&agent_id) {
                return Err(Error::InvalidInput(format!(
                    "agent '{}' is not configured",
                    agent_id
                )));
            }
            if !profile.agents.contains(&agent_id) {
                return Err(Error::InvalidInput(format!(
                    "profile '{}' does not include agent '{}'",
                    profile.id, agent_id
                )));
            }
            config.active_profiles.insert(agent_id, profile.id.clone());
        }
    }

    let warnings = validate_references(config, profile);
    report.warnings.extend(warnings);

    Ok(report)
}

fn validate_profile(config: &ConfigFile, profile: &Profile) -> Result<()> {
    if profile.id.trim().is_empty() {
        return Err(Error::InvalidInput(
            "profile id cannot be empty".to_string(),
        ));
    }
    let agent_ids = collect_agent_ids(config);
    for agent in &profile.agents {
        if !agent_ids.contains(agent) {
            return Err(Error::InvalidInput(format!(
                "profile '{}' references unknown agent '{}'",
                profile.id, agent
            )));
        }
    }

    Ok(())
}

fn validate_references(config: &ConfigFile, profile: &Profile) -> Vec<String> {
    let mut warnings = Vec::new();

    let skills = collect_catalog(&config.skills);
    for skill in &profile.skills {
        if !skills.contains(skill.as_str()) {
            warnings.push(format!(
                "profile '{}' references missing skill '{}'",
                profile.id, skill
            ));
        }
    }

    let commands = collect_catalog(&config.commands);
    for command in &profile.commands {
        if !commands.contains(command.as_str()) {
            warnings.push(format!(
                "profile '{}' references missing command '{}'",
                profile.id, command
            ));
        }
    }

    let mcps: BTreeSet<&str> = config.mcp_servers.keys().map(String::as_str).collect();
    for mcp in &profile.mcps {
        if !mcps.contains(mcp.as_str()) {
            warnings.push(format!(
                "profile '{}' references missing MCP '{}'",
                profile.id, mcp
            ));
        }
    }

    warnings
}

fn collect_agent_ids(config: &ConfigFile) -> BTreeSet<String> {
    config.agents.iter().map(|agent| agent.id.clone()).collect()
}

fn collect_catalog(entries: &[CatalogEntry]) -> BTreeSet<&str> {
    entries.iter().map(|entry| entry.name.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ConfigFile;

    #[test]
    fn create_and_switch_profile() {
        let mut config = ConfigFile::minimal();
        let mut draft = ProfileDraft::minimal("work");
        draft.agents.push("codex".to_string());
        create_profile(&mut config, draft).unwrap();

        let report = switch_profile(&mut config, "work", AgentScope::AllAgentsInProfile).unwrap();
        assert!(report.warnings.is_empty());
        assert_eq!(
            config.active_profiles.get("codex"),
            Some(&"work".to_string())
        );
    }

    #[test]
    fn update_profile_keeps_validation() {
        let mut config = ConfigFile::minimal();
        let mut draft = ProfileDraft::minimal("work");
        draft.agents.push("codex".to_string());
        create_profile(&mut config, draft).unwrap();

        let mut patch = ProfilePatch::default();
        patch.agents = Some(vec!["claude".to_string()]);
        update_profile(&mut config, "work", patch).unwrap();

        let profile = get_profile(&config, "work").unwrap();
        assert_eq!(profile.agents, vec!["claude".to_string()]);
    }

    #[test]
    fn remove_profile_clears_active() {
        let mut config = ConfigFile::minimal();
        let mut draft = ProfileDraft::minimal("work");
        draft.agents.push("codex".to_string());
        create_profile(&mut config, draft).unwrap();
        switch_profile(&mut config, "work", AgentScope::AllAgentsInProfile).unwrap();

        remove_profile(&mut config, "work").unwrap();
        assert!(config.active_profiles.is_empty());
    }
}
