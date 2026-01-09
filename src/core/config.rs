use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::jsonc;
use super::paths;
use crate::core::utils as mews_utils;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConfigFile {
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default)]
    pub active_profiles: BTreeMap<String, String>,
    #[serde(default)]
    pub skills: Vec<CatalogEntry>,
    #[serde(default)]
    pub commands: Vec<CatalogEntry>,
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, McpServer>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl ConfigFile {
    pub fn minimal() -> Self {
        Self {
            agents: default_agents(),
            profiles: Vec::new(),
            active_profiles: BTreeMap::new(),
            skills: Vec::new(),
            commands: Vec::new(),
            mcp_servers: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }

    pub fn load() -> Result<Self> {
        let path = paths::config_file_path()?;
        Self::load_from_path(&path)
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path).map_err(|err| Error::io(path, err))?;
        let stripped = jsonc::strip_jsonc(&raw);
        let config = serde_json::from_str(&stripped).map_err(|err| Error::serde_json(path, err))?;
        Ok(config)
    }

    pub fn load_or_create() -> Result<Self> {
        let path = paths::config_file_path()?;
        if path.exists() {
            match Self::load_from_path(&path) {
                Ok(config) => {
                    if config.validate_schema().is_ok() {
                        Ok(config)
                    } else {
                        Self::reset_to_minimal(&path)
                    }
                }
                Err(_) => Self::reset_to_minimal(&path),
            }
        } else {
            let config = Self::minimal();
            config.save_to_path(&path)?;
            Ok(config)
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = paths::config_file_path()?;
        self.save_to_path(&path)
    }

    pub fn save_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| Error::io(parent, err))?;
        }
        let contents =
            serde_json::to_string_pretty(self).map_err(|err| Error::serde_json(path, err))?;
        mews_utils::write_atomic(path, format!("{}\n", contents).as_bytes())
    }

    fn reset_to_minimal(path: &Path) -> Result<Self> {
        let config = Self::minimal();
        config.save_to_path(path)?;
        Ok(config)
    }

    fn validate_schema(&self) -> Result<()> {
        if self.agents.is_empty() {
            return Err(Error::InvalidInput("no agents configured".to_string()));
        }

        let mut ids = std::collections::BTreeSet::new();
        for agent in &self.agents {
            if agent.id.trim().is_empty() {
                return Err(Error::InvalidInput("agent id cannot be empty".to_string()));
            }
            if !ids.insert(agent.id.clone()) {
                return Err(Error::InvalidInput(format!(
                    "duplicate agent id '{}'",
                    agent.id
                )));
            }
            if agent.rules_file.trim().is_empty() {
                return Err(Error::InvalidInput(format!(
                    "agent '{}' rulesFile cannot be empty",
                    agent.id
                )));
            }
            if agent.global_config_dir.trim().is_empty() {
                return Err(Error::InvalidInput(format!(
                    "agent '{}' globalConfigDir cannot be empty",
                    agent.id
                )));
            }
            if agent.project_config_dir.trim().is_empty() {
                return Err(Error::InvalidInput(format!(
                    "agent '{}' projectConfigDir cannot be empty",
                    agent.id
                )));
            }
            if agent.config_file.trim().is_empty() {
                return Err(Error::InvalidInput(format!(
                    "agent '{}' configFile cannot be empty",
                    agent.id
                )));
            }
            if agent.skills_dir.trim().is_empty() {
                return Err(Error::InvalidInput(format!(
                    "agent '{}' skillsDir cannot be empty",
                    agent.id
                )));
            }
            if agent.commands_dir.trim().is_empty() {
                return Err(Error::InvalidInput(format!(
                    "agent '{}' commandsDir cannot be empty",
                    agent.id
                )));
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    pub id: String,
    pub rules_file: String,
    pub global_config_dir: String,
    pub project_config_dir: String,
    pub config_file: String,
    pub skills_dir: String,
    pub commands_dir: String,
    pub installed: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub mcps: Vec<String>,
    #[serde(default)]
    pub models: BTreeMap<String, String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub name: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

fn default_agents() -> Vec<AgentConfig> {
    vec![
        AgentConfig {
            id: "codex".to_string(),
            rules_file: "AGENTS.md".to_string(),
            global_config_dir: "~/.codex".to_string(),
            project_config_dir: ".codex".to_string(),
            config_file: "config.toml".to_string(),
            skills_dir: "skills".to_string(),
            commands_dir: "prompts".to_string(),
            installed: false,
            extra: BTreeMap::new(),
        },
        AgentConfig {
            id: "claude".to_string(),
            rules_file: "CLAUDE.md".to_string(),
            global_config_dir: "~/.claude".to_string(),
            project_config_dir: ".claude".to_string(),
            config_file: "settings.local.json".to_string(),
            skills_dir: "skills".to_string(),
            commands_dir: "commands".to_string(),
            installed: false,
            extra: BTreeMap::new(),
        },
        AgentConfig {
            id: "opencode".to_string(),
            rules_file: "AGENTS.md".to_string(),
            global_config_dir: "~/.config/opencode".to_string(),
            project_config_dir: ".opencode".to_string(),
            config_file: "opencode.jsonc".to_string(),
            skills_dir: "skill".to_string(),
            commands_dir: "command".to_string(),
            installed: false,
            extra: BTreeMap::new(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::ConfigFile;
    use crate::cli::test_setup::{EnvGuard, TEST_LOCK};
    use crate::core::paths;

    #[test]
    fn minimal_has_known_agents() {
        let config = ConfigFile::minimal();
        let ids: Vec<&str> = config
            .agents
            .iter()
            .map(|agent| agent.id.as_str())
            .collect();
        assert_eq!(ids, vec!["codex", "claude", "opencode"]);
        assert!(config.skills.is_empty());
        assert!(config.commands.is_empty());
    }

    #[test]
    fn load_or_create_resets_on_invalid_json() {
        let _lock = TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let _env = EnvGuard::new(dir.path());

        let path = paths::config_file_path().expect("config path");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create config dir");
        }
        std::fs::write(&path, "{ not json").expect("write config");

        let config = ConfigFile::load_or_create().expect("load config");
        assert_eq!(config.agents.len(), 3);

        let contents = std::fs::read_to_string(&path).expect("read config");
        assert!(contents.contains("\"rulesFile\""));
    }

    #[test]
    fn load_or_create_resets_on_invalid_schema() {
        let _lock = TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let _env = EnvGuard::new(dir.path());

        let path = paths::config_file_path().expect("config path");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create config dir");
        }
        let invalid_schema = r#"{
  "agents": [
    {
      "id": "codex",
      "rulesFile": "",
      "globalConfigDir": "~/.codex",
      "projectConfigDir": ".codex",
      "configFile": "config.toml",
      "skillsDir": "skills",
      "commandsDir": "prompts",
      "installed": false
    }
  ]
}"#;
        std::fs::write(&path, invalid_schema).expect("write config");

        let config = ConfigFile::load_or_create().expect("load config");
        assert_eq!(config.agents.len(), 3);
        assert_eq!(config.agents[0].rules_file, "AGENTS.md");
    }
}
