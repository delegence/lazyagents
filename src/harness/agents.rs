use anyhow::{Context, Result};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::harness::drift::DriftItem;
use crate::harness::integration::ImportedFile;
use crate::harness::managed::write_text_atomic;

#[derive(Debug, Clone)]
pub struct SubAgent {
    pub name: String,
    pub description: String,
    pub model: Option<Value>,
    pub tools: Option<Value>,
    pub permission: Option<Value>,
    pub max_turns: Option<u64>,
    pub harness: BTreeMap<String, Value>,
    pub body: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    model: Option<Value>,
    #[serde(default)]
    tools: Option<Value>,
    #[serde(default)]
    permission: Option<Value>,
    #[serde(default, alias = "max_turns")]
    max_turns: Option<u64>,
    #[serde(default)]
    harness: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedAgent {
    pub relative_path: PathBuf,
    pub contents: String,
}

pub fn profile_agents(profile_path: &Path) -> Result<Vec<SubAgent>> {
    let agents_dir = profile_path.join("agents");
    let mut files = Vec::new();
    if !agents_dir.exists() {
        return Ok(Vec::new());
    }
    collect_markdown_files(&agents_dir, &mut files)?;
    let mut agents = Vec::new();
    let mut names = BTreeSet::new();
    for file in files {
        let agent = read_agent(&file)?;
        if !names.insert(agent.name.clone()) {
            anyhow::bail!("duplicate agent name {}", agent.name);
        }
        agents.push(agent);
    }
    agents.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(agents)
}

pub fn scan_agents(path: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let mut valid = Vec::new();
    let mut ignored = Vec::new();
    if !path.exists() {
        return Ok((valid, ignored));
    }
    scan_agent_dir(path, path, &mut valid, &mut ignored)?;
    valid.sort();
    ignored.sort();
    Ok((valid, ignored))
}

pub fn apply_rendered_agents(agents: &[RenderedAgent], target_dir: &Path) -> Result<()> {
    fs::create_dir_all(target_dir)
        .with_context(|| format!("failed to create {}", target_dir.display()))?;
    for agent in agents {
        let target = target_dir.join(&agent.relative_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        write_text_atomic(&target, &agent.contents)
            .with_context(|| format!("failed to write {}", target.display()))?;
    }
    Ok(())
}

pub fn verify_rendered_agents(agents: &[RenderedAgent], target_dir: &Path) -> Result<()> {
    for agent in agents {
        let target = target_dir.join(&agent.relative_path);
        let actual = fs::read_to_string(&target)
            .with_context(|| format!("failed to read rendered agent {}", target.display()))?;
        if actual != agent.contents {
            anyhow::bail!(
                "sub-agent file {} does not match rendered profile sub-agent",
                target.display()
            );
        }
    }
    Ok(())
}

pub fn collect_rendered_agent_drift(
    agents: &[RenderedAgent],
    target_dir: &Path,
    items: &mut Vec<DriftItem>,
) -> Result<()> {
    let mut expected = BTreeSet::new();
    for agent in agents {
        expected.insert(agent.relative_path.clone());
        let target = target_dir.join(&agent.relative_path);
        match fs::read_to_string(&target) {
            Ok(actual) if actual == agent.contents => {}
            Ok(_) => items.push(DriftItem {
                surface: "agents".to_string(),
                detail: format!("{} does not match active profile", target.display()),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => items.push(DriftItem {
                surface: "agents".to_string(),
                detail: format!("{} is missing", target.display()),
            }),
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", target.display()))
            }
        }
    }

    if target_dir.exists() {
        for file in crate::harness::fs::import_files_recursive(target_dir, target_dir)? {
            if !expected.contains(&file.relative_path) {
                items.push(DriftItem {
                    surface: "agents".to_string(),
                    detail: format!(
                        "unexpected managed entry {}",
                        target_dir.join(file.relative_path).display()
                    ),
                });
            }
        }
    }
    Ok(())
}

pub fn sub_agent_import_file(agent: &SubAgent) -> ImportedFile {
    ImportedFile {
        relative_path: PathBuf::from(format!("{}.md", agent.name)),
        contents: sub_agent_to_markdown(agent).into_bytes(),
    }
}

pub fn sub_agent_to_markdown(agent: &SubAgent) -> String {
    let mut map = Mapping::new();
    map.insert(
        Value::String("name".to_string()),
        Value::String(agent.name.clone()),
    );
    map.insert(
        Value::String("description".to_string()),
        Value::String(agent.description.clone()),
    );
    if let Some(model) = &agent.model {
        map.insert(Value::String("model".to_string()), model.clone());
    }
    if let Some(tools) = &agent.tools {
        map.insert(Value::String("tools".to_string()), tools.clone());
    }
    if let Some(permission) = &agent.permission {
        map.insert(Value::String("permission".to_string()), permission.clone());
    }
    if let Some(max_turns) = agent.max_turns {
        map.insert(
            Value::String("maxTurns".to_string()),
            Value::Number(max_turns.into()),
        );
    }
    if !agent.harness.is_empty() {
        let mut harness = Mapping::new();
        for (key, value) in &agent.harness {
            harness.insert(Value::String(key.clone()), value.clone());
        }
        map.insert(
            Value::String("harness".to_string()),
            Value::Mapping(harness),
        );
    }
    let yaml = serde_yaml::to_string(&map).unwrap_or_default();
    format!("---\n{}---\n{}\n", trim_yaml_header(&yaml), agent.body)
}

pub fn parse_sub_agent(text: &str) -> Result<SubAgent> {
    let (frontmatter, body) = split_markdown_frontmatter(text)?;
    let frontmatter: AgentFrontmatter = serde_yaml::from_str(frontmatter)?;
    let body = body.trim().to_string();
    validate_agent_name(&frontmatter.name)?;
    if frontmatter.description.trim().is_empty() {
        anyhow::bail!("agent description cannot be empty");
    }
    Ok(SubAgent {
        name: frontmatter.name,
        description: frontmatter.description,
        model: frontmatter.model,
        tools: frontmatter.tools,
        permission: frontmatter.permission,
        max_turns: frontmatter.max_turns,
        harness: frontmatter.harness,
        body,
    })
}

fn validate_agent_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("agent name cannot be empty");
    }
    if name != name.trim() {
        anyhow::bail!("agent name cannot have leading or trailing whitespace");
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        anyhow::bail!(
            "agent name {name} may contain only ASCII letters, numbers, dash, and underscore"
        );
    }
    Ok(())
}

pub fn split_markdown_frontmatter(text: &str) -> Result<(&str, &str)> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .ok_or_else(|| anyhow::anyhow!("sub-agent file must start with YAML frontmatter"))?;
    rest.split_once("\n---\n")
        .or_else(|| rest.split_once("\r\n---\r\n"))
        .ok_or_else(|| anyhow::anyhow!("sub-agent file must close YAML frontmatter with ---"))
}

