use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const STATE_DIR: &str = ".agents";
pub const SOUL_FILE: &str = "SOUL.md";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentColor {
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Orange,
    Coral,
    Pink,
    Rose,
    Violet,
    Lavender,
    Purple,
    Teal,
    Turquoise,
    Mint,
    Lime,
    Gold,
    Amber,
    Sky,
}

impl AgentColor {
    const ALL: [Self; 20] = [
        Self::Red,
        Self::Green,
        Self::Yellow,
        Self::Blue,
        Self::Magenta,
        Self::Cyan,
        Self::Orange,
        Self::Coral,
        Self::Pink,
        Self::Rose,
        Self::Violet,
        Self::Lavender,
        Self::Purple,
        Self::Teal,
        Self::Turquoise,
        Self::Mint,
        Self::Lime,
        Self::Gold,
        Self::Amber,
        Self::Sky,
    ];

    pub fn random() -> Self {
        let random = uuid::Uuid::now_v7();
        Self::ALL[usize::from(random.as_bytes()[15]) % Self::ALL.len()]
    }

    pub fn ansi(self) -> &'static str {
        match self {
            Self::Red => "1;91",
            Self::Green => "1;92",
            Self::Yellow => "1;93",
            Self::Blue => "1;94",
            Self::Magenta => "1;95",
            Self::Cyan => "1;96",
            Self::Orange => "1;38;5;208",
            Self::Coral => "1;38;5;209",
            Self::Pink => "1;38;5;212",
            Self::Rose => "1;38;5;211",
            Self::Violet => "1;38;5;141",
            Self::Lavender => "1;38;5;183",
            Self::Purple => "1;38;5;135",
            Self::Teal => "1;38;5;80",
            Self::Turquoise => "1;38;5;45",
            Self::Mint => "1;38;5;121",
            Self::Lime => "1;38;5;154",
            Self::Gold => "1;38;5;220",
            Self::Amber => "1;38;5;214",
            Self::Sky => "1;38;5;117",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<AgentColor>,
    pub slug: String,
    pub description: String,
    pub harness: String,
    pub model: Option<String>,
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<crate::permissions::Approval>,
    #[serde(default)]
    pub permission_mode: crate::permissions::Mode,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub draft: String,
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout: std::num::NonZeroU64,
}

pub fn default_idle_timeout() -> std::num::NonZeroU64 {
    std::num::NonZeroU64::new(300).unwrap()
}

impl AgentConfig {
    pub fn new(name: &str, slug: &str, description: &str, harness: &str) -> Self {
        Self {
            name: name.to_owned(),
            color: None,
            slug: slug.to_owned(),
            description: description.to_owned(),
            harness: harness.to_owned(),
            model: None,
            thinking: None,
            permissions: Vec::new(),
            permission_mode: Default::default(),
            draft: String::new(),
            idle_timeout: default_idle_timeout(),
        }
    }

    pub fn load(root: &Path) -> Result<Self> {
        let path = paths(root).config;
        let bytes =
            fs::read(&path).with_context(|| format!("could not read {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not valid agent configuration", path.display()))
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        write_config(root, &serde_json::to_value(self)?)
    }
}

pub fn update(root: &Path, edit: impl FnOnce(&mut serde_json::Value) -> Result<()>) -> Result<()> {
    static WRITER: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = WRITER
        .lock()
        .map_err(|_| anyhow::anyhow!("agent config writer stopped"))?;
    let mut config = serde_json::from_slice(&fs::read(paths(root).config)?)?;
    edit(&mut config)?;
    write_config(root, &config)
}

pub fn save_permissions(root: &Path, permissions: &[crate::permissions::Approval]) -> Result<()> {
    update(root, |config| {
        config["permissions"] = serde_json::to_value(permissions)?;
        Ok(())
    })
}

pub fn save_permission_mode(root: &Path, mode: crate::permissions::Mode) -> Result<()> {
    update(root, |config| {
        config["permission_mode"] = serde_json::to_value(mode)?;
        Ok(())
    })
}

pub fn save_draft(root: &Path, draft: &str) -> Result<()> {
    update(root, |config| {
        config["draft"] = draft.into();
        Ok(())
    })
}

fn write_config(root: &Path, config: &serde_json::Value) -> Result<()> {
    let path = paths(root).config;
    let json = serde_json::to_vec_pretty(config)?;
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
    fn saves_configuration_without_leaving_a_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(STATE_DIR)).unwrap();
        let config = AgentConfig::new("Agent", "agent", "Test", "codex");

        config.save(root.path()).unwrap();

        assert_eq!(AgentConfig::load(root.path()).unwrap().name, "Agent");
        let mut saved = serde_json::to_value(&config).unwrap();
        assert_eq!(saved["idle_timeout"], 300);
        saved.as_object_mut().unwrap().remove("idle_timeout");
        assert_eq!(
            serde_json::from_value::<AgentConfig>(saved.clone())
                .unwrap()
                .idle_timeout
                .get(),
            300
        );
        saved["idle_timeout"] = 0.into();
        assert!(serde_json::from_value::<AgentConfig>(saved).is_err());
        assert_eq!(fs::read_dir(paths(root.path()).state).unwrap().count(), 1);
    }
}
