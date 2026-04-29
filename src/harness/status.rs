use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::harness::drift::DriftReport;
use crate::harness::integration::{Detection, HarnessIntegration, LoadedProfile, RuntimeEnv};
use crate::harness::kind::HarnessKind;
use crate::harness::registry;
use crate::profile::{ProfileName, ProfileStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusRow {
    pub harness: HarnessKind,
    pub profile: StatusProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusProfile {
    Inactive,
    Active {
        name: ProfileName,
        drift: DriftState,
        has_validation_errors: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftState {
    Clean,
    Drifted,
    Error,
}

pub fn status_rows(env: &RuntimeEnv, profile_store: &ProfileStore) -> Result<Vec<StatusRow>> {
    status_rows_for(env, profile_store, registry::all())
}

pub fn status_rows_for(
    env: &RuntimeEnv,
    profile_store: &ProfileStore,
    integrations: Vec<Box<dyn HarnessIntegration>>,
) -> Result<Vec<StatusRow>> {
    let state = LazyagentsState::load(&env.lazyagents_home.join("state.json"))?;
    let mut rows = Vec::new();

    for integration in integrations {
        if !matches!(integration.detect(env)?, Detection::Detected { .. }) {
            continue;
        }

        let profile = match state.active_profiles.get(integration.kind().id()) {
            Some(name) => {
                let name = ProfileName::parse(name.clone())?;
                let loaded = LoadedProfile {
                    path: profile_store.profile_dir(&name),
                    name: name.clone(),
                };

                let has_validation_errors = if loaded.path.exists() {
                    let issues = crate::profile::validation::validate_profile(&loaded.path);
                    issues
                        .iter()
                        .any(|i| i.severity == crate::profile::validation::Severity::Error)
                } else {
                    true
                };

                let drift = match integration.paths(env).and_then(|paths| {
                    if !loaded.path.exists() {
                        anyhow::bail!("Active profile directory is missing");
                    }
                    if !loaded.path.join("config.json").exists() {
                        anyhow::bail!("Active profile missing config.json");
                    }
                    if !loaded.path.join("AGENTS.md").exists() {
                        anyhow::bail!("Active profile missing instruction source file");
                    }
                    integration
                        .detect_drift(&loaded, &paths)
                        .map(|report| drift_state(&report))
                }) {
                    Ok(state) => state,
                    Err(_) => DriftState::Error,
                };
                StatusProfile::Active {
                    name,
                    drift,
                    has_validation_errors,
                }
            }
            None => StatusProfile::Inactive,
        };

        rows.push(StatusRow {
            harness: integration.kind(),
            profile,
        });
    }

    rows.sort_by_key(|row| row.harness);
    Ok(rows)
}

fn drift_state(report: &DriftReport) -> DriftState {
    if report.is_clean() {
        DriftState::Clean
    } else {
        DriftState::Drifted
    }
}

#[derive(Debug, Default, Deserialize)]
struct LazyagentsState {
    #[serde(default)]
    active_profiles: BTreeMap<String, String>,
}

impl LazyagentsState {
    fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read state file at {}", path.display()));
            }
        };

        serde_json::from_str(&text)
            .with_context(|| format!("invalid state file at {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use anyhow::{anyhow, Result};

    use super::*;
    use crate::harness::drift::DriftItem;
    use crate::harness::integration::{HarnessPaths, PreferenceImport, ProfileImport};
    use crate::harness::managed::ManagedSurface;
    use crate::profile::LazyagentsHome;

    struct StatusIntegration {
        kind: HarnessKind,
        detected: bool,
        drift: Result<DriftReport, &'static str>,
    }

    impl StatusIntegration {
        fn detected(kind: HarnessKind, drift: DriftReport) -> Self {
            Self {
                kind,
                detected: true,
                drift: Ok(drift),
            }
        }

        fn undetected(kind: HarnessKind) -> Self {
            Self {
                kind,
                detected: false,
                drift: Ok(DriftReport::clean()),
            }
        }

        fn detected_error(kind: HarnessKind) -> Self {
            Self {
                kind,
                detected: true,
                drift: Err("drift failed"),
            }
        }
    }

    impl HarnessIntegration for StatusIntegration {
        fn kind(&self) -> HarnessKind {
            self.kind
        }

        fn detect(&self, _env: &RuntimeEnv) -> Result<Detection> {
            if self.detected {
                Ok(Detection::Detected {
                    binary_path: PathBuf::from("/bin/fake"),
                })
            } else {
                Ok(Detection::NotDetected)
            }
        }

        fn paths(&self, env: &RuntimeEnv) -> Result<HarnessPaths> {
            let root = env.user_home.join(self.kind.id());
            Ok(HarnessPaths {
                config_dir: root.clone(),
                instruction_target: root.join("AGENTS.md"),
                skills_dir: root.join("skills"),
                commands_dir: root.join("commands"),
                settings_file: root.join("settings.json"),
                mcp_file: root.join("mcp.json"),
            })
        }

        fn managed_surfaces(&self, _paths: &HarnessPaths) -> Vec<ManagedSurface> {
            Vec::new()
        }

        fn preflight(&self, _profile: &LoadedProfile) -> Result<()> {
            Ok(())
        }

        fn detect_drift(
            &self,
            _active: &LoadedProfile,
            _paths: &HarnessPaths,
        ) -> Result<DriftReport> {
            match &self.drift {
                Ok(report) => Ok(report.clone()),
                Err(error) => Err(anyhow!(*error)),
            }
        }

        fn import_from_harness(&self, _paths: &HarnessPaths) -> Result<ProfileImport> {
            Ok(ProfileImport {
                instruction: None,
                skills: Vec::new(),
                commands: Vec::new(),
                mcp_definitions: None,
                model_preference: PreferenceImport::default_value(),
                permission_preference: PreferenceImport::default_value(),
            })
        }

        fn apply(&self, _profile: &LoadedProfile, _paths: &HarnessPaths) -> Result<()> {
            Ok(())
        }

        fn verify(&self, _profile: &LoadedProfile, _paths: &HarnessPaths) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn status_is_detected_only_and_reports_inline_drift_state() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("lazyagents");
        let store = ProfileStore::new(LazyagentsHome::from_path(&home));
        let work = ProfileName::parse("work").unwrap();
        let play = ProfileName::parse("play").unwrap();
        store.create_skeleton(&work).unwrap();
        store.create_skeleton(&play).unwrap();
        fs::write(
            home.join("state.json"),
            r#"{"active_profiles":{"codex":"work","claude":"play"}}"#,
        )
        .unwrap();
        let env = RuntimeEnv {
            lazyagents_home: home,
            user_home: temp.path().join("user"),
            path_entries: Vec::new(),
        };

        let rows = status_rows_for(
            &env,
            &store,
            vec![
                Box::new(StatusIntegration::detected(
                    HarnessKind::Codex,
                    DriftReport::clean(),
                )),
                Box::new(StatusIntegration::undetected(HarnessKind::Claude)),
                Box::new(StatusIntegration::detected(
                    HarnessKind::OpenCode,
                    DriftReport {
                        items: vec![DriftItem {
                            surface: "mcp".to_string(),
                            detail: "changed".to_string(),
                        }],
                    },
                )),
            ],
        )
        .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].profile,
            StatusProfile::Active {
                name: work,
                drift: DriftState::Clean,
                has_validation_errors: false,
            }
        );
        assert_eq!(rows[1].profile, StatusProfile::Inactive);
    }

    #[test]
    fn status_reports_error_when_drift_check_fails() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("lazyagents");
        let store = ProfileStore::new(LazyagentsHome::from_path(&home));
        let work = ProfileName::parse("work").unwrap();
        store.create_skeleton(&work).unwrap();
        fs::write(
            home.join("state.json"),
            r#"{"active_profiles":{"codex":"work"}}"#,
        )
        .unwrap();
        let env = RuntimeEnv {
            lazyagents_home: home,
            user_home: temp.path().join("user"),
            path_entries: Vec::new(),
        };

        let rows = status_rows_for(
            &env,
            &store,
            vec![Box::new(StatusIntegration::detected_error(
                HarnessKind::Codex,
            ))],
        )
        .unwrap();

        assert_eq!(
            rows[0].profile,
            StatusProfile::Active {
                name: work,
                drift: DriftState::Error,
                has_validation_errors: false,
            }
        );
    }

    #[test]
    fn status_reports_error_when_active_profile_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("lazyagents");
        let store = ProfileStore::new(LazyagentsHome::from_path(&home));
        let work = ProfileName::parse("work").unwrap();

        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("state.json"),
            r#"{"active_profiles":{"codex":"work"}}"#,
        )
        .unwrap();
        let env = RuntimeEnv {
            lazyagents_home: home,
            user_home: temp.path().join("user"),
            path_entries: Vec::new(),
        };

        let rows = status_rows_for(
            &env,
            &store,
            vec![Box::new(StatusIntegration::detected(
                HarnessKind::Codex,
                DriftReport::clean(),
            ))],
        )
        .unwrap();

        assert_eq!(
            rows[0].profile,
            StatusProfile::Active {
                name: work,
                drift: DriftState::Error,
                has_validation_errors: true,
            }
        );
    }
}
