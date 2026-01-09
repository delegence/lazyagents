use crate::cli::prompt::{select_list, select_one, ListItem, PromptConfig};
use crate::cli::ProfileCommand;
use crate::core::ConfigFile;
use crate::core::{self, AgentScope, ProfileDraft, ProfilePatch};
use crate::error::{Error, Result};
use crate::harness;

pub fn handle(command: Option<ProfileCommand>) -> Result<()> {
    let mut config = ConfigFile::load_or_create()?;
    match command {
        None => list_profiles(&config),
        Some(ProfileCommand::New { id, agents }) => {
            let mut draft = ProfileDraft::minimal(id.clone());
            let selected_agents = if agents.is_empty() {
                prompt_agent_selection(&config)?
            } else {
                validate_agents(&config, &agents)?;
                agents
            };
            draft.agents = selected_agents;
            core::create_profile(&mut config, draft)?;
            config.save()?;
            println!("created profile '{}'", id);
            Ok(())
        }
        Some(ProfileCommand::Rename { id, new_id }) => {
            core::rename_profile(&mut config, &id, &new_id)?;
            config.save()?;
            println!("renamed profile '{}' to '{}'", id, new_id);
            Ok(())
        }
        Some(ProfileCommand::Edit {
            id,
            agents,
            skills,
            commands,
            mcps,
        }) => {
            if agents.is_empty() && skills.is_empty() && commands.is_empty() && mcps.is_empty() {
                return Err(Error::InvalidInput(
                    "provide at least one of --agent, --skill, --command, or --mcp".to_string(),
                ));
            }
            let mut patch = ProfilePatch::default();
            if !agents.is_empty() {
                patch.agents = Some(agents);
            }
            if !skills.is_empty() {
                patch.skills = Some(skills);
            }
            if !commands.is_empty() {
                patch.commands = Some(commands);
            }
            if !mcps.is_empty() {
                patch.mcps = Some(mcps);
            }
            core::update_profile(&mut config, &id, patch)?;
            config.save()?;
            println!("updated profile '{}'", id);
            Ok(())
        }
        Some(ProfileCommand::Switch { id, agent }) => {
            let profile = config
                .profiles
                .iter()
                .find(|profile| profile.id == id)
                .cloned()
                .ok_or_else(|| Error::NotFound(format!("profile '{}' not found", id)))?;

            if profile.agents.is_empty() {
                return Err(Error::InvalidInput(format!(
                    "profile '{}' has no agents",
                    id
                )));
            }

            let chosen_agent = match agent.clone() {
                Some(agent_id) => Some(agent_id),
                None if profile.agents.len() > 1 => Some(prompt_agent_choice(&profile.agents)?),
                None => None,
            };

            let scope = match chosen_agent.clone() {
                Some(agent_id) => AgentScope::OnlyAgent(agent_id),
                None => AgentScope::AllAgentsInProfile,
            };

            let report = core::switch_profile(&mut config, &id, scope)?;

            let agent_ids: Vec<String> = match chosen_agent {
                Some(agent_id) => vec![agent_id],
                None => profile.agents.clone(),
            };

            for agent_id in &agent_ids {
                if config.agents.iter().any(|agent| agent.id == *agent_id) {
                    harness::apply_profile_for_agent(&config, &profile, agent_id)?;
                }
            }

            config.save()?;

            let agents_label = if agent_ids.is_empty() {
                "no agents".to_string()
            } else if agent_ids.len() == 1 {
                agent_ids[0].clone()
            } else {
                agent_ids.join(", ")
            };

            if report.warnings.is_empty() {
                println!("switched to '{}' for {}", id, agents_label);
            } else {
                println!("switched to '{}' for {}", id, agents_label);
                for warning in report.warnings {
                    eprintln!("warning: {}", warning);
                }
            }
            Ok(())
        }
        Some(ProfileCommand::Rm { id }) => {
            core::remove_profile(&mut config, &id)?;
            config.save()?;
            println!("removed profile '{}'", id);
            Ok(())
        }
    }
}

fn list_profiles(config: &ConfigFile) -> Result<()> {
    if config.profiles.is_empty() {
        println!("no profiles");
        return Ok(());
    }

    for profile in &config.profiles {
        let agents = if profile.agents.is_empty() {
            "no agents".to_string()
        } else {
            profile.agents.join(", ")
        };
        println!("- {} ({})", profile.id, agents);
    }
    Ok(())
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

fn prompt_agent_selection(config: &ConfigFile) -> Result<Vec<String>> {
    if config.agents.is_empty() {
        return Err(Error::InvalidInput("no agents configured".to_string()));
    }

    let items: Vec<ListItem> = config
        .agents
        .iter()
        .map(|agent| ListItem {
            id: agent.id.clone(),
            label: agent.id.clone(),
        })
        .collect();

    let mut config_ui = PromptConfig::new(
        "Select agents:",
        "Enter numbers to toggle (e.g. 1). 'all' selects all: ",
        "no agents configured",
    );
    config_ui.default_select_all = false;
    config_ui.empty_selection_message = Some("no agents selected for profile".to_string());

    let selected = select_list(config_ui, &items)?;
    Ok(selected.into_iter().collect())
}

fn prompt_agent_choice(agent_ids: &[String]) -> Result<String> {
    let items: Vec<ListItem> = agent_ids
        .iter()
        .map(|agent| ListItem {
            id: agent.clone(),
            label: agent.clone(),
        })
        .collect();

    let mut config_ui = PromptConfig::new(
        "Select agent to switch:",
        "Enter agent number: ",
        "no agents available",
    );
    config_ui.actions_hint = None;
    config_ui.empty_selection_message = Some("no agent selected".to_string());

    select_one(config_ui, &items)
}

#[cfg(test)]
mod tests {
    use super::handle;
    use crate::cli::test_setup::{EnvGuard, TEST_LOCK};
    use crate::cli::ProfileCommand;
    use crate::core::{ConfigFile, Profile};

    #[test]
    fn cli_switch_profile_updates_active_profiles() {
        let _guard = TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let _env = EnvGuard::new(dir.path());

        let mut config = ConfigFile::minimal();
        config.profiles.push(Profile {
            id: "work".to_string(),
            agents: vec!["codex".to_string()],
            skills: Vec::new(),
            commands: Vec::new(),
            mcps: Vec::new(),
            models: std::collections::BTreeMap::new(),
            extra: std::collections::BTreeMap::new(),
        });
        config.save().unwrap();

        handle(Some(ProfileCommand::Switch {
            id: "work".to_string(),
            agent: None,
        }))
        .unwrap();

        let config = ConfigFile::load_or_create().unwrap();
        assert_eq!(
            config.active_profiles.get("codex"),
            Some(&"work".to_string())
        );
    }
}
