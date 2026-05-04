use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::profile::mcp::{parse_mcp_definitions, McpSummary};
use crate::profile::ProfileName;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactStatus {
    Present,
    Missing,
    NotFile,
}

#[derive(Debug, Clone)]
pub struct ProfileSummary {
    pub name: ProfileName,
    pub path: PathBuf,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub instruction_source: ArtifactStatus,
    pub valid_skills: Vec<String>,
    pub ignored_skills: Vec<String>,
    pub commands: Vec<String>,
    pub ignored_command_files: Vec<String>,
    pub agents: Vec<String>,
    pub ignored_agent_files: Vec<String>,
    pub mcp_summary: McpSummary,
    pub models: BTreeMap<String, Value>,
    pub permissions: BTreeMap<String, Value>,
    pub validation_issues: Vec<crate::profile::validation::ValidationIssue>,
}

pub(crate) fn artifact_status(path: PathBuf) -> ArtifactStatus {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => ArtifactStatus::Present,
        Ok(_) => ArtifactStatus::NotFile,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ArtifactStatus::Missing,
        Err(_) => ArtifactStatus::Missing,
    }
}

pub(crate) fn scan_skills(path: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let mut valid = Vec::new();
    let mut ignored = Vec::new();
    if !path.exists() {
        return Ok((valid, ignored));
    }

    for entry in
        std::fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if entry.file_type()?.is_dir() && entry.path().join("SKILL.md").is_file() {
            valid.push(name);
        } else {
            ignored.push(name);
        }
    }

    valid.sort();
    ignored.sort();
    Ok((valid, ignored))
}

pub(crate) fn scan_commands(path: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let mut commands = Vec::new();
    let mut ignored = Vec::new();
    if !path.exists() {
        return Ok((commands, ignored));
    }

    scan_command_dir(path, path, &mut commands, &mut ignored)?;
    commands.sort();
    ignored.sort();
    Ok((commands, ignored))
}

fn scan_command_dir(
    root: &Path,
    path: &Path,
    commands: &mut Vec<String>,
    ignored: &mut Vec<String>,
) -> Result<()> {
    for entry in
        std::fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let entry_path = entry.path();
        if entry.file_type()?.is_dir() {
            scan_command_dir(root, &entry_path, commands, ignored)?;
            continue;
        }

        let relative = relative_slash_path(root, &entry_path)?;
        if entry_path
            .extension()
            .is_some_and(|extension| extension == "md")
        {
            commands.push(relative);
        } else {
            ignored.push(relative);
        }
    }

    Ok(())
}

pub(crate) fn summarize_mcps(path: &Path) -> Result<McpSummary> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(McpSummary::Empty),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read MCP definitions at {}", path.display()));
        }
    };

    if text.trim().is_empty() {
        return Ok(McpSummary::Empty);
    }

    let definitions = match parse_mcp_definitions(&text) {
        Ok(definitions) => definitions,
        Err(error) => return Ok(McpSummary::Invalid(error.to_string())),
    };

    let mut names = Vec::new();
    for definition in definitions {
        if definition.enabled {
            names.push(definition.name);
        } else {
            names.push(format!("{} (disabled)", definition.name));
        }
    }
    names.sort();
    Ok(McpSummary::Servers(names))
}

fn relative_slash_path(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("{} is not under {}", path.display(), root.display()))?;
    Ok(relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}
