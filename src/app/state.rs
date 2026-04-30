use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::harness::kind::HarnessKind;
use crate::profile::ProfileName;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LazyagentsState {
    pub active_profiles: BTreeMap<HarnessKind, ProfileName>,
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
        "opencode" => Ok(HarnessKind::OpenCode),
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
}
