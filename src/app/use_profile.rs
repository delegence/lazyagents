use anyhow::Result;

use crate::app::create_profile::{merge_shared_agent_skills, remove_imported_shared_skills};
use crate::app::harness_registry::HarnessRegistry;
use crate::app::state::LazyagentsState;
use crate::harness::apply::{
    apply_profile_to_harness_with_commit, ProfileUseResult, ProfileUseStatus,
};
use crate::harness::drift::DriftReport;
use crate::harness::integration::{
    AppEnvironment, HarnessDetection, HarnessIntegration, ProfileRef,
};
use crate::profile::mcp::read_mcp_definitions;
use crate::profile::{ProfileName, ProfileStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftDecision {
    SaveChanges,
    DiscardChanges,
    Cancel,
}

pub enum UseProfileTarget {
    Harness(String),
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
        display_name: String,
        profile: ProfileName,
        drift: DriftReport,
    },
    NeedsAllHarnessDriftDecision {
        harnesses: Vec<HarnessDrift>,
    },
}

pub struct HarnessDrift {
    pub display_name: String,
    pub profile: ProfileName,
    pub drift: DriftReport,
}

pub struct UseProfileAllResult {
    pub applied: Vec<ProfileUseResult>,
    pub failures: Vec<(String, String, anyhow::Error)>,
}

