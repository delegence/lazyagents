use std::collections::BTreeSet;

use crate::core::{utils, ConfigFile};
use crate::error::{Error, Result};
use crate::harness;

#[derive(Debug, Clone, Default)]
pub struct UninstallReport {
    pub skills: Vec<String>,
    pub commands: Vec<String>,
    pub mcps: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn uninstall_from_profiles(
    profile_ids: &[String],
    skills: &[String],
    commands: &[String],
    mcps: &[String],
    agents: &[String],
) -> Result<UninstallReport> {
    let mut config = ConfigFile::load_or_create()?;
    let report = uninstall_from_profiles_in_config(
        &mut config,
        profile_ids,
        skills,
        commands,
        mcps,
        agents,
    )?;
    config.save()?;
    Ok(report)
}

pub fn uninstall_from_profiles_in_config(
    config: &mut ConfigFile,
    profile_ids: &[String],
    skills: &[String],
    commands: &[String],
    mcps: &[String],
    agents: &[String],
) -> Result<UninstallReport> {
    if skills.is_empty() && commands.is_empty() && mcps.is_empty() {
        return Err(Error::InvalidInput(
            "provide at least one --skill, --command, or --mcp".to_string(),
        ));
    }
    if profile_ids.is_empty() {
        return Err(Error::InvalidInput(
            "at least one target profile is required".to_string(),
        ));
    }

    let profile_indexes = resolve_profile_indexes(config, profile_ids)?;
    let mut removed_skills = BTreeSet::new();
    let mut removed_commands = BTreeSet::new();
    let mut removed_mcps = BTreeSet::new();

    for &index in &profile_indexes {
        let profile = &mut config.profiles[index];
        ensure_profile_contains(profile, skills, commands, mcps)?;
        remove_items(&mut profile.skills, skills, &mut removed_skills);
        remove_items(&mut profile.commands, commands, &mut removed_commands);
        remove_items(&mut profile.mcps, mcps, &mut removed_mcps);
    }

    prune_catalogs(config, &removed_skills, &removed_commands, &removed_mcps)?;

    let agent_filter = if agents.is_empty() {
        None
    } else {
        Some(agents.iter().cloned().collect::<BTreeSet<_>>())
    };

    let selected_profiles: BTreeSet<&str> = profile_ids.iter().map(|id| id.as_str()).collect();

    let active_profiles = config.active_profiles.clone();
    for (agent_id, active_profile) in active_profiles {
        if !selected_profiles.contains(active_profile.as_str()) {
            continue;
        }
        if let Some(filter) = &agent_filter {
            if !filter.contains(&agent_id) {
                continue;
            }
        }
        let Some(profile) = config
            .profiles
            .iter()
            .find(|profile| profile.id == active_profile)
            .cloned()
        else {
            continue;
        };
        if !profile.agents.iter().any(|agent| agent == &agent_id) {
            continue;
        }
        harness::apply_profile_for_agent(config, &profile, &agent_id)?;
    }

    Ok(UninstallReport {
        skills: removed_skills.into_iter().collect(),
        commands: removed_commands.into_iter().collect(),
        mcps: removed_mcps.into_iter().collect(),
        warnings: Vec::new(),
    })
}

fn resolve_profile_indexes(config: &ConfigFile, profile_ids: &[String]) -> Result<Vec<usize>> {
    let mut indexes = Vec::new();
    for profile_id in profile_ids {
        let index = config
            .profiles
            .iter()
            .position(|profile| profile.id == *profile_id)
            .ok_or_else(|| Error::NotFound(format!("profile '{}' not found", profile_id)))?;
        if !indexes.contains(&index) {
            indexes.push(index);
        }
    }
    Ok(indexes)
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

fn remove_items(list: &mut Vec<String>, to_remove: &[String], removed: &mut BTreeSet<String>) {
    let remove_set: BTreeSet<&str> = to_remove.iter().map(|name| name.as_str()).collect();
    list.retain(|item| {
        if remove_set.contains(item.as_str()) {
            removed.insert(item.clone());
            false
        } else {
            true
        }
    });
}

fn prune_catalogs(
    config: &mut ConfigFile,
    removed_skills: &BTreeSet<String>,
    removed_commands: &BTreeSet<String>,
    removed_mcps: &BTreeSet<String>,
) -> Result<()> {
    let used_skills: BTreeSet<&str> = config
        .profiles
        .iter()
        .flat_map(|profile| profile.skills.iter().map(|skill| skill.as_str()))
        .collect();
    let used_commands: BTreeSet<&str> = config
        .profiles
        .iter()
        .flat_map(|profile| profile.commands.iter().map(|cmd| cmd.as_str()))
        .collect();
    let used_mcps: BTreeSet<&str> = config
        .profiles
        .iter()
        .flat_map(|profile| profile.mcps.iter().map(|mcp| mcp.as_str()))
        .collect();

    for skill in removed_skills {
        if !used_skills.contains(skill.as_str()) {
            config.skills.retain(|entry| entry.name != *skill);
            let path = crate::core::skills_dir()?.join(skill);
            if path.exists() {
                std::fs::remove_dir_all(&path).map_err(|err| Error::io(&path, err))?;
            }
        }
    }

    for command in removed_commands {
        if !used_commands.contains(command.as_str()) {
            config.commands.retain(|entry| entry.name != *command);
            let path = crate::core::commands_dir()?.join(format!("{}.md", command));
            utils::remove_file_if_exists(&path)?;
        }
    }

    for mcp in removed_mcps {
        if !used_mcps.contains(mcp.as_str()) {
            config.mcp_servers.remove(mcp);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::uninstall_from_profiles_in_config;
    use crate::cli::test_setup::{EnvGuard, TEST_LOCK};
    use crate::core::{self, utils, CatalogEntry, ConfigFile, McpServer, Profile};
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    #[test]
    fn uninstall_errors_on_missing_items() {
        let mut config = ConfigFile::minimal();
        config.profiles.push(Profile {
            id: "work".to_string(),
            agents: vec!["codex".to_string()],
            skills: vec!["alpha".to_string()],
            commands: vec!["build".to_string()],
            mcps: vec!["local".to_string()],
            models: std::collections::BTreeMap::new(),
            extra: std::collections::BTreeMap::new(),
        });

        let err = uninstall_from_profiles_in_config(
            &mut config,
            &vec!["work".to_string()],
            &vec!["alpha".to_string(), "missing".to_string()],
            &vec!["build".to_string()],
            &vec!["ghost".to_string()],
            &[],
        )
        .unwrap_err();

        assert!(err.to_string().contains("does not include"));
    }

    #[test]
    fn uninstall_prunes_unused_catalog_and_cache() {
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
        config
            .mcp_servers
            .insert("local".to_string(), McpServer::default());
        config.profiles.push(Profile {
            id: "work".to_string(),
            agents: vec!["codex".to_string()],
            skills: vec!["alpha".to_string()],
            commands: vec!["build".to_string()],
            mcps: vec!["local".to_string()],
            models: BTreeMap::new(),
            extra: BTreeMap::new(),
        });

        let cache_dirs = core::ensure_cache_dirs().expect("cache dirs");
        let skill_dir = cache_dirs.skills_dir.join("alpha");
        utils::ensure_dir(&skill_dir).expect("skill dir");
        utils::write_string(&skill_dir.join("SKILL.md"), "Skill body").expect("skill file");
        let command_file = cache_dirs.commands_dir.join("build.md");
        utils::write_string(&command_file, "Command body").expect("command file");

        uninstall_from_profiles_in_config(
            &mut config,
            &vec!["work".to_string()],
            &vec!["alpha".to_string()],
            &vec!["build".to_string()],
            &vec!["local".to_string()],
            &[],
        )
        .expect("uninstall");

        assert!(config.skills.is_empty());
        assert!(config.commands.is_empty());
        assert!(config.mcp_servers.is_empty());
        assert!(!skill_dir.exists());
        assert!(!command_file.exists());
    }
}
