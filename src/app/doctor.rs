use anyhow::Result;

use crate::app::harness_registry::HarnessRegistry;
use crate::app::state::LazyagentsState;
use crate::harness::drift::DriftReport;
use crate::harness::integration::{
    AppEnvironment, HarnessDetection, HarnessIntegration, ProfileRef,
};
use crate::profile::{ProfileConfigStatus, ProfileName, ProfileStore};

pub struct DoctorReport {
    pub harnesses: Vec<HarnessStatus>,
    pub profiles: ProfileDoctorReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessStatus {
    pub harness: String,
    pub display_name: String,
    pub harness_type: String,
    pub config_dir: std::path::PathBuf,
    pub config_dir_exists: bool,
    pub shared_config_with: Option<String>,
    pub profile: HarnessProfileStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessProfileStatus {
    Inactive,
    Active {
        name: ProfileName,
        drift: DriftStatus,
        has_validation_errors: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftStatus {
    Clean,
    Drifted,
    Error,
}

pub struct ProfileDoctorReport {
    pub marker: &'static str,
    pub summary: Vec<String>,
    pub lines: Vec<String>,
}

pub fn doctor_report(
    registry: &dyn HarnessRegistry,
    env: &AppEnvironment,
    store: &ProfileStore,
) -> Result<DoctorReport> {
    let harnesses = status_rows_for(env, store, registry.all(env)?)?;
    let profiles = profile_doctor_report(store, &harnesses)?;
    Ok(DoctorReport {
        harnesses,
        profiles,
    })
}

fn status_rows_for(
    env: &AppEnvironment,
    profile_store: &ProfileStore,
    integrations: Vec<Box<dyn HarnessIntegration>>,
) -> Result<Vec<HarnessStatus>> {
    let state = LazyagentsState::load(&env.lazyagents_home.join("state.json"))?;
    let mut rows = Vec::new();

    for integration in integrations {
        if !matches!(integration.detect(env)?, HarnessDetection::Detected { .. }) {
            continue;
        }

        let profile = match state.active_profiles.get(integration.instance_id()) {
            Some(name) => {
                let loaded = ProfileRef {
                    name: name.clone(),
                    path: profile_store.profile_dir(name),
                    harness_id: integration.instance_id().to_string(),
                };

                let has_validation_errors = if loaded.path.exists() {
                    has_relevant_validation_errors(integration.as_ref(), &loaded.path)
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
                    Err(_) => DriftStatus::Error,
                };
                HarnessProfileStatus::Active {
                    name: name.clone(),
                    drift,
                    has_validation_errors,
                }
            }
            None => HarnessProfileStatus::Inactive,
        };

        let config_dir = integration.paths(env)?.config_dir;
        let config_dir_exists = config_dir.is_dir();
        rows.push(HarnessStatus {
            harness: integration.instance_id().to_string(),
            display_name: integration.display_name().to_string(),
            harness_type: integration.kind().id().to_string(),
            config_dir,
            config_dir_exists,
            shared_config_with: None,
            profile,
        });
    }

    rows.sort_by(|left, right| left.harness.cmp(&right.harness));
    let mut first_by_config: std::collections::BTreeMap<(String, String), String> =
        std::collections::BTreeMap::new();
    for row in &mut rows {
        let key = (
            row.harness_type.clone(),
            row.config_dir.display().to_string(),
        );
        if let Some(first) = first_by_config.get(&key) {
            row.shared_config_with = Some(first.clone());
        } else {
            first_by_config.insert(key, row.harness.clone());
        }
    }
    Ok(rows)
}

fn has_relevant_validation_errors(
    integration: &dyn HarnessIntegration,
    path: &std::path::Path,
) -> bool {
    crate::profile::validation::validate_profile(path)
        .iter()
        .any(|issue| {
            issue.severity == crate::profile::validation::Severity::Error
                && match issue.category.as_str() {
                    "Skills" => integration.supports_skills(),
                    "Commands" => integration.supports_commands(),
                    "MCP" => integration.supports_mcp(),
                    _ => true,
                }
        })
}

fn profile_doctor_report(
    store: &ProfileStore,
    rows: &[HarnessStatus],
) -> Result<ProfileDoctorReport> {
    let profiles = store.list_profiles()?;
    let mut lines = Vec::new();
    let mut drifted = 0usize;
    let mut errors = 0usize;

    for profile in profiles {
        let mut states = Vec::new();
        let mut clean = Vec::new();
        let mut drift = Vec::new();
        let mut error = Vec::new();

        for row in rows {
            let HarnessProfileStatus::Active {
                name,
                drift: drift_state,
                has_validation_errors,
            } = &row.profile
            else {
                continue;
            };
            if name != &profile.name {
                continue;
            }

            match drift_state {
                DriftStatus::Clean => clean.push(row.harness.clone()),
                DriftStatus::Drifted => drift.push(row.harness.clone()),
                DriftStatus::Error => error.push(row.harness.clone()),
            }
            if *has_validation_errors && !error.contains(&row.harness) {
                error.push(row.harness.clone());
            }
        }

        drifted += drift.len();
        errors += error.len();

        let validation_error =
            profile_validation_error(store, &profile.name, &profile.config_status);
        if validation_error.is_some() {
            errors += 1;
        }

        if !clean.is_empty() {
            states.push(format!("used by {}", clean.join(", ")));
        }
        if !drift.is_empty() {
            states.push(format!("drifted by {}", drift.join(", ")));
        }
        if !error.is_empty() {
            states.push(format!("error: {}", error.join(", ")));
        }
        if let Some(error) = validation_error {
            states.push(format!("invalid: {error}"));
        }
        if states.is_empty() {
            states.push("unused".to_string());
        }

        lines.push(format!("  - {} ({})", profile.name, states.join(", ")));
    }

    let marker = if drifted == 0 && errors == 0 {
        "[✓]"
    } else {
        "[!]"
    };
    let mut summary = Vec::new();
    if drifted > 0 {
        summary.push(format!("{drifted} drifted"));
    }
    if errors > 0 {
        summary.push(format!("{errors} error"));
    }

    Ok(ProfileDoctorReport {
        marker,
        summary,
        lines,
    })
}

fn profile_validation_error(
    store: &ProfileStore,
    name: &ProfileName,
    config_status: &ProfileConfigStatus,
) -> Option<String> {
    match config_status {
        ProfileConfigStatus::Valid => {
            let path = store.profile_dir(name);
            let issues = crate::profile::validation::validate_profile(&path);
            issues
                .into_iter()
                .find(|issue| issue.severity == crate::profile::validation::Severity::Error)
                .map(|issue| issue.message)
        }
        ProfileConfigStatus::Missing => Some("missing config.json".to_string()),
        ProfileConfigStatus::Invalid(error) => Some(format!("config.json {error}")),
    }
}

fn drift_state(report: &DriftReport) -> DriftStatus {
    if report.is_clean() {
        DriftStatus::Clean
    } else {
        DriftStatus::Drifted
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use anyhow::{anyhow, Result};

    use super::*;
    use crate::harness::drift::DriftItem;
    use crate::harness::integration::{HarnessConfigPaths, ImportedPreference, ProfileImport};
    use crate::harness::kind::HarnessKind;
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

        fn default_config_dir(&self, env: &AppEnvironment) -> PathBuf {
            env.user_home.join(self.kind.id())
        }

        fn paths_from_config_dir(&self, root: PathBuf) -> Result<HarnessConfigPaths> {
            Ok(HarnessConfigPaths {
                config_dir: root.clone(),
                instruction_target: root.join("AGENTS.md"),
                skills_dir: root.join("skills"),
                commands_dir: root.join("commands"),
                settings_file: root.join("settings.json"),
                mcp_file: root.join("mcp.json"),
            })
        }

        fn detect(&self, _env: &AppEnvironment) -> Result<HarnessDetection> {
            if self.detected {
                Ok(HarnessDetection::Detected {
                    binary_path: PathBuf::from("/bin/fake"),
                })
            } else {
                Ok(HarnessDetection::NotDetected)
            }
        }

        fn paths(&self, env: &AppEnvironment) -> Result<HarnessConfigPaths> {
            self.paths_from_config_dir(self.default_config_dir(env))
        }

        fn managed_surfaces(&self, _paths: &HarnessConfigPaths) -> Vec<ManagedSurface> {
            Vec::new()
        }

        fn preflight(&self, _profile: &ProfileRef) -> Result<()> {
            Ok(())
        }

        fn detect_drift(
            &self,
            _active: &ProfileRef,
            _paths: &HarnessConfigPaths,
        ) -> Result<DriftReport> {
            match &self.drift {
                Ok(report) => Ok(report.clone()),
                Err(error) => Err(anyhow!(*error)),
            }
        }

        fn import_from_harness(&self, _paths: &HarnessConfigPaths) -> Result<ProfileImport> {
            Ok(ProfileImport {
                instruction: None,
                skills: Vec::new(),
                commands: Vec::new(),
                mcp_definitions: None,
                model_preference: ImportedPreference::default_value(),
                permission_preference: ImportedPreference::default_value(),
            })
        }

        fn apply(&self, _profile: &ProfileRef, _paths: &HarnessConfigPaths) -> Result<()> {
            Ok(())
        }

        fn verify(&self, _profile: &ProfileRef, _paths: &HarnessConfigPaths) -> Result<()> {
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
        let env = AppEnvironment {
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
            HarnessProfileStatus::Active {
                name: work,
                drift: DriftStatus::Clean,
                has_validation_errors: false,
            }
        );
        assert_eq!(rows[1].profile, HarnessProfileStatus::Inactive);
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
        let env = AppEnvironment {
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
            HarnessProfileStatus::Active {
                name: work,
                drift: DriftStatus::Error,
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
        let env = AppEnvironment {
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
            HarnessProfileStatus::Active {
                name: work,
                drift: DriftStatus::Error,
                has_validation_errors: true,
            }
        );
    }

    #[test]
    fn doctor_report_prepares_profile_lines_for_cli_rendering() {
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
        let env = AppEnvironment {
            lazyagents_home: home,
            user_home: temp.path().join("user"),
            path_entries: Vec::new(),
        };
        let registry = TestRegistry {
            integrations: vec![Box::new(StatusIntegration::detected(
                HarnessKind::Codex,
                DriftReport::clean(),
            ))],
        };

        let report = doctor_report(&registry, &env, &store).unwrap();

        assert_eq!(report.profiles.marker, "[✓]");
        assert_eq!(report.profiles.summary, Vec::<String>::new());
        assert_eq!(report.profiles.lines, vec!["  - work (used by codex)"]);
    }

    struct TestRegistry {
        integrations: Vec<Box<dyn HarnessIntegration>>,
    }

    impl HarnessRegistry for TestRegistry {
        fn all(&self, _env: &AppEnvironment) -> Result<Vec<Box<dyn HarnessIntegration>>> {
            Ok(self
                .integrations
                .iter()
                .map(|integration| {
                    Box::new(StatusIntegration::detected(
                        integration.kind(),
                        DriftReport::clean(),
                    )) as Box<dyn HarnessIntegration>
                })
                .collect())
        }
    }
}