pub fn use_profile_workflow(
    registry: &dyn HarnessRegistry,
    env: &AppEnvironment,
    store: &ProfileStore,
    request: UseProfileRequest,
) -> Result<UseProfileOutcome> {
    match request.target {
        UseProfileTarget::Harness(id) => {
            let integration = registry
                .get(env, &id)?
                .ok_or_else(|| anyhow::anyhow!("unsupported harness {id}"))?;
            match integration.detect(env)? {
                HarnessDetection::Detected { .. } => {}
                HarnessDetection::NotDetected => {
                    anyhow::bail!("{} was not detected on PATH", integration.instance_id())
                }
            }

            let decision = match request.drift_decision {
                Some(decision) => decision,
                None => {
                    let active = active_drift(integration.as_ref(), env, store)?;
                    if active.drift.is_clean() {
                        DriftDecision::DiscardChanges
                    } else {
                        return Ok(UseProfileOutcome::NeedsSingleHarnessDriftDecision {
                            display_name: integration.display_name().to_string(),
                            profile: active.profile.ok_or_else(|| {
                                anyhow::anyhow!("drift reported without an active profile")
                            })?,
                            drift: active.drift,
                        });
                    }
                }
            };

            Ok(UseProfileOutcome::Applied(apply_one(
                registry,
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
                    let active = active_drift(integration.as_ref(), env, store)?;
                    if !active.drift.is_clean() {
                        drifted.push(HarnessDrift {
                            display_name: integration.display_name().to_string(),
                            profile: active.profile.ok_or_else(|| {
                                anyhow::anyhow!("drift reported without an active profile")
                            })?,
                            drift: active.drift,
                        });
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
                    registry,
                    integration.as_ref(),
                    env,
                    store,
                    &request.profile,
                    DriftDecision::DiscardChanges,
                ) {
                    Ok(result) => results.applied.push(result),
                    Err(error) => results.failures.push((
                        integration.instance_id().to_string(),
                        integration.display_name().to_string(),
                        error,
                    )),
                }
            }
            Ok(UseProfileOutcome::All(results))
        }
    }
}

fn apply_one(
    registry: &dyn HarnessRegistry,
    integration: &dyn HarnessIntegration,
    env: &AppEnvironment,
    store: &ProfileStore,
    profile: &ProfileName,
    decision: DriftDecision,
) -> Result<ProfileUseResult> {
    ensure_target_profile_can_be_used(store, profile)?;
    let paths = integration.paths(env)?;

    let state_path = env.lazyagents_home.join("state.json");
    let mut state = LazyagentsState::load(&state_path)?;
    let active_profile = state
        .active_profiles
        .get(integration.instance_id())
        .map(|name| active_profile_for_drift(integration, store, name))
        .transpose()?;
    let drift = match active_profile.as_ref() {
        Some(active) => integration.detect_drift(active, &paths)?,
        None => DriftReport::clean(),
    };

    if !drift.is_clean() {
        match decision {
            DriftDecision::Cancel => {
                return Ok(ProfileUseResult {
                    harness: integration.instance_id().to_string(),
                    display_name: integration.display_name().to_string(),
                    alias_updates: Vec::new(),
                    profile: profile.clone(),
                    status: ProfileUseStatus::CancelledForDrift,
                });
            }
            DriftDecision::DiscardChanges => {}
            DriftDecision::SaveChanges => {
                let mut imported = integration.import_from_harness(&paths)?;
                let shared_skills = merge_shared_agent_skills(&mut imported.skills, env)?;
                if let Some(active) = active_profile.as_ref() {
                    store.apply_import(&active.name, integration.instance_id(), imported)?;
                    remove_imported_shared_skills(&shared_skills)?;
                }
            }
        }
    }

    apply_profile_to_harness_with_commit(integration, env, store, profile, || {
        for alias in registry.aliases_for(env, integration)? {
            state.active_profiles.insert(alias, profile.clone());
        }
        state.save(&state_path)
    })?;
    let alias_updates = registry
        .aliases_for(env, integration)?
        .into_iter()
        .filter(|alias| alias != integration.instance_id())
        .collect();
    Ok(ProfileUseResult {
        harness: integration.instance_id().to_string(),
        display_name: integration.display_name().to_string(),
        alias_updates,
        profile: profile.clone(),
        status: ProfileUseStatus::Applied,
    })
}

struct ActiveDrift {
    profile: Option<ProfileName>,
    drift: DriftReport,
}

fn active_drift(
    integration: &dyn HarnessIntegration,
    env: &AppEnvironment,
    store: &ProfileStore,
) -> Result<ActiveDrift> {
    let paths = integration.paths(env)?;
    let state = LazyagentsState::load(&env.lazyagents_home.join("state.json"))?;
    let active_profile = state
        .active_profiles
        .get(integration.instance_id())
        .map(|name| active_profile_for_drift(integration, store, name))
        .transpose()?;
    match active_profile.as_ref() {
        Some(active) => Ok(ActiveDrift {
            profile: Some(active.name.clone()),
            drift: integration.detect_drift(active, &paths)?,
        }),
        None => Ok(ActiveDrift {
            profile: None,
            drift: DriftReport::clean(),
        }),
    }
}

fn ensure_target_profile_can_be_used(store: &ProfileStore, profile: &ProfileName) -> Result<()> {
    let profile_dir = store.profile_dir(profile);
    if !profile_dir.is_dir() {
        anyhow::bail!(
            "profile {profile} does not exist at {}",
            profile_dir.display()
        );
    }
    store.load_config(profile)?;
    Ok(())
}

fn active_profile_for_drift(
    integration: &dyn HarnessIntegration,
    store: &ProfileStore,
    name: &ProfileName,
) -> Result<ProfileRef> {
    let path = store.profile_dir(name);
    if !path.is_dir() {
        anyhow::bail!("active profile {name} is missing at {}", path.display());
    }

    store.load_config(name)?;

    let instruction_source = path.join("AGENTS.md");
    if !instruction_source.is_file() {
        anyhow::bail!(
            "active profile {name} is missing instruction source at {}",
            instruction_source.display()
        );
    }

    if integration.supports_mcp() {
        read_mcp_definitions(&path)?;
    }

    Ok(ProfileRef {
        name: name.clone(),
        path,
        harness_id: integration.instance_id().to_string(),
    })
}

fn detected_integrations(
    registry: &dyn HarnessRegistry,
    env: &AppEnvironment,
) -> Result<Vec<Box<dyn HarnessIntegration>>> {
    let mut detected = Vec::new();
    for integration in registry.all(env)? {
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
    use crate::harness::kind::HarnessKind;
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
        supports_mcp: bool,
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
                supports_mcp: true,
                import_called: Cell::new(false),
            }
        }

        fn without_mcp(mut self) -> Self {
            self.supports_mcp = false;
            self
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

        fn default_config_dir(&self, _env: &AppEnvironment) -> PathBuf {
            self.root.clone()
        }

        fn paths_from_config_dir(&self, config_dir: PathBuf) -> Result<HarnessConfigPaths> {
            Ok(HarnessConfigPaths {
                instruction_target: config_dir.join("AGENTS.md"),
                skills_dir: config_dir.join("skills"),
                commands_dir: config_dir.join("commands"),
                settings_file: config_dir.join("settings.json"),
                mcp_file: config_dir.join("mcp.json"),
                config_dir,
            })
        }

        fn supports_mcp(&self) -> bool {
            self.supports_mcp
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
            self.paths_from_config_dir(self.root.clone())
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
        fn all(&self, _env: &AppEnvironment) -> Result<Vec<Box<dyn HarnessIntegration>>> {
            Ok(self
                .integrations
                .iter()
                .cloned()
                .map(|integration| Box::new(integration) as Box<dyn HarnessIntegration>)
                .collect())
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
                target: UseProfileTarget::Harness("codex".to_string()),
                drift_decision: None,
            },
        )
        .unwrap();

        assert!(matches!(
            outcome,
            UseProfileOutcome::NeedsSingleHarnessDriftDecision {
                display_name,
                ..
            } if display_name == "Codex"
        ));
    }

    #[test]
    fn cancel_for_drift_does_not_normalize_target_profile() {
        let fixture = Fixture::new();
        fixture.profile("active");
        fixture.profile("target");
        fs::remove_file(fixture.profile_path("target").join("AGENTS.md")).unwrap();
        fs::remove_file(fixture.profile_path("target").join("mcps.json")).unwrap();
        fs::remove_dir_all(fixture.profile_path("target").join("skills")).unwrap();
        fs::remove_dir_all(fixture.profile_path("target").join("commands")).unwrap();
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
                target: UseProfileTarget::Harness("codex".to_string()),
                drift_decision: Some(DriftDecision::Cancel),
            },
        )
        .unwrap();

        assert!(matches!(
            outcome,
            UseProfileOutcome::Applied(ProfileUseResult {
                status: ProfileUseStatus::CancelledForDrift,
                ..
            })
        ));
        assert!(!fixture.profile_path("target").join("AGENTS.md").exists());
        assert!(!fixture.profile_path("target").join("mcps.json").exists());
        assert!(!fixture.profile_path("target").join("skills").exists());
        assert!(!fixture.profile_path("target").join("commands").exists());
    }

    #[test]
    fn active_profile_missing_instruction_fails_before_drift_decision() {
        let fixture = Fixture::new();
        fixture.profile("active");
        fixture.profile("target");
        fs::remove_file(fixture.profile_path("active").join("AGENTS.md")).unwrap();
        fixture.write_state(r#"{"active_profiles":{"codex":"active"}}"#);
        let registry = FakeCatalog {
            integrations: vec![FakeIntegration::new(
                HarnessKind::Codex,
                fixture.harness("codex"),
            )],
        };

        let error = match use_profile_workflow(
            &registry,
            &fixture.env,
            &fixture.store,
            UseProfileRequest {
                profile: ProfileName::parse("target").unwrap(),
                target: UseProfileTarget::Harness("codex".to_string()),
                drift_decision: None,
            },
        ) {
            Ok(_) => panic!("expected missing active instruction to fail"),
            Err(error) => error,
        };

        assert!(error
            .to_string()
            .contains("active profile active is missing instruction source"));
    }

    #[test]
    fn active_profile_invalid_mcp_is_ignored_for_harness_without_mcp_support() {
        let fixture = Fixture::new();
        fixture.profile("active");
        fixture.profile("target");
        fs::write(fixture.profile_path("active").join("mcps.json"), "not json").unwrap();
        fixture.write_state(r#"{"active_profiles":{"codex":"active"}}"#);
        let integration =
            FakeIntegration::new(HarnessKind::Codex, fixture.harness("codex")).without_mcp();
        let registry = FakeCatalog {
            integrations: vec![integration],
        };

        let outcome = use_profile_workflow(
            &registry,
            &fixture.env,
            &fixture.store,
            UseProfileRequest {
                profile: ProfileName::parse("target").unwrap(),
                target: UseProfileTarget::Harness("codex".to_string()),
                drift_decision: None,
            },
        )
        .unwrap();

        match outcome {
            UseProfileOutcome::Applied(result) => {
                assert_eq!(result.profile, ProfileName::parse("target").unwrap());
            }
            _ => panic!("expected apply outcome"),
        }
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
                target: UseProfileTarget::Harness("codex".to_string()),
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
        assert_eq!(config.model_preference("codex"), "saved-model");
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
                target: UseProfileTarget::Harness("codex".to_string()),
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
        assert_eq!(results.applied[0].harness, "codex");
        assert_eq!(results.failures.len(), 1);
        assert_eq!(results.failures[0].0, "claude");
        let state = LazyagentsState::load(&fixture.home.join("state.json")).unwrap();
        assert_eq!(state.active_profiles.get("codex").unwrap().as_str(), "work");
        assert!(!state.active_profiles.contains_key("claude"));
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
