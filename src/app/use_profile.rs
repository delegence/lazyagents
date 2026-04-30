use anyhow::Result;

use crate::app::harness_registry::HarnessRegistry;
use crate::app::state::LazyagentsState;
use crate::harness::apply::{
    apply_profile_to_harness_with_commit, ProfileUseResult, ProfileUseStatus,
};
use crate::harness::drift::DriftReport;
use crate::harness::integration::{
    AppEnvironment, HarnessDetection, HarnessIntegration, ProfileRef,
};
use crate::harness::kind::HarnessKind;
use crate::profile::{ProfileName, ProfileStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftDecision {
    SaveChanges,
    DiscardChanges,
    Cancel,
}

pub enum UseProfileTarget {
    Harness(HarnessKind),
    All,
}

pub struct UseProfileRequest {
    pub profile: ProfileName,
    pub target: UseProfileTarget,
    pub drift_decision: Option<DriftDecision>,
}

pub enum UseProfileOutcome {
    Applied(ProfileUseResult),
    All(UseProfileAllResult),
    NeedsSingleHarnessDriftDecision {
        harness: HarnessKind,
        drift: DriftReport,
    },
    NeedsAllHarnessDriftDecision {
        harnesses: Vec<HarnessKind>,
    },
}

pub struct UseProfileAllResult {
    pub applied: Vec<ProfileUseResult>,
    pub failures: Vec<(HarnessKind, anyhow::Error)>,
}

pub fn use_profile_workflow(
    registry: &dyn HarnessRegistry,
    env: &AppEnvironment,
    store: &ProfileStore,
    request: UseProfileRequest,
) -> Result<UseProfileOutcome> {
    match request.target {
        UseProfileTarget::Harness(kind) => {
            let integration = registry
                .get(kind)
                .ok_or_else(|| anyhow::anyhow!("unsupported harness {kind}"))?;
            match integration.detect(env)? {
                HarnessDetection::Detected { .. } => {}
                HarnessDetection::NotDetected => anyhow::bail!("{kind} was not detected on PATH"),
            }

            let decision = match request.drift_decision {
                Some(decision) => decision,
                None => {
                    let drift = active_drift(integration.as_ref(), env, store)?;
                    if drift.is_clean() {
                        DriftDecision::DiscardChanges
                    } else {
                        return Ok(UseProfileOutcome::NeedsSingleHarnessDriftDecision {
                            harness: kind,
                            drift,
                        });
                    }
                }
            };

            Ok(UseProfileOutcome::Applied(apply_one(
                integration.as_ref(),
                env,
                store,
                &request.profile,
                decision,
            )?))
        }
        UseProfileTarget::All => {
            if matches!(request.drift_decision, Some(DriftDecision::SaveChanges)) {
                anyhow::bail!("--save-changes cannot be used with --all");
            }

            let detected = detected_integrations(registry, env)?;
            if detected.is_empty() {
                anyhow::bail!("no supported harnesses detected");
            }

            if request.drift_decision.is_none() {
                let mut drifted = Vec::new();
                for integration in &detected {
                    let drift = active_drift(integration.as_ref(), env, store)?;
                    if !drift.is_clean() {
                        drifted.push(integration.kind());
                    }
                }
                if !drifted.is_empty() {
                    return Ok(UseProfileOutcome::NeedsAllHarnessDriftDecision {
                        harnesses: drifted,
                    });
                }
            }

            let mut results = UseProfileAllResult {
                applied: Vec::new(),
                failures: Vec::new(),
            };
            for integration in detected {
                match apply_one(
                    integration.as_ref(),
                    env,
                    store,
                    &request.profile,
                    DriftDecision::DiscardChanges,
                ) {
                    Ok(result) => results.applied.push(result),
                    Err(error) => results.failures.push((integration.kind(), error)),
                }
            }
            Ok(UseProfileOutcome::All(results))
        }
    }
}

fn apply_one(
    integration: &dyn HarnessIntegration,
    env: &AppEnvironment,
    store: &ProfileStore,
    profile: &ProfileName,
    decision: DriftDecision,
) -> Result<ProfileUseResult> {
    store.normalize_optional_artifacts(profile)?;
    let paths = integration.paths(env)?;
    let target_profile = ProfileRef {
        name: profile.clone(),
        path: store.profile_dir(profile),
    };
    integration.preflight(&target_profile)?;

    let state_path = env.lazyagents_home.join("state.json");
    let mut state = LazyagentsState::load(&state_path)?;
    let active_profile = state
        .active_profiles
        .get(&integration.kind())
        .map(|name| ProfileRef {
            name: name.clone(),
            path: store.profile_dir(name),
        });
    let drift = match active_profile.as_ref() {
        Some(active) => integration.detect_drift(active, &paths)?,
        None => DriftReport::clean(),
    };

    if !drift.is_clean() {
        match decision {
            DriftDecision::Cancel => {
                return Ok(ProfileUseResult {
                    harness: integration.kind(),
                    profile: profile.clone(),
                    status: ProfileUseStatus::CancelledForDrift,
                });
            }
            DriftDecision::DiscardChanges => {}
            DriftDecision::SaveChanges => {
                let imported = integration.import_from_harness(&paths)?;
                if let Some(active) = active_profile.as_ref() {
                    store.apply_import(&active.name, integration.kind(), imported)?;
                }
            }
        }
    }

    apply_profile_to_harness_with_commit(integration, env, store, profile, || {
        state
            .active_profiles
            .insert(integration.kind(), profile.clone());
        state.save(&state_path)
    })?;
    Ok(ProfileUseResult {
        harness: integration.kind(),
        profile: profile.clone(),
        status: ProfileUseStatus::Applied,
    })
}

fn active_drift(
    integration: &dyn HarnessIntegration,
    env: &AppEnvironment,
    store: &ProfileStore,
) -> Result<DriftReport> {
    let paths = integration.paths(env)?;
    let state = LazyagentsState::load(&env.lazyagents_home.join("state.json"))?;
    let active_profile = state
        .active_profiles
        .get(&integration.kind())
        .map(|name| ProfileRef {
            name: name.clone(),
            path: store.profile_dir(name),
        });
    match active_profile.as_ref() {
        Some(active) => integration.detect_drift(active, &paths),
        None => Ok(DriftReport::clean()),
    }
}

fn detected_integrations(
    registry: &dyn HarnessRegistry,
    env: &AppEnvironment,
) -> Result<Vec<Box<dyn HarnessIntegration>>> {
    let mut detected = Vec::new();
    for integration in registry.all() {
        if matches!(integration.detect(env)?, HarnessDetection::Detected { .. }) {
            detected.push(integration);
        }
    }
    Ok(detected)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::path::PathBuf;

    use anyhow::Result;

    use super::*;
    use crate::harness::drift::DriftItem;
    use crate::harness::integration::{HarnessConfigPaths, ImportedPreference, ProfileImport};
    use crate::harness::managed::ManagedSurface;
    use crate::profile::LazyagentsHome;

    #[derive(Clone)]
    struct FakeIntegration {
        kind: HarnessKind,
        root: PathBuf,
        detected: bool,
        drift: DriftReport,
        fail_apply: bool,
        replace_state_with_dir: bool,
        import_called: Cell<bool>,
    }

    impl FakeIntegration {
        fn new(kind: HarnessKind, root: PathBuf) -> Self {
            Self {
                kind,
                root,
                detected: true,
                drift: DriftReport::clean(),
                fail_apply: false,
                replace_state_with_dir: false,
                import_called: Cell::new(false),
            }
        }

        fn with_drift(mut self) -> Self {
            self.drift = DriftReport {
                items: vec![DriftItem {
                    surface: "instructions".to_string(),
                    detail: "changed".to_string(),
                }],
            };
            self
        }

        fn undetected(mut self) -> Self {
            self.detected = false;
            self
        }

        fn fail_apply(mut self) -> Self {
            self.fail_apply = true;
            self
        }

        fn replace_state_with_dir_on_apply(mut self) -> Self {
            self.replace_state_with_dir = true;
            self
        }
    }

    impl HarnessIntegration for FakeIntegration {
        fn kind(&self) -> HarnessKind {
            self.kind
        }

        fn detect(&self, _env: &AppEnvironment) -> Result<HarnessDetection> {
            if self.detected {
                Ok(HarnessDetection::Detected {
                    binary_path: self.root.join("bin").join("fake"),
                })
            } else {
                Ok(HarnessDetection::NotDetected)
            }
        }

        fn paths(&self, _env: &AppEnvironment) -> Result<HarnessConfigPaths> {
            Ok(HarnessConfigPaths {
                config_dir: self.root.clone(),
                instruction_target: self.root.join("AGENTS.md"),
                skills_dir: self.root.join("skills"),
                commands_dir: self.root.join("commands"),
                settings_file: self.root.join("settings.json"),
                mcp_file: self.root.join("mcp.json"),
            })
        }

        fn managed_surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface> {
            vec![
                ManagedSurface::file(&paths.instruction_target),
                ManagedSurface::directory(&paths.skills_dir),
                ManagedSurface::directory(&paths.commands_dir),
            ]
        }

        fn preflight(&self, _profile: &ProfileRef) -> Result<()> {
            Ok(())
        }

        fn detect_drift(
            &self,
            _active: &ProfileRef,
            _paths: &HarnessConfigPaths,
        ) -> Result<DriftReport> {
            Ok(self.drift.clone())
        }

        fn import_from_harness(&self, _paths: &HarnessConfigPaths) -> Result<ProfileImport> {
            self.import_called.set(true);
            Ok(ProfileImport {
                instruction: Some("saved drift".to_string()),
                skills: Vec::new(),
                commands: Vec::new(),
                mcp_definitions: None,
                model_preference: ImportedPreference::new(serde_json::json!("saved-model")),
                permission_preference: ImportedPreference::default_value(),
            })
        }

        fn apply(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
            if self.fail_apply {
                anyhow::bail!("apply failed");
            }
            fs::create_dir_all(&paths.config_dir)?;
            fs::create_dir_all(&paths.skills_dir)?;
            fs::create_dir_all(&paths.commands_dir)?;
            fs::write(
                &paths.instruction_target,
                format!("profile={}", profile.name.as_str()),
            )?;
            if self.replace_state_with_dir {
                let state_path = paths
                    .config_dir
                    .parent()
                    .and_then(|path| path.parent())
                    .unwrap()
                    .join("lazyagents/state.json");
                let _ = fs::remove_file(&state_path);
                fs::create_dir_all(&state_path)?;
            }
            Ok(())
        }

        fn verify(&self, _profile: &ProfileRef, _paths: &HarnessConfigPaths) -> Result<()> {
            Ok(())
        }
    }

    struct FakeCatalog {
        integrations: Vec<FakeIntegration>,
    }

    impl HarnessRegistry for FakeCatalog {
        fn all(&self) -> Vec<Box<dyn HarnessIntegration>> {
            self.integrations
                .iter()
                .cloned()
                .map(|integration| Box::new(integration) as Box<dyn HarnessIntegration>)
                .collect()
        }

        fn get(&self, kind: HarnessKind) -> Option<Box<dyn HarnessIntegration>> {
            self.integrations
                .iter()
                .find(|integration| integration.kind == kind)
                .cloned()
                .map(|integration| Box::new(integration) as Box<dyn HarnessIntegration>)
        }
    }

    #[test]
    fn returns_single_harness_drift_decision_request() {
        let fixture = Fixture::new();
        fixture.profile("active");
        fixture.profile("target");
        fixture.write_state(r#"{"active_profiles":{"codex":"active"}}"#);
        let registry = FakeCatalog {
            integrations: vec![
                FakeIntegration::new(HarnessKind::Codex, fixture.harness("codex")).with_drift(),
            ],
        };

        let outcome = use_profile_workflow(
            &registry,
            &fixture.env,
            &fixture.store,
            UseProfileRequest {
                profile: ProfileName::parse("target").unwrap(),
                target: UseProfileTarget::Harness(HarnessKind::Codex),
                drift_decision: None,
            },
        )
        .unwrap();

        assert!(matches!(
            outcome,
            UseProfileOutcome::NeedsSingleHarnessDriftDecision {
                harness: HarnessKind::Codex,
                ..
            }
        ));
    }

    #[test]
    fn save_changes_imports_active_drift_before_applying_target() {
        let fixture = Fixture::new();
        fixture.profile("active");
        fixture.profile("target");
        fixture.write_state(r#"{"active_profiles":{"codex":"active"}}"#);
        let integration =
            FakeIntegration::new(HarnessKind::Codex, fixture.harness("codex")).with_drift();
        let registry = FakeCatalog {
            integrations: vec![integration],
        };

        let outcome = use_profile_workflow(
            &registry,
            &fixture.env,
            &fixture.store,
            UseProfileRequest {
                profile: ProfileName::parse("target").unwrap(),
                target: UseProfileTarget::Harness(HarnessKind::Codex),
                drift_decision: Some(DriftDecision::SaveChanges),
            },
        )
        .unwrap();

        assert!(matches!(outcome, UseProfileOutcome::Applied(_)));
        assert_eq!(
            fs::read_to_string(fixture.profile_path("active").join("AGENTS.md")).unwrap(),
            "saved drift"
        );
        let config = fixture
            .store
            .load_config(&ProfileName::parse("active").unwrap())
            .unwrap();
        assert_eq!(config.model_preference(HarnessKind::Codex), "saved-model");
    }

    #[test]
    fn state_save_failure_rolls_back_harness_surfaces() {
        let fixture = Fixture::new();
        fixture.profile("work");
        fs::create_dir_all(fixture.harness("codex").join("skills")).unwrap();
        fs::write(fixture.harness("codex").join("AGENTS.md"), "previous").unwrap();
        fs::write(fixture.harness("codex").join("skills/old.txt"), "old").unwrap();
        fixture.write_state(r#"{"active_profiles":{}}"#);
        let registry = FakeCatalog {
            integrations: vec![
                FakeIntegration::new(HarnessKind::Codex, fixture.harness("codex"))
                    .replace_state_with_dir_on_apply(),
            ],
        };

        let error = match use_profile_workflow(
            &registry,
            &fixture.env,
            &fixture.store,
            UseProfileRequest {
                profile: ProfileName::parse("work").unwrap(),
                target: UseProfileTarget::Harness(HarnessKind::Codex),
                drift_decision: Some(DriftDecision::DiscardChanges),
            },
        ) {
            Ok(_) => panic!("expected state save failure"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("state.json"));
        assert_eq!(
            fs::read_to_string(fixture.harness("codex").join("AGENTS.md")).unwrap(),
            "previous"
        );
        assert_eq!(
            fs::read_to_string(fixture.harness("codex").join("skills/old.txt")).unwrap(),
            "old"
        );
    }

    #[test]
    fn use_all_rejects_save_and_continues_after_failures() {
        let fixture = Fixture::new();
        fixture.profile("work");
        let registry = FakeCatalog {
            integrations: vec![
                FakeIntegration::new(HarnessKind::Codex, fixture.harness("codex")),
                FakeIntegration::new(HarnessKind::Claude, fixture.harness("claude")).fail_apply(),
                FakeIntegration::new(HarnessKind::OpenCode, fixture.harness("opencode"))
                    .undetected(),
            ],
        };

        let save_error = match use_profile_workflow(
            &registry,
            &fixture.env,
            &fixture.store,
            UseProfileRequest {
                profile: ProfileName::parse("work").unwrap(),
                target: UseProfileTarget::All,
                drift_decision: Some(DriftDecision::SaveChanges),
            },
        ) {
            Ok(_) => panic!("expected --save-changes with --all to fail"),
            Err(error) => error,
        };
        assert!(save_error.to_string().contains("--save-changes"));

        let outcome = use_profile_workflow(
            &registry,
            &fixture.env,
            &fixture.store,
            UseProfileRequest {
                profile: ProfileName::parse("work").unwrap(),
                target: UseProfileTarget::All,
                drift_decision: Some(DriftDecision::DiscardChanges),
            },
        )
        .unwrap();

        let UseProfileOutcome::All(results) = outcome else {
            panic!("expected all result");
        };
        assert_eq!(results.applied.len(), 1);
        assert_eq!(results.applied[0].harness, HarnessKind::Codex);
        assert_eq!(results.failures.len(), 1);
        assert_eq!(results.failures[0].0, HarnessKind::Claude);
        let state = LazyagentsState::load(&fixture.home.join("state.json")).unwrap();
        assert_eq!(
            state
                .active_profiles
                .get(&HarnessKind::Codex)
                .unwrap()
                .as_str(),
            "work"
        );
        assert!(!state.active_profiles.contains_key(&HarnessKind::Claude));
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        home: PathBuf,
        env: AppEnvironment,
        store: ProfileStore,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("lazyagents");
            let env = AppEnvironment {
                lazyagents_home: home.clone(),
                user_home: temp.path().join("user"),
                path_entries: Vec::new(),
            };
            let store = ProfileStore::new(LazyagentsHome::from_path(&home));
            Self {
                _temp: temp,
                home,
                env,
                store,
            }
        }

        fn profile(&self, name: &str) {
            self.store
                .create_skeleton(&ProfileName::parse(name).unwrap())
                .unwrap();
        }

        fn profile_path(&self, name: &str) -> PathBuf {
            self.home.join("profiles").join(name)
        }

        fn harness(&self, name: &str) -> PathBuf {
            self.env.user_home.join(name)
        }

        fn write_state(&self, contents: &str) {
            fs::create_dir_all(&self.home).unwrap();
            fs::write(self.home.join("state.json"), contents).unwrap();
        }
    }
}