pub fn select_harness_value<'a>(value: Option<&'a Value>, harness_id: &str) -> Option<&'a Value> {
    let value = value?;
    if let Value::Mapping(map) = value {
        map.get(Value::String(harness_id.to_string()))
            .or_else(|| map.get(Value::String("default".to_string())))
    } else {
        Some(value)
    }
}

pub fn harness_scoped_value(harness_id: &str, value: Option<Value>) -> Option<Value> {
    value.map(|value| {
        let mut map = Mapping::new();
        map.insert(Value::String(harness_id.to_string()), value);
        Value::Mapping(map)
    })
}

pub fn yaml_scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        _ => None,
    }
}

pub fn remove_value(map: &mut Mapping, key: &str) -> Option<Value> {
    map.remove(Value::String(key.to_string()))
}

pub fn remove_string(map: &mut Mapping, key: &str) -> Result<Option<String>> {
    match remove_value(map, key) {
        Some(Value::String(value)) => Ok(Some(value)),
        Some(other) => anyhow::bail!("{key} must be a string, got {}", format_yaml_value(&other)),
        None => Ok(None),
    }
}

pub fn format_yaml_value(value: &Value) -> String {
    serde_yaml::to_string(value)
        .unwrap_or_else(|_| "<unprintable>".to_string())
        .trim()
        .to_string()
}

