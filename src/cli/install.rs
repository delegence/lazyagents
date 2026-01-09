use std::collections::BTreeSet;

use crate::cli::prompt::{
    confirm_yes_no, select_list, select_tree, ListItem, PromptConfig, TreeItem, TreeSection,
};
use crate::core::{self, ConfigFile};
use crate::error::{Error, Result};
use crate::harness;
use crate::install::{self, Discovery};

pub fn handle(source: &str, agents: &[String], profiles: &[String]) -> Result<()> {
    let mut config = ConfigFile::load_or_create()?;
    let selected_profiles = resolve_target_profiles(&config, agents, profiles)?;
    let interactive = profiles.is_empty();

    let mut discovery = install::discover_from_source(source)?;
    if interactive {
        let selection = select_components(&discovery)?;
        discovery = filter_discovery(discovery, &selection);
        discovery = resolve_conflicts(&config, &selected_profiles, discovery)?;
    }

    let report = install::install_from_discovery(&mut config, &selected_profiles, discovery)?;
    config.save()?;

    println!("installed {} skills", report.skills.len());
    println!("installed {} commands", report.commands.len());
    println!("installed {} MCPs", report.mcps.len());
    for warning in report.warnings {
        eprintln!("warning: {}", warning);
    }
    Ok(())
}

fn resolve_target_profiles(
    config: &ConfigFile,
    agents: &[String],
    profiles: &[String],
) -> Result<Vec<String>> {
    if !profiles.is_empty() {
        validate_agents(config, agents)?;
        return validate_profiles(config, profiles);
    }

    let agent_ids = if agents.is_empty() {
        config.agents.iter().map(|agent| agent.id.clone()).collect()
    } else {
        validate_agents(config, agents)?;
        agents.to_vec()
    };

    let selection = prompt_profile_selection(config, &agent_ids)?;
    if selection.is_empty() {
        return Err(Error::InvalidInput(
            "no profiles selected for install".to_string(),
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

fn validate_profiles(config: &ConfigFile, profiles: &[String]) -> Result<Vec<String>> {
    let mut resolved = Vec::new();
    for profile in profiles {
        if config.profiles.iter().any(|entry| entry.id == *profile) {
            if !resolved.iter().any(|entry| entry == profile) {
                resolved.push(profile.clone());
            }
        } else {
            return Err(Error::InvalidInput(format!(
                "profile '{}' not found",
                profile
            )));
        }
    }
    Ok(resolved)
}

fn prompt_profile_selection(config: &ConfigFile, agent_ids: &[String]) -> Result<BTreeSet<String>> {
    let mut sections = Vec::new();
    for agent_id in agent_ids {
        let mut items = Vec::new();
        for profile in &config.profiles {
            if profile.agents.iter().any(|agent| agent == agent_id) {
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
        }
        sections.push(TreeSection {
            label: agent_id.clone(),
            items,
        });
    }

    let mut config_ui = PromptConfig::new(
        "Select target profiles:",
        "Enter numbers to toggle (e.g. 1, 1.2). 'all' selects all: ",
        "no profiles found for selected agents",
    );
    config_ui.empty_children_label = "(no profiles)".to_string();
    config_ui.empty_selection_message = Some("no profiles selected for install".to_string());

    select_tree(config_ui, &sections)
}

struct InstallSelection {
    skills: BTreeSet<String>,
    commands: BTreeSet<String>,
    mcps: BTreeSet<String>,
}

fn select_components(discovery: &Discovery) -> Result<InstallSelection> {
    let skills = discovery
        .skills
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<Vec<_>>();
    let commands = discovery
        .commands
        .iter()
        .map(|command| command.name.clone())
        .collect::<Vec<_>>();
    let mcps = discovery
        .mcps
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();

    if skills.is_empty() && commands.is_empty() && mcps.is_empty() {
        return Err(Error::InvalidInput(
            "no skills, commands, or MCPs found to install".to_string(),
        ));
    }

    let selected_skills = prompt_item_selection("skills", &skills)?;
    let selected_commands = prompt_item_selection("commands", &commands)?;
    let selected_mcps = prompt_item_selection("mcps", &mcps)?;

    if selected_skills.is_empty() && selected_commands.is_empty() && selected_mcps.is_empty() {
        return Err(Error::InvalidInput(
            "no components selected for install".to_string(),
        ));
    }

    Ok(InstallSelection {
        skills: selected_skills,
        commands: selected_commands,
        mcps: selected_mcps,
    })
}

fn prompt_item_selection(label: &str, items: &[String]) -> Result<BTreeSet<String>> {
    if items.is_empty() {
        return Ok(BTreeSet::new());
    }

    let list_items: Vec<ListItem> = items
        .iter()
        .map(|item| ListItem {
            id: item.clone(),
            label: item.clone(),
        })
        .collect();
    let config = PromptConfig::new(
        format!("Select {}:", label),
        "Enter numbers to toggle. 'all' installs all: ",
        format!("no {} found", label),
    );
    select_list(config, &list_items)
}

fn filter_discovery(mut discovery: Discovery, selection: &InstallSelection) -> Discovery {
    discovery.skills = discovery
        .skills
        .into_iter()
        .filter(|skill| selection.skills.contains(&skill.name))
        .collect();
    discovery.commands = discovery
        .commands
        .into_iter()
        .filter(|command| selection.commands.contains(&command.name))
        .collect();
    discovery.mcps = discovery
        .mcps
        .into_iter()
        .filter(|(name, _)| selection.mcps.contains(name))
        .collect();
    discovery
}

fn resolve_conflicts(
    config: &ConfigFile,
    target_profiles: &[String],
    mut discovery: Discovery,
) -> Result<Discovery> {
    let mut skills = Vec::new();
    let mut commands = Vec::new();
    let mut active_agents = Vec::new();
    for (agent_id, profile_id) in &config.active_profiles {
        if target_profiles.iter().any(|id| id == profile_id) {
            if let Some(agent) = config.agents.iter().find(|agent| agent.id == *agent_id) {
                if agent.installed {
                    active_agents.push(agent.clone());
                }
            }
        }
    }

    for skill in discovery.skills {
        if skill_conflicts(&active_agents, &skill.name)?
            && !confirm_overwrite("skill", &skill.name)?
        {
            continue;
        }
        skills.push(skill);
    }

    for command in discovery.commands {
        if command_conflicts(&active_agents, &command.name)?
            && !confirm_overwrite("command", &command.name)?
        {
            continue;
        }
        commands.push(command);
    }

    discovery.skills = skills;
    discovery.commands = commands;
    Ok(discovery)
}

fn confirm_overwrite(kind: &str, name: &str) -> Result<bool> {
    confirm_yes_no(
        &format!(
            "{} '{}' already exists. do you want to overwrite [y/n]?: ",
            kind, name
        ),
        false,
    )
}

fn skill_conflicts(agents: &[core::AgentConfig], name: &str) -> Result<bool> {
    let slug = harness::slugify(name);
    for agent in agents {
        let base_dir = core::utils::expand_home(&agent.global_config_dir)?;
        let skill_dir = base_dir
            .join(&agent.skills_dir)
            .join(&slug)
            .join("SKILL.md");
        if skill_dir.exists() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn command_conflicts(agents: &[core::AgentConfig], name: &str) -> Result<bool> {
    let slug = harness::slugify(name);
    for agent in agents {
        let base_dir = core::utils::expand_home(&agent.global_config_dir)?;
        let command_file = base_dir
            .join(&agent.commands_dir)
            .join(format!("{}.md", slug));
        if command_file.exists() {
            return Ok(true);
        }
    }
    Ok(false)
}
