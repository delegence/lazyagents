use anyhow::{Context, Result};

use crate::harness::integration::{
    AppEnvironment, HarnessConfigPaths, HarnessIntegration, ProfileRef,
};
use crate::harness::kind::HarnessKind;
use crate::harness::managed::{clear_surfaces, ManagedBackup};
use crate::profile::{ProfileName, ProfileStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileUseStatus {
    Applied,
    CancelledForDrift,
}

#[derive(Debug, Clone)]
pub struct ProfileUseResult {
    pub harness: HarnessKind,
    pub profile: ProfileName,
    pub status: ProfileUseStatus,
}

pub fn apply_profile_to_harness_with_commit<F>(
    integration: &dyn HarnessIntegration,
    env: &AppEnvironment,
    profile_store: &ProfileStore,
    profile: &ProfileName,
    commit: F,
) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    profile_store.normalize_optional_artifacts(profile)?;
    let paths = integration.paths(env)?;
    let target_profile = load_profile(profile_store, profile);
    integration.preflight(&target_profile)?;
    let surfaces = integration.managed_surfaces(&paths);
    let backup = ManagedBackup::capture(&env.lazyagents_home, integration.kind(), &surfaces)?;
    if let Err(error) =
        apply_transaction(integration, &target_profile, &paths, &surfaces).and_then(|_| commit())
    {
        backup
            .restore(&surfaces)
            .context("profile use failed and rollback failed")?;
        return Err(error);
    }
    Ok(())
}

fn apply_transaction(
    integration: &dyn HarnessIntegration,
    profile: &ProfileRef,
    paths: &HarnessConfigPaths,
    surfaces: &[crate::harness::managed::ManagedSurface],
) -> Result<()> {
    clear_surfaces(surfaces)?;
    integration.apply(profile, paths)?;
    integration.verify(profile, paths)?;
    Ok(())
}

