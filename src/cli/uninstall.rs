use std::collections::BTreeSet;

use crate::cli::prompt::{select_tree, PromptConfig, TreeItem, TreeSection};
use crate::core::ConfigFile;
use crate::error::{Error, Result};
use crate::install;

pub fn handle(
    agents: &[String],
    profiles: &[String],
    skills: &[String],
    commands: &[String],
    mcps: &[String],
) -> Result<()> {
    if skills.is_empty() && commands.is_empty() && mcps.is_empty() {
        return Err(Error::InvalidInput(
            "provide at least one --skill, --command, or --mcp".to_string(),
        ));
    }

    let mut config = ConfigFile::load_or_create()?;
    let target_profiles =
        resolve_target_profiles(&config, agents, profiles, skills, commands, mcps)?;

    let report = install::uninstall_from_profiles_in_config(
        &mut config,
        &target_profiles,
        skills,
        commands,
        mcps,
        agents,
    )?;
    config.save()?;

    println!("removed {} skills", report.skills.len());
    println!("removed {} commands", report.commands.len());
    println!("removed {} MCPs", report.mcps.len());
    for warning in report.warnings {
        eprintln!("warning: {}", warning);
    }
    Ok(())
}

fn resolve_target_profiles(
    config: &ConfigFile,
    agents: &[String],
    profiles: &[String],
    skills: &[String],
    commands: &[String],
    mcps: &[String],
) -> Result<Vec<String>> {
    if !profiles.is_empty() {
        validate_agents(config, agents)?;
        let mut resolved = Vec::new();
        for profile_id in profiles {
            let profile = config
                .profiles
                .iter()
                .find(|profile| profile.id == *profile_id)
                .ok_or_else(|| {
                    Error::InvalidInput(format!("profile '{}' not found", profile_id))
                })?;
            if !agents.is_empty()
                && !agents
                    .iter()
                    .all(|agent| profile.agents.iter().any(|id| id == agent))
            {
                return Err(Error::InvalidInput(format!(
                    "profile '{}' does not include all selected agents",
                    profile_id
                )));
            }
            ensure_profile_contains(profile, skills, commands, mcps)?;
            if !resolved.iter().any(|entry| entry == profile_id) {
                resolved.push(profile_id.clone());
            }
        }
        return Ok(resolved);
    }

    let agent_ids = if agents.is_empty() {
        config.agents.iter().map(|agent| agent.id.clone()).collect()
    } else {
        validate_agents(config, agents)?;
        agents.to_vec()
    };

    let selection = prompt_profile_selection(config, &agent_ids, skills, commands, mcps)?;
    if selection.is_empty() {
        return Err(Error::InvalidInput(
            "no profiles selected for uninstall".to_string(),
        ));
    }
    Ok(selection.into_iter().collect())
}

fn validate_agents(config: &ConfigFile, agents: &[String]) -> Result<()> {
    for agent in agents {
        if !config.agents.iter().any(|entry| entry.id == *agent) {
            return Err(Error::InvalidInput(format!(
                "agent '{}' is not configured",
                agent
            )));
        }
    }
    Ok(())
}

fn ensure_profile_contains(
    profile: &crate::core::Profile,
    skills: &[String],
    commands: &[String],
    mcps: &[String],
) -> Result<()> {
    for skill in skills {
        if !profile.skills.iter().any(|entry| entry == skill) {
            return Err(Error::InvalidInput(format!(
                "profile '{}' does not include skill '{}'",
                profile.id, skill
            )));
        }
    }
    for command in commands {
        if !profile.commands.iter().any(|entry| entry == command) {
            return Err(Error::InvalidInput(format!(
                "profile '{}' does not include command '{}'",
                profile.id, command
            )));
        }
    }
    for mcp in mcps {
        if !profile.mcps.iter().any(|entry| entry == mcp) {
            return Err(Error::InvalidInput(format!(
                "profile '{}' does not include MCP '{}'",
                profile.id, mcp
            )));
        }
    }
    Ok(())
}

fn prompt_profile_selection(
    config: &ConfigFile,
    agent_ids: &[String],
    skills: &[String],
    commands: &[String],
    mcps: &[String],
) -> Result<BTreeSet<String>> {
    let mut sections = Vec::new();
    for agent_id in agent_ids {
        let mut items = Vec::new();
        for profile in &config.profiles {
            if !profile.agents.iter().any(|agent| agent == agent_id) {
                continue;
            }
            if ensure_profile_contains(profile, skills, commands, mcps).is_err() {
                continue;
            }
            let active = config
                .active_profiles
                .get(agent_id)
                .map(|active| active == &profile.id)
                .unwrap_or(false);
            let label = if active {
                format!("{} (active)", profile.id)
            } else {
                profile.id.clone()
            };
            items.push(TreeItem {
                id: profile.id.clone(),
                label,
            });
        }
        sections.push(TreeSection {
            label: agent_id.clone(),
            items,
        });
    }

    let mut config_ui = PromptConfig::new(
        "Select profiles to uninstall from:",
        "Enter numbers to toggle (e.g. 1, 1.2). 'all' selects all: ",
        "no profiles contain the selected components",
    );
    config_ui.empty_children_label = "(no profiles)".to_string();
    config_ui.empty_selection_message = Some("no profiles selected for uninstall".to_string());

    select_tree(config_ui, &sections)
}

#[cfg(test)]
mod tests {
    use super::handle;
    use crate::cli::test_setup::{EnvGuard, TEST_LOCK};
    use crate::core::{ConfigFile, Profile};

    #[test]
    fn cli_uninstall_removes_profile_items() {
        let _guard = TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let _env = EnvGuard::new(dir.path());

        let mut config = ConfigFile::minimal();
        config
            .active_profiles
            .insert("codex".to_string(), "work".to_string());
        config.profiles.push(Profile {
            id: "work".to_string(),
            agents: vec!["codex".to_string()],
            skills: vec!["alpha".to_string()],
            commands: vec!["build".to_string()],
            mcps: vec!["local".to_string()],
            models: std::collections::BTreeMap::new(),
            extra: std::collections::BTreeMap::new(),
        });
        config.save().unwrap();

        handle(
            &[],
            &vec!["work".to_string()],
            &vec!["alpha".to_string()],
            &vec!["build".to_string()],
            &vec!["local".to_string()],
        )
        .unwrap();

        let config = ConfigFile::load_or_create().unwrap();
        let profile = &config.profiles[0];
        assert!(profile.skills.is_empty());
        assert!(profile.commands.is_empty());
        assert!(profile.mcps.is_empty());
    }
}
