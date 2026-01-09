use std::fs;
use std::path::Path;

use serde::Deserialize;
use walkdir::WalkDir;

use crate::core::McpServer;
use crate::error::{Error, Result};
use crate::install::source::GithubSource;

#[derive(Debug, Default)]
pub struct Discovery {
    pub skills: Vec<DiscoveredSkill>,
    pub commands: Vec<DiscoveredCommand>,
    pub mcps: Vec<(String, McpServer)>,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct DiscoveredSkill {
    pub name: String,
    pub content: String,
    pub source: String,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct DiscoveredCommand {
    pub name: String,
    pub content: String,
    pub source: String,
    pub warnings: Vec<String>,
}

pub fn discover_components(repo_root: &Path, source: &GithubSource) -> Discovery {
    let mut discovery = Discovery::default();

    for entry in WalkDir::new(repo_root)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        if is_skill_file(path) {
            match parse_skill(path, repo_root, source) {
                Ok(skill) => discovery.skills.push(skill),
                Err(err) => discovery.warnings.push(err.to_string()),
            }
        } else if is_command_file(path) {
            match parse_command(path, repo_root, source) {
                Ok(command) => discovery.commands.push(command),
                Err(err) => discovery.warnings.push(err.to_string()),
            }
        }
    }

    discovery
}

fn is_skill_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name == "SKILL.md")
        .unwrap_or(false)
}

fn is_command_file(path: &Path) -> bool {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if file_name == "COMMAND.md" {
        return true;
    }
    if let Some(parent) = path.parent() {
        let parent_name = parent.file_name().and_then(|name| name.to_str());
        if matches!(parent_name, Some("commands") | Some("prompts")) {
            return file_name.ends_with(".md");
        }
    }
    false
}

fn parse_skill(path: &Path, repo_root: &Path, source: &GithubSource) -> Result<DiscoveredSkill> {
    let content = fs::read_to_string(path).map_err(|err| Error::io(path, err))?;
    let (name, warnings) = resolve_name(&content, path, repo_root, "skill");
    let source_id = source_id(source, repo_root, path);

    Ok(DiscoveredSkill {
        name,
        content,
        source: source_id,
        warnings,
    })
}

fn parse_command(
    path: &Path,
    repo_root: &Path,
    source: &GithubSource,
) -> Result<DiscoveredCommand> {
    let content = fs::read_to_string(path).map_err(|err| Error::io(path, err))?;
    let (name, warnings) = resolve_name(&content, path, repo_root, "command");
    let source_id = source_id(source, repo_root, path);

    Ok(DiscoveredCommand {
        name,
        content,
        source: source_id,
        warnings,
    })
}

fn resolve_name(content: &str, path: &Path, repo_root: &Path, kind: &str) -> (String, Vec<String>) {
    let mut warnings = Vec::new();
    if let Some(frontmatter) = parse_frontmatter(content) {
        if let Some(name) = frontmatter.name {
            return (name, warnings);
        }
        warnings.push(format!(
            "{} at {} is missing a name in frontmatter",
            kind,
            path.display()
        ));
    } else {
        warnings.push(format!(
            "{} at {} is missing frontmatter",
            kind,
            path.display()
        ));
    }

    let fallback = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && name != &"commands")
        .map(|name| name.to_string())
        .unwrap_or_else(|| fallback_name(path, repo_root));

    (fallback, warnings)
}

fn fallback_name(path: &Path, repo_root: &Path) -> String {
    if let Some(stem) = path.file_stem().and_then(|name| name.to_str()) {
        if stem != "SKILL" && stem != "COMMAND" {
            return stem.to_string();
        }
    }

    repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("item")
        .to_string()
}

fn source_id(source: &GithubSource, repo_root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(repo_root).unwrap_or(path);
    let rel = rel.to_string_lossy().replace('\\', "/");
    format!("{}/{}:{}", source.owner, source.repo, rel)
}

#[derive(Debug, Deserialize)]
struct Frontmatter {
    name: Option<String>,
}

fn parse_frontmatter(content: &str) -> Option<Frontmatter> {
    let mut lines = content.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }

    let mut yaml = String::new();
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        yaml.push_str(line);
        yaml.push('\n');
    }

    if yaml.trim().is_empty() {
        return None;
    }

    serde_yaml::from_str(&yaml).ok()
}

#[cfg(test)]
mod tests {
    use super::{discover_components, parse_frontmatter};
    use crate::install::source::{GithubSource, ReferenceKind};
    use tempfile::tempdir;

    #[test]
    fn parse_frontmatter_name() {
        let input = "---\nname: Test\n---\nBody";
        let fm = parse_frontmatter(input).unwrap();
        assert_eq!(fm.name.as_deref(), Some("Test"));
    }

    #[test]
    fn discovers_skills_and_commands() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();

        let skill_dir = root.join("skills").join("alpha");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Alpha\n---\nSkill body",
        )
        .unwrap();

        let commands_dir = root.join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        std::fs::write(
            commands_dir.join("build.md"),
            "---\nname: Build\n---\nCommand body",
        )
        .unwrap();

        let source = GithubSource {
            owner: "acme".to_string(),
            repo: "repo".to_string(),
            reference: None,
            reference_kind: ReferenceKind::Heads,
            archive_url: None,
        };

        let discovery = discover_components(root, &source);
        assert_eq!(discovery.skills.len(), 1);
        assert_eq!(discovery.skills[0].name, "Alpha");
        assert_eq!(discovery.commands.len(), 1);
        assert_eq!(discovery.commands[0].name, "Build");
    }
}
