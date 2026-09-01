use crate::profile::name::ProfileName;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

pub const PROFILE_FILE_NAME: &str = "PROFILE.md";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProfileConfig {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub models: BTreeMap<String, Value>,
    #[serde(default)]
    pub permissions: BTreeMap<String, Value>,
}

#[derive(Debug, Clone)]
pub struct ProfileDocument {
    pub config: ProfileConfig,
    pub instructions: String,
}

impl ProfileConfig {
    pub fn default_for(name: &ProfileName) -> Self {
        Self {
            name: Some(default_display_name(name)),
            description: Some(String::new()),
            models: BTreeMap::new(),
            permissions: BTreeMap::new(),
        }
    }

    pub fn model_preference(&self, harness_id: &str) -> Value {
        self.models
            .get(harness_id)
            .cloned()
            .unwrap_or_else(default_preference_value)
    }

    pub fn permission_preference(&self, harness_id: &str) -> Value {
        self.permissions
            .get(harness_id)
            .cloned()
            .unwrap_or_else(default_preference_value)
    }
}

fn default_display_name(name: &ProfileName) -> String {
    let mut chars = name.as_str().chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

pub fn default_preference_value() -> Value {
    json!("default")
}

pub fn default_profile_document(name: &ProfileName) -> ProfileDocument {
    ProfileDocument {
        config: ProfileConfig::default_for(name),
        instructions: String::new(),
    }
}

pub fn read_profile_document(profile_path: &Path) -> Result<ProfileDocument> {
    let path = profile_path.join(PROFILE_FILE_NAME);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("missing or unreadable profile file at {}", path.display()))?;
    parse_profile_document(&text)
        .with_context(|| format!("invalid profile file at {}", path.display()))
}

pub fn read_profile_config(profile_path: &Path) -> Result<ProfileConfig> {
    Ok(read_profile_document(profile_path)?.config)
}

pub fn read_profile_instructions(profile_path: &Path) -> Result<String> {
    Ok(read_profile_document(profile_path)?.instructions)
}

pub fn write_profile_document(profile_path: &Path, document: &ProfileDocument) -> Result<()> {
    let path = profile_path.join(PROFILE_FILE_NAME);
    let text = profile_document_to_markdown(document)?;
    crate::file_system::write_text_atomic(&path, &text)
        .with_context(|| format!("failed to write profile file at {}", path.display()))
}

pub fn parse_profile_document(text: &str) -> Result<ProfileDocument> {
    let (frontmatter, body) = split_markdown_frontmatter(text)?;
    let config: ProfileConfig = crate::yaml::from_str(frontmatter)?;
    Ok(ProfileDocument {
        config,
        instructions: body.to_string(),
    })
}

pub fn profile_document_to_markdown(document: &ProfileDocument) -> Result<String> {
    let yaml = crate::yaml::to_string(&document.config)?;
    Ok(format!(
        "---\n{}---\n{}",
        trim_yaml_header(&yaml),
        document.instructions
    ))
}

fn split_markdown_frontmatter(text: &str) -> Result<(&str, &str)> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .ok_or_else(|| anyhow::anyhow!("{PROFILE_FILE_NAME} must start with YAML frontmatter"))?;
    rest.split_once("\n---\n")
        .or_else(|| rest.split_once("\r\n---\r\n"))
        .ok_or_else(|| anyhow::anyhow!("{PROFILE_FILE_NAME} must close YAML frontmatter with ---"))
}

fn trim_yaml_header(yaml: &str) -> String {
    yaml.strip_prefix("---\n").unwrap_or(yaml).to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileConfigStatus {
    Valid,
    Missing,
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_display_name_capitalizes_folder_name() {
        let config = ProfileConfig::default_for(&ProfileName::parse("work-profile").unwrap());

        assert_eq!(config.name.as_deref(), Some("Work-profile"));
    }

    #[test]
    fn profile_config_missing_preferences_resolve_to_default() {
        let config: ProfileConfig = serde_json::from_str(
            r#"{"models":{"codex":"gpt-5"},"permissions":{"codex":"on-request"}}"#,
        )
        .unwrap();

        assert_eq!(config.model_preference("codex"), "gpt-5");
        assert_eq!(config.model_preference("opencode"), "default");
        assert_eq!(config.permission_preference("opencode"), "default");
    }

    #[test]
    fn profile_config_preserves_opencode_object_permission_preference() {
        let config: ProfileConfig = serde_json::from_str(
            r#"{"models":{},"permissions":{"opencode":{"*":"ask","bash":"allow"}}}"#,
        )
        .unwrap();

        assert_eq!(
            config.permission_preference("opencode"),
            serde_json::json!({"*":"ask","bash":"allow"})
        );
    }

    #[test]
    fn profile_document_round_trips_frontmatter_and_instructions() {
        let document = ProfileDocument {
            config: ProfileConfig {
                name: Some("Work".to_string()),
                description: Some("Daily profile".to_string()),
                models: BTreeMap::from([("codex".to_string(), json!("gpt-5"))]),
                permissions: BTreeMap::from([("codex".to_string(), json!("on-request"))]),
            },
            instructions: "# Instructions\n\nDo careful work.\n".to_string(),
        };

        let text = profile_document_to_markdown(&document).unwrap();
        let parsed = parse_profile_document(&text).unwrap();

        assert_eq!(parsed.config.name.as_deref(), Some("Work"));
        assert_eq!(parsed.config.model_preference("codex"), "gpt-5");
        assert_eq!(parsed.instructions, "# Instructions\n\nDo careful work.\n");
    }
}