fn load_profile(profile_store: &ProfileStore, profile: &ProfileName) -> ProfileRef {
    ProfileRef {
        name: profile.clone(),
        path: profile_store.profile_dir(profile),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::path::{Path, PathBuf};

    use anyhow::anyhow;

    use super::*;
    use crate::app::state::LazyagentsState;
    use crate::harness::drift::DriftItem;
    use crate::harness::drift::DriftReport;
    use crate::harness::integration::{HarnessDetection, ImportedPreference, ProfileImport};
    use crate::harness::managed::ManagedSurface;
    use crate::profile::LazyagentsHome;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum DriftPolicy {
        Cancel,
        Discard,
        SaveChanges,
    }

    fn use_profile(
        integration: &dyn HarnessIntegration,
        env: &AppEnvironment,
        profile_store: &ProfileStore,
        profile: &ProfileName,
        policy: DriftPolicy,
    ) -> Result<ProfileUseResult> {
        profile_store.normalize_optional_artifacts(profile)?;
        let paths = integration.paths(env)?;
        let state_path = env.lazyagents_home.join("state.json");
        let mut state = LazyagentsState::load(&state_path)?;
        let active_profile = state
            .active_profiles
            .get(&integration.kind())
            .map(|name| load_profile(profile_store, name));

        let drift = match active_profile.as_ref() {
            Some(active) => integration.detect_drift(active, &paths)?,
            None => DriftReport::clean(),
        };

        if !drift.is_clean() {
            match policy {
                DriftPolicy::Cancel => {
                    return Ok(ProfileUseResult {
                        harness: integration.kind(),
                        profile: profile.clone(),
                        status: ProfileUseStatus::CancelledForDrift,
                    });
                }
                DriftPolicy::Discard => {}
                DriftPolicy::SaveChanges => {
                    let imported = integration.import_from_harness(&paths)?;
                    if let Some(active) = active_profile.as_ref() {
                        profile_store.apply_import(&active.name, integration.kind(), imported)?;
                    }
                }
            }
        }

        apply_profile_to_harness_with_commit(integration, env, profile_store, profile, || {
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

    struct UseProfileAllResult {
        applied: Vec<ProfileUseResult>,
        failures: Vec<(HarnessKind, anyhow::Error)>,
    }

    fn use_profile_all<F>(
        integrations: &[&dyn HarnessIntegration],
        env: &AppEnvironment,
        profile_store: &ProfileStore,
        profile: &ProfileName,
        discard_changes: bool,
        mut prompt_discard_drift: F,
    ) -> Result<UseProfileAllResult>
    where
        F: FnMut(&[String]) -> Result<bool>,
    {
        let mut drifted_harnesses = Vec::new();
        for integration in integrations {
            let paths = integration.paths(env)?;
            let state = LazyagentsState::load(&env.lazyagents_home.join("state.json"))?;
            let active_profile = state
                .active_profiles
                .get(&integration.kind())
                .map(|name| load_profile(profile_store, name));
            let drift = match active_profile.as_ref() {
                Some(active) => integration.detect_drift(active, &paths)?,
                None => DriftReport::clean(),
            };
            if !drift.is_clean() {
                drifted_harnesses.push(integration.kind().display_name().to_string());
            }
        }

        if !drifted_harnesses.is_empty()
            && !discard_changes
            && !prompt_discard_drift(&drifted_harnesses)?
        {
            anyhow::bail!(
                "operation cancelled due to drift in {}",
                drifted_harnesses.join(", ")
            );
        }

        let mut results = UseProfileAllResult {
            applied: Vec::new(),
            failures: Vec::new(),
        };

        for integration in integrations {
            match use_profile(
                *integration,
                env,
                profile_store,
                profile,
                DriftPolicy::Discard,
            ) {
                Ok(res) => match res.status {
                    ProfileUseStatus::Applied => results.applied.push(res),
                    ProfileUseStatus::CancelledForDrift => {}
                },
                Err(e) => results.failures.push((integration.kind(), e)),
            }
        }

        Ok(results)
    }

    struct FakeIntegration {
        root: PathBuf,
        drift: DriftReport,
        fail_verify: bool,
        import_called: Cell<bool>,
    }

    impl FakeIntegration {
        fn new(root: PathBuf) -> Self {
            Self {
                root,
                drift: DriftReport::clean(),
                fail_verify: false,
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

        fn fail_verify(mut self) -> Self {
            self.fail_verify = true;
            self
        }
    }

    impl HarnessIntegration for FakeIntegration {
        fn kind(&self) -> HarnessKind {
            HarnessKind::Codex
        }

        fn detect(&self, _env: &AppEnvironment) -> Result<HarnessDetection> {
            Ok(HarnessDetection::Detected {
                binary_path: self.root.join("bin").join("fake"),
            })
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
                instruction: Some("saved".to_string()),
                skills: Vec::new(),
                commands: Vec::new(),
                mcp_definitions: None,
                model_preference: ImportedPreference::new(serde_json::json!("imported-model")),
                permission_preference: ImportedPreference::new(serde_json::json!({"bash":"allow"})),
            })
        }

        fn apply(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
            fs::create_dir_all(&paths.config_dir)?;
            fs::create_dir_all(&paths.skills_dir)?;
            fs::create_dir_all(&paths.commands_dir)?;
            fs::write(
                &paths.instruction_target,
                format!("profile={}", profile.name.as_str()),
            )?;
            if profile.name.as_str() == "full" {
                fs::write(paths.skills_dir.join("skill.txt"), "skill")?;
                fs::write(paths.commands_dir.join("cmd.md"), "cmd")?;
            }
            Ok(())
        }

        fn verify(&self, _profile: &ProfileRef, _paths: &HarnessConfigPaths) -> Result<()> {
            if self.fail_verify {
                Err(anyhow!("verify failed"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn shared_orchestration_removes_stale_managed_surfaces() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("lazyagents");
        let harness_root = temp.path().join("harness");
        let store = ProfileStore::new(LazyagentsHome::from_path(&home));
        let full = ProfileName::parse("full").unwrap();
        let empty = ProfileName::parse("empty").unwrap();
        store.create_skeleton(&full).unwrap();
        store.create_skeleton(&empty).unwrap();
        let env = test_env(&home);
        let integration = FakeIntegration::new(harness_root.clone());

        use_profile(&integration, &env, &store, &full, DriftPolicy::Discard).unwrap();
        use_profile(&integration, &env, &store, &empty, DriftPolicy::Discard).unwrap();

        assert_eq!(
            fs::read_to_string(harness_root.join("AGENTS.md")).unwrap(),
            "profile=empty"
        );
        assert!(fs::read_dir(harness_root.join("skills"))
            .unwrap()
            .next()
            .is_none());
        assert!(fs::read_dir(harness_root.join("commands"))
            .unwrap()
            .next()
            .is_none());
        assert_eq!(
            fs::read_to_string(home.join("state.json")).unwrap(),
            "{\n  \"active_profiles\": {\n    \"codex\": \"empty\"\n  }\n}\n"
        );
    }

    #[test]
    fn rollback_restores_surfaces_and_skips_state_update_on_verify_failure() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("lazyagents");
        let harness_root = temp.path().join("harness");
        fs::create_dir_all(harness_root.join("skills")).unwrap();
        fs::write(harness_root.join("AGENTS.md"), "previous").unwrap();
        fs::write(harness_root.join("skills").join("old.txt"), "old").unwrap();
        let store = ProfileStore::new(LazyagentsHome::from_path(&home));
        let profile = ProfileName::parse("work").unwrap();
        store.create_skeleton(&profile).unwrap();
        let env = test_env(&home);
        let integration = FakeIntegration::new(harness_root.clone()).fail_verify();

        let error =
            use_profile(&integration, &env, &store, &profile, DriftPolicy::Discard).unwrap_err();

        assert!(error.to_string().contains("verify failed"));
        assert_eq!(
            fs::read_to_string(harness_root.join("AGENTS.md")).unwrap(),
            "previous"
        );
        assert_eq!(
            fs::read_to_string(harness_root.join("skills").join("old.txt")).unwrap(),
            "old"
        );
        assert!(!home.join("state.json").exists());
    }

    #[test]
    fn rollback_restores_surfaces_when_state_update_fails() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("lazyagents");
        let harness_root = temp.path().join("harness");
        fs::create_dir_all(harness_root.join("skills")).unwrap();
        fs::write(harness_root.join("AGENTS.md"), "previous").unwrap();
        fs::write(harness_root.join("skills").join("old.txt"), "old").unwrap();
        let store = ProfileStore::new(LazyagentsHome::from_path(&home));
        let profile = ProfileName::parse("work").unwrap();
        store.create_skeleton(&profile).unwrap();
        fs::create_dir(home.join("state.json")).unwrap();
        let env = test_env(&home);
        let integration = FakeIntegration::new(harness_root.clone());

        let error =
            use_profile(&integration, &env, &store, &profile, DriftPolicy::Discard).unwrap_err();

        assert!(error.to_string().contains("state.json"));
        assert_eq!(
            fs::read_to_string(harness_root.join("AGENTS.md")).unwrap(),
            "previous"
        );
        assert_eq!(
            fs::read_to_string(harness_root.join("skills").join("old.txt")).unwrap(),
            "old"
        );
        assert!(home.join("state.json").is_dir());
    }

    #[test]
    fn save_changes_reuses_import_before_apply() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("lazyagents");
        let harness_root = temp.path().join("harness");
        let store = ProfileStore::new(LazyagentsHome::from_path(&home));
        let active = ProfileName::parse("active").unwrap();
        let target = ProfileName::parse("target").unwrap();
        store.create_skeleton(&active).unwrap();
        store.create_skeleton(&target).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("state.json"),
            "{\n  \"active_profiles\": {\n    \"codex\": \"active\"\n  }\n}\n",
        )
        .unwrap();
        let env = test_env(&home);
        let integration = FakeIntegration::new(harness_root).with_drift();

        let result = use_profile(
            &integration,
            &env,
            &store,
            &target,
            DriftPolicy::SaveChanges,
        )
        .unwrap();

        assert_eq!(result.status, ProfileUseStatus::Applied);
        assert!(integration.import_called.get());

        let active_config = store.load_config(&active).unwrap();
        let target_config = store.load_config(&target).unwrap();
        assert_eq!(
            active_config.model_preference(crate::harness::kind::HarnessKind::Codex),
            "imported-model"
        );
        assert_eq!(
            active_config.permission_preference(crate::harness::kind::HarnessKind::Codex),
            serde_json::json!({"bash":"allow"})
        );
        assert_eq!(
            active_config.model_preference(crate::harness::kind::HarnessKind::Claude),
            "default"
        );
        assert_eq!(
            target_config.model_preference(crate::harness::kind::HarnessKind::Codex),
            "default"
        );
    }

    #[test]
    fn cancel_policy_returns_without_mutation_when_drift_exists() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("lazyagents");
        let harness_root = temp.path().join("harness");
        let store = ProfileStore::new(LazyagentsHome::from_path(&home));
        let active = ProfileName::parse("active").unwrap();
        let target = ProfileName::parse("target").unwrap();
        store.create_skeleton(&active).unwrap();
        store.create_skeleton(&target).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("state.json"),
            "{\n  \"active_profiles\": {\n    \"codex\": \"active\"\n  }\n}\n",
        )
        .unwrap();
        let env = test_env(&home);
        let integration = FakeIntegration::new(harness_root.clone()).with_drift();

        let result = use_profile(&integration, &env, &store, &target, DriftPolicy::Cancel).unwrap();

        assert_eq!(result.status, ProfileUseStatus::CancelledForDrift);
        assert!(!harness_root.join("AGENTS.md").exists());
        assert_eq!(
            fs::read_to_string(home.join("state.json")).unwrap(),
            "{\n  \"active_profiles\": {\n    \"codex\": \"active\"\n  }\n}\n"
        );
    }

    fn test_env(home: &Path) -> AppEnvironment {
        AppEnvironment {
            lazyagents_home: home.to_path_buf(),
            user_home: home.join("user"),
            path_entries: Vec::new(),
        }
    }

    #[test]
    fn use_profile_all_reports_failures_but_applies_successes() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("lazyagents");
        let harness_root1 = temp.path().join("harness1");
        let harness_root2 = temp.path().join("harness2");
        let store = ProfileStore::new(LazyagentsHome::from_path(&home));
        let profile = ProfileName::parse("target").unwrap();
        store.create_skeleton(&profile).unwrap();

        let env = test_env(&home);

        struct Integration2 {
            root: PathBuf,
        }
        impl HarnessIntegration for Integration2 {
            fn kind(&self) -> HarnessKind {
                HarnessKind::Claude
            }
            fn detect(&self, _e: &AppEnvironment) -> Result<HarnessDetection> {
                Ok(HarnessDetection::NotDetected)
            }
            fn paths(&self, _e: &AppEnvironment) -> Result<HarnessConfigPaths> {
                Ok(HarnessConfigPaths {
                    instruction_target: self.root.join("CLAUDE.md"),
                    skills_dir: self.root.join("skills"),
                    commands_dir: self.root.join("commands"),
                    settings_file: self.root.join("settings.json"),
                    mcp_file: self.root.join("settings.json"),
                    config_dir: self.root.clone(),
                })
            }
            fn managed_surfaces(&self, _p: &HarnessConfigPaths) -> Vec<ManagedSurface> {
                vec![]
            }
            fn preflight(&self, _p: &crate::harness::integration::ProfileRef) -> Result<()> {
                Ok(())
            }
            fn detect_drift(
                &self,
                _p: &crate::harness::integration::ProfileRef,
                _paths: &HarnessConfigPaths,
            ) -> Result<DriftReport> {
                Ok(DriftReport::clean())
            }
            fn apply(
                &self,
                _p: &crate::harness::integration::ProfileRef,
                _paths: &HarnessConfigPaths,
            ) -> Result<()> {
                anyhow::bail!("apply failure")
            }
            fn verify(
                &self,
                _p: &crate::harness::integration::ProfileRef,
                _paths: &HarnessConfigPaths,
            ) -> Result<()> {
                Ok(())
            }
            fn import_from_harness(&self, _paths: &HarnessConfigPaths) -> Result<ProfileImport> {
                anyhow::bail!("no")
            }
        }

        let integration1 = FakeIntegration::new(harness_root1);
        let integration2 = Integration2 {
            root: harness_root2,
        };

        let integrations: Vec<&dyn HarnessIntegration> = vec![&integration1, &integration2];

        let results =
            use_profile_all(&integrations, &env, &store, &profile, true, |_| Ok(true)).unwrap();

        assert_eq!(results.applied.len(), 1);
        assert_eq!(results.applied[0].harness, HarnessKind::Codex);
        assert_eq!(results.failures.len(), 1);
        assert_eq!(results.failures[0].0, HarnessKind::Claude);
        assert!(results.failures[0].1.to_string().contains("apply failure"));
    }
}
