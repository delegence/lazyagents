use std::collections::BTreeMap;

use tempfile::tempdir;

use crate::core::{self, utils as mews_utils, CatalogEntry, ConfigFile, Profile};
use crate::error::{Error, Result};
use crate::install::discover::{discover_components, Discovery};
use crate::install::extract::{extract_zip, find_repo_root};
use crate::install::source::{download_archive, parse_github_source};

#[derive(Debug, Clone, Default)]
pub struct InstallReport {
    pub skills: Vec<String>,
    pub commands: Vec<String>,
    pub mcps: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn install_from_source(source: &str, target_profiles: &[String]) -> Result<InstallReport> {
    let mut config = ConfigFile::load_or_create()?;
    let discovery = discover_from_source(source)?;
    let report = install_from_discovery(&mut config, target_profiles, discovery)?;
    config.save()?;
    Ok(report)
}

pub fn discover_from_source(source: &str) -> Result<Discovery> {
    let source_info = parse_github_source(source)?;
    let temp_dir = tempdir().map_err(|err| Error::io("tempdir", err))?;
    let archive_path = temp_dir.path().join("archive.zip");
    download_archive(&source_info, &archive_path)?;

    let extract_dir = temp_dir.path().join("extract");
    extract_zip(&archive_path, &extract_dir)?;
    let repo_root = find_repo_root(&extract_dir);

    Ok(discover_components(&repo_root, &source_info))
}

pub fn install_from_discovery(
    config: &mut ConfigFile,
    target_profiles: &[String],
    discovery: Discovery,
) -> Result<InstallReport> {
    if target_profiles.is_empty() {
        return Err(Error::InvalidInput(
            "at least one target profile is required".to_string(),
        ));
    }

    let profile_indexes = resolve_profile_indexes(config, target_profiles)?;
    let cache_dirs = core::ensure_cache_dirs()?;

    let mut report = apply_discovery(config, &profile_indexes, &cache_dirs, discovery)?;
    apply_active_profiles(config, target_profiles, &mut report)?;
    Ok(report)
}

fn apply_discovery(
    config: &mut ConfigFile,
    profile_indexes: &[usize],
    cache_dirs: &core::CacheDirs,
    discovery: Discovery,
) -> Result<InstallReport> {
    let mut report = InstallReport::default();
    report.warnings.extend(discovery.warnings);

    for skill in discovery.skills {
        let name = skill.name.clone();
        let target_dir = cache_dirs.skills_dir.join(&name);
        mews_utils::ensure_dir(&target_dir)?;
        mews_utils::write_string(&target_dir.join("SKILL.md"), &skill.content)?;
        upsert_catalog_entry(&mut config.skills, &name, &skill.source);
        report.skills.push(name.clone());
        add_to_profiles(
            config,
            profile_indexes,
            |profile| &mut profile.skills,
            &name,
        );
        report.warnings.extend(skill.warnings);
    }

    for command in discovery.commands {
        let name = command.name.clone();
        let target_file = cache_dirs.commands_dir.join(format!("{}.md", &name));
        mews_utils::write_string(&target_file, &command.content)?;
        upsert_catalog_entry(&mut config.commands, &name, &command.source);
        report.commands.push(name.clone());
        add_to_profiles(
            config,
            profile_indexes,
            |profile| &mut profile.commands,
            &name,
        );
        report.warnings.extend(command.warnings);
    }

    for (name, server) in discovery.mcps {
        config.mcp_servers.entry(name.clone()).or_insert(server);
        report.mcps.push(name.clone());
        add_to_profiles(config, profile_indexes, |profile| &mut profile.mcps, &name);
    }

    Ok(report)
}

fn apply_active_profiles(
    config: &ConfigFile,
    target_profiles: &[String],
    report: &mut InstallReport,
) -> Result<()> {
    let selected: std::collections::BTreeSet<&str> =
        target_profiles.iter().map(|id| id.as_str()).collect();

    let active_profiles = config.active_profiles.clone();
    for (agent_id, active_profile) in active_profiles {
        if !selected.contains(active_profile.as_str()) {
            continue;
        }
        let Some(agent) = config.agents.iter().find(|agent| agent.id == agent_id) else {
            continue;
        };
        if !agent.installed {
            continue;
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
        let apply = crate::harness::apply_profile_for_agent(config, &profile, &agent_id)?;
        report.warnings.extend(apply.warnings);
    }

    Ok(())
}

fn resolve_profile_indexes(config: &ConfigFile, target_profiles: &[String]) -> Result<Vec<usize>> {
    let mut indexes = Vec::new();
    for profile_id in target_profiles {
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

fn add_to_profiles<F>(config: &mut ConfigFile, profile_indexes: &[usize], mut field: F, name: &str)
where
    F: FnMut(&mut Profile) -> &mut Vec<String>,
{
    for &index in profile_indexes {
        let profile = &mut config.profiles[index];
        let list = field(profile);
        if !list.iter().any(|entry| entry == name) {
            list.push(name.to_string());
        }
    }
}

fn upsert_catalog_entry(entries: &mut Vec<CatalogEntry>, name: &str, source: &str) {
    if let Some(entry) = entries.iter_mut().find(|entry| entry.name == name) {
        if entry.source.is_none() {
            entry.source = Some(source.to_string());
        }
        return;
    }

    entries.push(CatalogEntry {
        name: name.to_string(),
        source: Some(source.to_string()),
        extra: BTreeMap::new(),
    });
}
