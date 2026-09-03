use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const STATE_DIR: &str = ".agents";
pub const SOUL_FILE: &str = "SOUL.md";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentConfig {
    pub name: String,
    pub slug: String,
    pub description: String,
    pub harness: String,
    pub model: Option<String>,
    pub thinking: Option<String>,
}

impl AgentConfig {
    pub fn load(root: &Path) -> Result<Self> {
        let path = paths(root).config;
        let bytes =
            fs::read(&path).with_context(|| format!("could not read {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not valid agent configuration", path.display()))
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = paths(root).config;
        let json = serde_json::to_vec_pretty(self)?;
        let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::now_v7()));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&json)?;
            file.sync_all()?;
            fs::rename(&temporary, &path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.with_context(|| format!("could not write {}", path.display()))
    }
}

#[derive(Debug)]
pub struct AgentPaths {
    pub state: PathBuf,
    pub config: PathBuf,
    pub mcp: PathBuf,
    pub skills: PathBuf,
    pub runtime: PathBuf,
    pub sessions: PathBuf,
    pub soul: PathBuf,
}

pub fn paths(root: &Path) -> AgentPaths {
    let state = root.join(STATE_DIR);
    AgentPaths {
        config: state.join("agent.json"),
        mcp: state.join("mcps.json"),
        skills: state.join("skills"),
        runtime: state.join("runtime"),
        sessions: state.join("sessions"),
        soul: state.join(SOUL_FILE),
        state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selections_serialize_as_plain_strings() {
        let config = AgentConfig {
            name: "Agent".into(),
            slug: "agent".into(),
            description: "Test".into(),
            harness: "codex".into(),
            model: Some("gpt-5.6".into()),
            thinking: Some("high".into()),
        };
        let json = serde_json::to_value(config).unwrap();
        assert_eq!(json["model"], "gpt-5.6");
        assert_eq!(json["thinking"], "high");
    }

    #[test]
    fn saves_configuration_without_leaving_a_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(STATE_DIR)).unwrap();
        let config = AgentConfig {
            name: "Agent".into(),
            slug: "agent".into(),
            description: "Test".into(),
            harness: "codex".into(),
            model: None,
            thinking: None,
        };

        config.save(root.path()).unwrap();

        assert_eq!(AgentConfig::load(root.path()).unwrap().name, "Agent");
        assert_eq!(fs::read_dir(paths(root.path()).state).unwrap().count(), 1);
    }
}
