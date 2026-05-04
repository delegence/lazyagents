use anyhow::{Context, Result};

use crate::harness::integration::{
    AppEnvironment, HarnessConfigPaths, HarnessIntegration, ProfileRef,
};
use crate::harness::managed::{clear_surfaces, ManagedBackup};
use crate::profile::{ProfileName, ProfileStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileUseStatus {
    Applied,
    CancelledForDrift,
}

#[derive(Debug, Clone)]
pub struct ProfileUseResult {
    pub harness: String,
    pub display_name: String,
    pub alias_updates: Vec<String>,
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
    let target_profile = load_profile(profile_store, profile, integration.instance_id());
    integration.preflight(&target_profile)?;
    let surfaces = integration.managed_surfaces(&paths);
    let backup =
        ManagedBackup::capture(&env.lazyagents_home, integration.instance_id(), &surfaces)?;
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

fn load_profile(
    profile_store: &ProfileStore,
    profile: &ProfileName,
    harness_id: &str,
) -> ProfileRef {
    ProfileRef {
        name: profile.clone(),
        path: profile_store.profile_dir(profile),
        harness_id: harness_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use anyhow::{anyhow, Context};

    use super::*;
    use crate::harness::drift::DriftReport;
    use crate::harness::integration::{HarnessDetection, ProfileImport};
    use crate::harness::kind::HarnessKind;
    use crate::harness::managed::ManagedSurface;
    use crate::profile::LazyagentsHome;

    struct FakeIntegration {
        root: PathBuf,
        fail_verify: bool,
    }

    impl FakeIntegration {
        fn new(root: PathBuf) -> Self {
            Self {
                root,
                fail_verify: false,
            }
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

        fn default_config_dir(&self, _env: &AppEnvironment) -> PathBuf {
            self.root.clone()
        }

        fn paths_from_config_dir(&self, config_dir: PathBuf) -> Result<HarnessConfigPaths> {
            Ok(HarnessConfigPaths {
                instruction_target: config_dir.join("AGENTS.md"),
                skills_dir: config_dir.join("skills"),
                commands_dir: config_dir.join("commands"),
                agents_dir: config_dir.join("agents"),
                settings_file: config_dir.join("settings.json"),
                mcp_file: config_dir.join("mcp.json"),
                config_dir,
            })
        }

        fn detect(&self, _env: &AppEnvironment) -> Result<HarnessDetection> {
            Ok(HarnessDetection::Detected {
                binary_path: self.root.join("bin").join("fake"),
            })
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
            Ok(DriftReport::clean())
        }

        fn import_from_harness(&self, _paths: &HarnessConfigPaths) -> Result<ProfileImport> {
            Ok(ProfileImport::default())
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

        apply_profile_to_harness_with_commit(&integration, &env, &store, &full, || Ok(())).unwrap();
        apply_profile_to_harness_with_commit(&integration, &env, &store, &empty, || {
            fs::create_dir_all(&home).unwrap();
            fs::write(
                home.join("state.json"),
                "{\n  \"active_profiles\": {\n    \"codex\": \"empty\"\n  }\n}\n",
            )
            .unwrap();
            Ok(())
        })
        .unwrap();

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
            apply_profile_to_harness_with_commit(&integration, &env, &store, &profile, || {
                fs::create_dir_all(&home).unwrap();
                fs::write(home.join("state.json"), "should not be written").unwrap();
                Ok(())
            })
            .unwrap_err();

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
            apply_profile_to_harness_with_commit(&integration, &env, &store, &profile, || {
                fs::write(home.join("state.json"), "{}").context("failed to write state.json")?;
                Ok(())
            })
            .unwrap_err();

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

    fn test_env(home: &Path) -> AppEnvironment {
        AppEnvironment {
            lazyagents_home: home.to_path_buf(),
            user_home: home.join("user"),
            path_entries: Vec::new(),
        }
    }
}
