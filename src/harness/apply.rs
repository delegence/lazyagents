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
    pub warnings: Vec<String>,
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
    let paths = integration.paths(env)?;
    let target_profile = load_profile(profile_store, profile, integration.instance_id());
    let surfaces = integration.managed_surfaces(&paths);
    reject_managed_path_overlap(env, &target_profile, &surfaces)?;
    profile_store.normalize_optional_artifacts(profile)?;
    integration.preflight(&target_profile, &paths)?;
    let backup =
        ManagedBackup::capture(&env.lazyagents_home, integration.instance_id(), &surfaces)?;
    if let Err(error) =
        apply_transaction(integration, &target_profile, &paths, &surfaces).and_then(|_| commit())
    {
        backup
            .restore()
            .context("profile use failed and rollback failed")?;
        return Err(error);
    }
    Ok(())
}

fn reject_managed_path_overlap(
    env: &AppEnvironment,
    profile: &ProfileRef,
    surfaces: &[crate::harness::managed::ManagedSurface],
) -> Result<()> {
    let protected = [
        ("source profile", profile.path.as_path()),
        ("LazyAgents home", env.lazyagents_home.as_path()),
    ];
    for surface in surfaces {
        let managed = crate::file_system::resolve_path_identity(&surface.path)?;
        for (label, path) in protected {
            let protected = crate::file_system::resolve_path_identity(path)?;
            if managed.starts_with(&protected) || protected.starts_with(&managed) {
                anyhow::bail!(
                    "managed path {} overlaps the {label} at {}",
                    surface.path.display(),
                    path.display()
                );
            }
        }
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
    use crate::harness::artifact::{ArtifactContext, ArtifactKind, HarnessArtifact};
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

        fn artifacts(&self) -> Vec<Box<dyn HarnessArtifact>> {
            vec![
                Box::new(FakeInstructionArtifact {
                    fail_verify: self.fail_verify,
                }),
                Box::new(FakeSkillsArtifact),
                Box::new(FakeCommandsArtifact),
            ]
        }
    }

    struct FakeInstructionArtifact {
        fail_verify: bool,
    }

    impl HarnessArtifact for FakeInstructionArtifact {
        fn kind(&self) -> ArtifactKind {
            ArtifactKind::Instructions
        }

        fn surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface> {
            vec![ManagedSurface::file(&paths.instruction_target)]
        }

        fn apply(&self, ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
            fs::write(
                &ctx.paths.instruction_target,
                format!("profile={}", profile.name.as_str()),
            )?;
            Ok(())
        }

        fn verify(&self, _ctx: &ArtifactContext<'_>, _profile: &ProfileRef) -> Result<()> {
            if self.fail_verify {
                Err(anyhow!("verify failed"))
            } else {
                Ok(())
            }
        }
    }

    struct FakeSkillsArtifact;

    impl HarnessArtifact for FakeSkillsArtifact {
        fn kind(&self) -> ArtifactKind {
            ArtifactKind::Skills
        }

        fn surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface> {
            vec![ManagedSurface::directory(&paths.skills_dir)]
        }

        fn apply(&self, ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
            if profile.name.as_str() == "full" {
                fs::write(ctx.paths.skills_dir.join("skill.txt"), "skill")?;
            }
            Ok(())
        }
    }

    struct FakeCommandsArtifact;

    impl HarnessArtifact for FakeCommandsArtifact {
        fn kind(&self) -> ArtifactKind {
            ArtifactKind::Commands
        }

        fn surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface> {
            vec![ManagedSurface::directory(&paths.commands_dir)]
        }

        fn apply(&self, ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
            if profile.name.as_str() == "full" {
                fs::write(ctx.paths.commands_dir.join("cmd.md"), "cmd")?;
            }
            Ok(())
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

    #[cfg(unix)]
    #[test]
    fn verify_failure_preserves_hidden_opaque_entries_in_managed_directories() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::{symlink, FileTypeExt};

        unsafe extern "C" {
            fn mkfifo(path: *const std::os::raw::c_char, mode: u32) -> i32;
        }

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("lazyagents");
        let harness_root = temp.path().join("harness");
        let skills = harness_root.join("skills");
        fs::create_dir_all(skills.join("visible/nested")).unwrap();
        fs::write(skills.join("visible/old.txt"), "old").unwrap();
        fs::write(skills.join(".hidden"), "secret").unwrap();
        fs::write(skills.join("visible/nested/.data"), "nested").unwrap();
        let external = temp.path().join("external");
        fs::write(&external, "external").unwrap();
        symlink(&external, skills.join(".external-link")).unwrap();
        let fifo = skills.join(".pipe");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        fs::write(harness_root.join("AGENTS.md"), "previous").unwrap();
        let store = ProfileStore::new(LazyagentsHome::from_path(&home));
        let profile = ProfileName::parse("work").unwrap();
        store.create_skeleton(&profile).unwrap();

        let error = apply_profile_to_harness_with_commit(
            &FakeIntegration::new(harness_root.clone()).fail_verify(),
            &test_env(&home),
            &store,
            &profile,
            || Ok(()),
        )
        .unwrap_err();

        assert!(error.to_string().contains("verify failed"));
        assert_eq!(
            fs::read_to_string(skills.join("visible/old.txt")).unwrap(),
            "old"
        );
        assert_eq!(
            fs::read_to_string(skills.join(".hidden")).unwrap(),
            "secret"
        );
        assert_eq!(
            fs::read_to_string(skills.join("visible/nested/.data")).unwrap(),
            "nested"
        );
        assert!(fs::symlink_metadata(skills.join(".external-link"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(fs::symlink_metadata(fifo).unwrap().file_type().is_fifo());
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

    #[test]
    fn rejects_profile_and_control_path_overlap_before_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("lazyagents");
        let store = ProfileStore::new(LazyagentsHome::from_path(&home));
        let profile = ProfileName::parse("work").unwrap();
        store.create_skeleton(&profile).unwrap();
        let profile_path = store.profile_dir(&profile);
        let env = test_env(&home);

        for managed in [
            profile_path.clone(),
            profile_path.join("nested"),
            profile_path.join("nested/../skills"),
            home.join("backups"),
        ] {
            let marker = profile_path.join("PROFILE.md");
            let before = fs::read(&marker).unwrap();
            let error = reject_managed_path_overlap(
                &env,
                &ProfileRef {
                    name: profile.clone(),
                    path: profile_path.clone(),
                    harness_id: "codex".to_string(),
                },
                &[ManagedSurface::directory(managed)],
            )
            .unwrap_err();
            assert!(error.to_string().contains("overlaps"));
            assert_eq!(fs::read(&marker).unwrap(), before);
        }
    }

    #[test]
    fn rejected_apply_does_not_normalize_or_create_backup() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("lazyagents");
        let store = ProfileStore::new(LazyagentsHome::from_path(&home));
        let profile = ProfileName::parse("work").unwrap();
        store.create_skeleton(&profile).unwrap();
        let profile_path = store.profile_dir(&profile);
        fs::remove_dir_all(profile_path.join("skills")).unwrap();
        fs::remove_dir_all(profile_path.join("commands")).unwrap();
        fs::remove_dir_all(profile_path.join("agents")).unwrap();
        fs::remove_file(profile_path.join("mcps.json")).unwrap();
        let integration = FakeIntegration::new(profile_path.clone());

        let error = apply_profile_to_harness_with_commit(
            &integration,
            &test_env(&home),
            &store,
            &profile,
            || Ok(()),
        )
        .unwrap_err();

        assert!(error.to_string().contains("overlaps"));
        assert!(!profile_path.join("skills").exists());
        assert!(!profile_path.join("mcps.json").exists());
        assert!(!home.join("backups").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_overlap_through_a_symlinked_ancestor() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("lazyagents");
        let store = ProfileStore::new(LazyagentsHome::from_path(&home));
        let profile = ProfileName::parse("work").unwrap();
        store.create_skeleton(&profile).unwrap();
        let link = temp.path().join("profile-link");
        symlink(store.profile_dir(&profile), &link).unwrap();

        let error = reject_managed_path_overlap(
            &test_env(&home),
            &ProfileRef {
                name: profile.clone(),
                path: store.profile_dir(&profile),
                harness_id: "codex".to_string(),
            },
            &[ManagedSurface::directory(link.join("missing"))],
        )
        .unwrap_err();

        assert!(error.to_string().contains("overlaps"));
        assert!(store.profile_dir(&profile).join("PROFILE.md").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn rejected_apply_resolves_symlink_before_parent_component() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("lazyagents");
        let store = ProfileStore::new(LazyagentsHome::from_path(&home));
        let profile = ProfileName::parse("work").unwrap();
        store.create_skeleton(&profile).unwrap();
        let profile_path = store.profile_dir(&profile);
        fs::create_dir(profile_path.join("sub")).unwrap();
        let config = temp.path().join("config");
        fs::create_dir(&config).unwrap();
        symlink(profile_path.join("sub"), config.join("link")).unwrap();
        let outside = config.join("skills");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), "outside").unwrap();
        let before = fs::read(profile_path.join("PROFILE.md")).unwrap();

        let error = apply_profile_to_harness_with_commit(
            &FakeIntegration::new(config.join("link/../skills")),
            &test_env(&home),
            &store,
            &profile,
            || Ok(()),
        )
        .unwrap_err();

        assert!(error.to_string().contains("overlaps"));
        assert_eq!(fs::read(profile_path.join("PROFILE.md")).unwrap(), before);
        assert_eq!(
            fs::read_to_string(outside.join("sentinel")).unwrap(),
            "outside"
        );
        assert!(!home.join("backups").exists());
    }

    fn test_env(home: &Path) -> AppEnvironment {
        AppEnvironment {
            lazyagents_home: home.to_path_buf(),
            user_home: home.join("user"),
            path_entries: Vec::new(),
        }
    }
}