fn read_agent(path: &Path) -> Result<SubAgent> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    parse_sub_agent(&text).with_context(|| format!("invalid agent {}", path.display()))
}

fn collect_markdown_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if entry.file_type()?.is_dir() {
            collect_markdown_files(&entry.path(), out)?;
        } else if entry.path().extension().is_some_and(|ext| ext == "md") {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(())
}

fn scan_agent_dir(
    root: &Path,
    path: &Path,
    valid: &mut Vec<String>,
    ignored: &mut Vec<String>,
) -> Result<()> {
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let entry_path = entry.path();
        if entry.file_type()?.is_dir() {
            scan_agent_dir(root, &entry_path, valid, ignored)?;
            continue;
        }
        let relative = relative_slash_path(root, &entry_path)?;
        if entry_path
            .extension()
            .is_some_and(|extension| extension == "md")
        {
            valid.push(relative);
        } else {
            ignored.push(relative);
        }
    }
    Ok(())
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

fn trim_yaml_header(yaml: &str) -> String {
    yaml.strip_prefix("---\n").unwrap_or(yaml).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_body_is_agent_prompt() {
        let agent = parse_sub_agent(
            r#"---
name: reviewer
description: Reviews code
---
Review code carefully.
"#,
        )
        .unwrap();

        assert_eq!(agent.name, "reviewer");
        assert_eq!(agent.description, "Reviews code");
        assert_eq!(agent.body, "Review code carefully.");
    }

    #[test]
    fn empty_markdown_body_is_allowed() {
        let agent = parse_sub_agent(
            r#"---
name: reviewer
description: Reviews code
---
"#,
        )
        .unwrap();

        assert_eq!(agent.name, "reviewer");
        assert_eq!(agent.body, "");
    }

    #[test]
    fn sub_agent_markdown_round_trips_core_fields() {
        let agent = SubAgent {
            name: "reviewer".to_string(),
            description: "Reviews code".to_string(),
            model: Some(Value::String("inherit".to_string())),
            tools: None,
            permission: None,
            max_turns: Some(7),
            harness: BTreeMap::new(),
            body: "Review code carefully.".to_string(),
        };

        let markdown = sub_agent_to_markdown(&agent);
        let parsed = parse_sub_agent(&markdown).unwrap();

        assert_eq!(parsed.name, agent.name);
        assert_eq!(parsed.description, agent.description);
        assert_eq!(parsed.body, agent.body);
        assert_eq!(parsed.max_turns, agent.max_turns);
    }

    #[test]
    fn rejects_path_like_agent_names() {
        for name in [
            "../reviewer",
            "nested/reviewer",
            "nested\\reviewer",
            ".reviewer",
            "reviewer.md",
            " reviewer",
            "reviewer ",
            "",
        ] {
            let markdown =
                format!("---\nname: {name:?}\ndescription: Reviews code\n---\nReview carefully.\n");

            assert!(parse_sub_agent(&markdown).is_err(), "{name:?}");
        }
    }

    #[test]
    fn accepts_identifier_agent_names() {
        for name in ["reviewer", "code-reviewer", "reviewer_2", "A1"] {
            let markdown =
                format!("---\nname: {name}\ndescription: Reviews code\n---\nReview carefully.\n");

            let agent = parse_sub_agent(&markdown).unwrap();
            assert_eq!(agent.name, name);
        }
    }
}
