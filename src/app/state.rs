use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::harness::kind::HarnessKind;
use crate::profile::ProfileName;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LazyagentsState {
    pub active_profiles: BTreeMap<HarnessKind, ProfileName>,
}

#[derive(Debug)]
pub struct LazyagentsHomeLock {
    file: File,
}

impl LazyagentsHomeLock {
    pub fn acquire(home: &Path) -> Result<Self> {
        fs::create_dir_all(home)
            .with_context(|| format!("failed to create lazyagents home {}", home.display()))?;
        let path = home.join(".lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .with_context(|| format!("failed to open lock file {}", path.display()))?;

        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self { file }),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                anyhow::bail!("another lazyagents command is already running")
            }
            Err(error) => Err(error)
                .with_context(|| format!("failed to lock lazyagents home {}", home.display())),
        }
    }
}

impl Drop for LazyagentsHomeLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl LazyagentsState {
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read state file at {}", path.display()));
            }
        };

        let raw: RawState = serde_json::from_str(&text)
            .with_context(|| format!("invalid state file at {}", path.display()))?;
        raw.into_state()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let temp_path = temp_state_path(path);
        let raw = RawState::from_state(self);
        let text = serde_json::to_string_pretty(&raw)?;
        fs::write(&temp_path, format!("{text}\n"))
            .with_context(|| format!("failed to write {}", temp_path.display()))?;
        fs::rename(&temp_path, path)
            .with_context(|| format!("failed to save state file at {}", path.display()))
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct RawState {
    #[serde(default)]
    active_profiles: BTreeMap<String, String>,
}

impl RawState {
    fn into_state(self) -> Result<LazyagentsState> {
        let mut active_profiles = BTreeMap::new();
        for (harness, profile) in self.active_profiles {
            let kind = parse_harness_kind(&harness)?;
            let profile = ProfileName::parse(profile)?;
            active_profiles.insert(kind, profile);
        }
        Ok(LazyagentsState { active_profiles })
    }

    fn from_state(state: &LazyagentsState) -> Self {
        let active_profiles = state
            .active_profiles
            .iter()
            .map(|(harness, profile)| (harness.id().to_string(), profile.as_str().to_string()))
            .collect();
        Self { active_profiles }
    }
}

fn parse_harness_kind(id: &str) -> Result<HarnessKind> {
    match id {
        "codex" => Ok(HarnessKind::Codex),
        "claude" => Ok(HarnessKind::Claude),
        "gemini" => Ok(HarnessKind::Gemini),
        "opencode" => Ok(HarnessKind::OpenCode),
        "pi" => Ok(HarnessKind::Pi),
        other => anyhow::bail!("unknown harness id in state: {other}"),
    }
}

fn temp_state_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.json");
    path.with_file_name(format!(".{file_name}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_state_loads_empty() {
        let temp = tempfile::tempdir().unwrap();

        let state = LazyagentsState::load(&temp.path().join("state.json")).unwrap();

        assert!(state.active_profiles.is_empty());
    }

    #[test]
    fn typed_state_round_trips_to_string_ids() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state.json");
        let mut state = LazyagentsState::default();
        state
            .active_profiles
            .insert(HarnessKind::Codex, ProfileName::parse("work").unwrap());

        state.save(&path).unwrap();
        let loaded = LazyagentsState::load(&path).unwrap();

        assert_eq!(loaded, state);
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "{\n  \"active_profiles\": {\n    \"codex\": \"work\"\n  }\n}\n"
        );
    }

    #[test]
    fn lock_prevents_second_mutating_command() {
        let temp = tempfile::tempdir().unwrap();
        let _first = LazyagentsHomeLock::acquire(temp.path()).unwrap();

        let error = LazyagentsHomeLock::acquire(temp.path()).unwrap_err();

        assert!(error
            .to_string()
            .contains("another lazyagents command is already running"));
    }

    #[test]
    fn lock_is_released_on_drop() {
        let temp = tempfile::tempdir().unwrap();
        {
            let _first = LazyagentsHomeLock::acquire(temp.path()).unwrap();
        }

        let _second = LazyagentsHomeLock::acquire(temp.path()).unwrap();
    }
}
