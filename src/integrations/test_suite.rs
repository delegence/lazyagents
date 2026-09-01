#[macro_export]
macro_rules! define_standard_harness_tests {
    ($adapter:ty) => {
        #[test]
        fn test_use_normalizes_missing_optional_artifacts() {
            $crate::integrations::test_suite::template::test_use_normalizes_missing_optional_artifacts(&<$adapter>::default());
        }

        #[test]
        fn test_use_removes_stale_surfaces_and_clears_mcp_list() {
            $crate::integrations::test_suite::template::test_use_removes_stale_surfaces_and_optionally_clears_mcp_list(&<$adapter>::default());
        }

        #[test]
        fn test_use_default_preferences_do_not_modify_existing_native_settings() {
            $crate::integrations::test_suite::template::test_use_default_preferences_do_not_modify_existing_native_settings(&<$adapter>::default());
        }

        #[test]
        fn test_use_handles_invalid_disabled_mcp_according_to_harness_support() {
            $crate::integrations::test_suite::template::test_use_handles_invalid_disabled_mcp_according_to_harness_support(&<$adapter>::default());
        }

        #[test]
        fn test_use_rolls_back_and_dereferences_symlink_backup_on_failure() {
            $crate::integrations::test_suite::template::test_use_rolls_back_and_dereferences_symlink_backup_on_failure(&<$adapter>::default());
        }

        #[test]
        fn test_rollback_restores_active_profile_without_drift() {
            $crate::integrations::test_suite::template::test_rollback_restores_active_profile_without_drift(&<$adapter>::default());
        }

        #[test]
        fn test_import_reads_managed_state_and_dereferences_symlinks() {
            $crate::integrations::test_suite::template::test_import_reads_managed_state_and_dereferences_symlinks(&<$adapter>::default());
        }

        #[test]
        fn test_import_fails_on_malformed_native_config() {
            $crate::integrations::test_suite::template::test_import_fails_on_malformed_native_config(&<$adapter>::default());
        }

        #[test]
        fn test_save_changes_imports_drift_into_active_profile_before_switching() {
            $crate::integrations::test_suite::template::test_save_changes_imports_drift_into_active_profile_before_switching(&<$adapter>::default());
        }

        #[test]
        fn test_discard_changes_switches_without_updating_active_profile() {
            $crate::integrations::test_suite::template::test_discard_changes_switches_without_updating_active_profile(&<$adapter>::default());
        }

        #[test]
        fn test_use_applies_profile_artifacts_preferences_mcp_and_state() {
            $crate::integrations::test_suite::template::test_use_applies_profile_artifacts_preferences_mcp_and_state(&<$adapter>::default());
        }

        #[test]
        fn test_nested_commands_behavior() {
            $crate::integrations::test_suite::template::test_nested_commands_behavior(&<$adapter>::default());
        }
    };
}

#[cfg(test)]
pub mod template {
    use crate::app::harness_registry::HarnessRegistry;
    use crate::app::use_profile::{
        use_profile_workflow, DriftDecision, UseProfileOutcome, UseProfileRequest, UseProfileTarget,
    };
    use crate::harness::apply::ProfileUseResult;
    use crate::harness::fs::{symlink_dir, symlink_file};
    use crate::harness::integration::{
        AppEnvironment, HarnessConfigPaths, HarnessIntegration, ProfileImport, ProfileRef,
    };
    use crate::profile::config::{read_profile_document, write_profile_document, ProfileDocument};
    use crate::profile::{
        LazyagentsHome, ProfileConfig, ProfileName, ProfileStore, PROFILE_FILE_NAME,
    };
    use std::fs;
    use std::path::{Path, PathBuf};

    pub struct HarnessTestFixture {
        pub temp: tempfile::TempDir,
        pub home: PathBuf,
        pub env: AppEnvironment,
        pub store: ProfileStore,
    }

    impl HarnessTestFixture {
        pub fn new(bin_name: &str) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("lazyagents");
            let user_home = temp.path().join("user");
            let bin = temp.path().join("bin");
            fs::create_dir_all(&bin).unwrap();
            fs::write(bin.join(bin_name), "").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(bin.join(bin_name), fs::Permissions::from_mode(0o755)).unwrap();
            }
            let env = AppEnvironment {
                lazyagents_home: home.clone(),
                user_home,
                path_entries: vec![bin],
            };
            let store = ProfileStore::new(LazyagentsHome::from_path(&home));
            Self {
                temp,
                home,
                env,
                store,
            }
        }

        pub fn profile(&self, name: &str) -> PathBuf {
            let name = ProfileName::parse(name).unwrap();
            self.store.create_skeleton(&name).unwrap()
        }
    }

    pub fn add_skill(profile: &Path, name: &str) {
        let path = profile.join("skills").join(name);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("SKILL.md"), "").unwrap();
    }

    pub fn add_command(profile: &Path, file_name: &str) {
        let path = profile.join("commands").join(file_name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, "").unwrap();
    }

    pub fn write_config(profile: &Path, content: &str) {
        let config: ProfileConfig = serde_json::from_str(content).unwrap();
        let instructions = read_profile_document(profile)
            .map(|document| document.instructions)
            .unwrap_or_default();
        write_profile_document(
            profile,
            &ProfileDocument {
                config,
                instructions,
            },
        )
        .unwrap();
    }

    pub fn assert_copied_file(target: PathBuf, source: PathBuf) {
        assert!(target.is_file(), "not a regular file: {}", target.display());
        assert!(
            !target.is_symlink(),
            "managed copy must not be a symlink: {}",
            target.display()
        );
        assert_eq!(fs::read(&target).unwrap(), fs::read(&source).unwrap());
    }

    pub fn assert_instructions_applied(target: PathBuf, profile: &Path) {
        let expected = crate::profile::read_profile_instructions(profile).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), expected);
        assert!(
            !fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink(),
            "instruction target should be a regular rendered file: {}",
            target.display()
        );
    }

    pub trait HarnessTestAdapter {
        fn integration(&self) -> Box<dyn HarnessIntegration>;
        fn bin_name(&self) -> &'static str;

        fn assert_mcp_cleared(&self, paths: &HarnessConfigPaths);
        fn write_malformed_native_config(&self, paths: &HarnessConfigPaths);

        fn supports_skills(&self) -> bool {
            self.integration().supports_skills()
        }
        fn supports_commands(&self) -> bool {
            self.integration().supports_commands()
        }
        fn supports_mcp(&self) -> bool {
            self.integration().supports_mcp()
        }
        fn supports_disabled_mcp(&self) -> bool {
            true
        }
        fn supports_nested_commands(&self) -> bool;

        fn write_existing_native_settings(&self, paths: &HarnessConfigPaths);
        fn assert_native_settings_preserved(&self, paths: &HarnessConfigPaths);

        fn setup_native_config_for_import(&self, paths: &HarnessConfigPaths);
        fn assert_imported_native_config(&self, import: &ProfileImport);
        fn setup_native_command_for_import(&self, paths: &HarnessConfigPaths) {
            fs::write(paths.commands_dir.join("cmd.md"), "command").unwrap();
        }
        fn assert_imported_command(&self, import: &ProfileImport) {
            assert_eq!(import.commands[0].contents, b"command");
        }

        fn setup_drift_native_config(&self, paths: &HarnessConfigPaths);
        fn assert_drift_saved(&self, config: &ProfileConfig);
        fn setup_drift_command(&self, paths: &HarnessConfigPaths) {
            fs::write(paths.commands_dir.join("new.md"), "new command").unwrap();
        }
        fn assert_drift_command_saved(&self, active: &Path) {
            assert_eq!(
                fs::read_to_string(active.join("commands").join("new.md")).unwrap(),
                "new command"
            );
        }

        fn write_profile_config(&self, profile: &Path);
        fn assert_applied_native_config(&self, paths: &HarnessConfigPaths);

        fn assert_command_applied(
            &self,
            paths: &HarnessConfigPaths,
            profile: &Path,
            relative: &str,
        ) {
            assert_copied_file(
                paths.commands_dir.join(relative),
                profile.join("commands").join(relative),
            );
        }
    }

    struct SingleHarnessRegistry<'a, A: HarnessTestAdapter> {
        adapter: &'a A,
    }

    impl<A: HarnessTestAdapter> HarnessRegistry for SingleHarnessRegistry<'_, A> {
        fn all(&self, _env: &AppEnvironment) -> anyhow::Result<Vec<Box<dyn HarnessIntegration>>> {
            Ok(vec![self.adapter.integration()])
        }
    }

    pub fn use_profile_for_test<A: HarnessTestAdapter>(
        adapter: &A,
        fixture: &HarnessTestFixture,
        profile: &str,
        decision: DriftDecision,
    ) -> anyhow::Result<ProfileUseResult> {
        let registry = SingleHarnessRegistry { adapter };
        let integration = adapter.integration();
        match use_profile_workflow(
            &registry,
            &fixture.env,
            &fixture.store,
            UseProfileRequest {
                profile: ProfileName::parse(profile).unwrap(),
                target: UseProfileTarget::Harness(integration.instance_id().to_string()),
                drift_decision: Some(decision),
            },
        )? {
            UseProfileOutcome::Applied(result) => Ok(result),
            _ => unreachable!("test supplied an explicit drift decision"),
        }
    }

    pub fn test_use_normalizes_missing_optional_artifacts<A: HarnessTestAdapter>(adapter: &A) {
        let fixture = HarnessTestFixture::new(adapter.bin_name());
        let profile = fixture.profile("work");
        fs::remove_file(profile.join("mcps.json")).unwrap();
        fs::remove_dir_all(profile.join("skills")).unwrap();
        fs::remove_dir_all(profile.join("commands")).unwrap();

        let integration = adapter.integration();
        use_profile_for_test(adapter, &fixture, "work", DriftDecision::DiscardChanges).unwrap();

        assert!(profile.join(PROFILE_FILE_NAME).is_file());
        assert!(profile.join("mcps.json").is_file());
        assert!(profile.join("skills").is_dir());
        assert!(profile.join("commands").is_dir());
        let paths = integration.paths(&fixture.env).unwrap();
        assert_instructions_applied(paths.instruction_target.clone(), &profile);
    }

    pub fn test_use_removes_stale_surfaces_and_optionally_clears_mcp_list<A: HarnessTestAdapter>(
        adapter: &A,
    ) {
        let fixture = HarnessTestFixture::new(adapter.bin_name());
        let full = fixture.profile("full");
        add_skill(&full, "writer");
        add_command(&full, "plan.md");
        fs::write(
            full.join("mcps.json"),
            r#"[{"name":"local","transport":"stdio","command":"server"}]"#,
        )
        .unwrap();
        fixture.profile("empty");

        let integration = adapter.integration();
        use_profile_for_test(adapter, &fixture, "full", DriftDecision::DiscardChanges).unwrap();
        let paths = integration.paths(&fixture.env).unwrap();
        if adapter.supports_skills() {
            fs::write(paths.skills_dir.join("writer/.hidden"), "keep").unwrap();
        }
        if adapter.supports_commands() {
            fs::create_dir_all(paths.commands_dir.join("local-only")).unwrap();
            fs::write(paths.commands_dir.join("local-only/.hidden"), "keep").unwrap();
        }

        use_profile_for_test(adapter, &fixture, "empty", DriftDecision::DiscardChanges).unwrap();

        if adapter.supports_skills() {
            assert_eq!(
                fs::read_to_string(paths.skills_dir.join("writer/.hidden")).unwrap(),
                "keep"
            );
            assert!(!paths.skills_dir.join("writer/SKILL.md").exists());
            assert!(!crate::harness::fs::has_visible_entries(&paths.skills_dir).unwrap());
        }
        if adapter.supports_commands() {
            assert_eq!(
                fs::read_to_string(paths.commands_dir.join("local-only/.hidden")).unwrap(),
                "keep"
            );
            assert!(!crate::harness::fs::has_visible_entries(&paths.commands_dir).unwrap());
        }
        if adapter.supports_mcp() {
            adapter.assert_mcp_cleared(&paths);
        }
    }

    pub fn test_use_default_preferences_do_not_modify_existing_native_settings<
        A: HarnessTestAdapter,
    >(
        adapter: &A,
    ) {
        let fixture = HarnessTestFixture::new(adapter.bin_name());
        let profile = fixture.profile("work");
        write_config(
            &profile,
            r#"{"name": "work", "models": {}, "permissions": {}}"#,
        );
        let integration = adapter.integration();
        let paths = integration.paths(&fixture.env).unwrap();
        fs::create_dir_all(&paths.config_dir).unwrap();

        adapter.write_existing_native_settings(&paths);

        use_profile_for_test(adapter, &fixture, "work", DriftDecision::DiscardChanges).unwrap();

        adapter.assert_native_settings_preserved(&paths);
    }

    pub fn test_use_handles_invalid_disabled_mcp_according_to_harness_support<
        A: HarnessTestAdapter,
    >(
        adapter: &A,
    ) {
        let fixture = HarnessTestFixture::new(adapter.bin_name());
        let profile = fixture.profile("work");
        fs::write(
            profile.join("mcps.json"),
            r#"[{"name":"invalid","enabled":false}]"#,
        )
        .unwrap();

        let integration = adapter.integration();
        let result = use_profile_for_test(adapter, &fixture, "work", DriftDecision::DiscardChanges);

        let state =
            crate::app::state::LazyagentsState::load(&fixture.home.join("state.json")).unwrap();
        if adapter.supports_mcp() {
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("MCP")
                    && (err.contains("requires")
                        || err.contains("missing")
                        || err.contains("invalid"))
            );
            assert!(!state
                .active_profiles
                .contains_key(integration.instance_id()));
        } else {
            result.unwrap();
            assert_eq!(
                state
                    .active_profiles
                    .get(integration.instance_id())
                    .unwrap()
                    .as_str(),
                "work"
            );
        }
    }

    pub fn test_use_rolls_back_and_dereferences_symlink_backup_on_failure<A: HarnessTestAdapter>(
        adapter: &A,
    ) {
        let fixture = HarnessTestFixture::new(adapter.bin_name());
        let profile = fixture.profile("work");
        fs::write(
            profile.join("mcps.json"),
            r#"[{"name":"bad","transport":"stdio"}]"#,
        )
        .unwrap();
        if !adapter.supports_mcp() {
            return;
        }

        let integration = adapter.integration();
        let paths = integration.paths(&fixture.env).unwrap();
        fs::create_dir_all(&paths.config_dir).unwrap();

        let old_source = fixture.temp.path().join("old-source.md");
        fs::write(&old_source, "previous instructions").unwrap();
        symlink_file(&old_source, &paths.instruction_target).unwrap();
        if adapter.supports_skills() {
            fs::create_dir_all(&paths.skills_dir).unwrap();
            fs::write(paths.skills_dir.join("old.txt"), "old").unwrap();
        }
        adapter.write_existing_native_settings(&paths);

        let error = use_profile_for_test(adapter, &fixture, "work", DriftDecision::DiscardChanges)
            .unwrap_err();

        assert!(format!("{error:#}").contains("requires command"));
        assert_eq!(
            fs::read_to_string(&paths.instruction_target).unwrap(),
            "previous instructions"
        );
        assert!(fs::symlink_metadata(&paths.instruction_target)
            .unwrap()
            .file_type()
            .is_symlink());
        if adapter.supports_skills() {
            assert_eq!(
                fs::read_to_string(paths.skills_dir.join("old.txt")).unwrap(),
                "old"
            );
        }
        adapter.assert_native_settings_preserved(&paths);
        assert!(!fixture.home.join("state.json").exists());
    }

    pub fn test_rollback_restores_active_profile_without_drift<A: HarnessTestAdapter>(adapter: &A) {
        if !adapter.supports_mcp() {
            return;
        }

        let fixture = HarnessTestFixture::new(adapter.bin_name());
        let active = fixture.profile("active");
        add_skill(&active, "writer");
        add_command(&active, "plan.md");
        let target = fixture.profile("target");
        fs::write(
            target.join("mcps.json"),
            r#"[{"name":"bad","transport":"stdio"}]"#,
        )
        .unwrap();

        use_profile_for_test(adapter, &fixture, "active", DriftDecision::DiscardChanges).unwrap();
        let error =
            use_profile_for_test(adapter, &fixture, "target", DriftDecision::DiscardChanges)
                .unwrap_err();
        assert!(format!("{error:#}").contains("requires command"));

        let integration = adapter.integration();
        let paths = integration.paths(&fixture.env).unwrap();
        let drift = integration
            .detect_drift(
                &ProfileRef {
                    name: ProfileName::parse("active").unwrap(),
                    path: active,
                    harness_id: integration.instance_id().to_string(),
                },
                &paths,
            )
            .unwrap();
        assert!(
            drift.is_clean(),
            "rollback should restore a drift-clean active profile, got {:?}",
            drift.items
        );
    }

    pub fn test_import_reads_managed_state_and_dereferences_symlinks<A: HarnessTestAdapter>(
        adapter: &A,
    ) {
        let fixture = HarnessTestFixture::new(adapter.bin_name());
        let integration = adapter.integration();
        let paths = integration.paths(&fixture.env).unwrap();

        fs::create_dir_all(&paths.config_dir).unwrap();
        if adapter.supports_skills() {
            fs::create_dir_all(&paths.skills_dir).unwrap();
        }
        if adapter.supports_commands() {
            fs::create_dir_all(&paths.commands_dir).unwrap();
        }
        if let Some(parent) = paths.instruction_target.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let instruction_source = fixture.temp.path().join("instruction-source.md");
        fs::write(&instruction_source, "imported instructions").unwrap();
        symlink_file(&instruction_source, &paths.instruction_target).unwrap();

        let skill_source = fixture.temp.path().join("skill-source");
        fs::create_dir_all(&skill_source).unwrap();
        fs::write(skill_source.join("SKILL.md"), "skill body").unwrap();
        if adapter.supports_skills() {
            symlink_dir(&skill_source, paths.skills_dir.join("linked")).unwrap();
        }

        if adapter.supports_commands() {
            adapter.setup_native_command_for_import(&paths);
        }

        adapter.setup_native_config_for_import(&paths);

        let imported = integration.import_from_harness(&paths).unwrap();

        assert_eq!(
            imported.instruction.as_deref(),
            Some("imported instructions")
        );
        if adapter.supports_skills() {
            assert_eq!(imported.skills[0].name, "linked");
            assert_eq!(imported.skills[0].files[0].contents, b"skill body");
        } else {
            assert!(imported.skills.is_empty());
        }
        if adapter.supports_commands() {
            adapter.assert_imported_command(&imported);
        } else {
            assert!(imported.commands.is_empty());
        }

        if adapter.supports_mcp() {
            assert!(imported.mcp_definitions.is_some());
        } else {
            assert!(imported.mcp_definitions.is_none());
        }
        adapter.assert_imported_native_config(&imported);
    }

    pub fn test_import_fails_on_malformed_native_config<A: HarnessTestAdapter>(adapter: &A) {
        let fixture = HarnessTestFixture::new(adapter.bin_name());
        let integration = adapter.integration();
        let paths = integration.paths(&fixture.env).unwrap();
        fs::create_dir_all(&paths.config_dir).unwrap();

        adapter.write_malformed_native_config(&paths);

        let error = integration.import_from_harness(&paths).unwrap_err();
        println!("Error: {}", error);
        assert!(
            error.to_string().contains("failed to parse")
                || error.to_string().contains("expected")
                || error.to_string().contains("malformed")
                || error.to_string().contains("EOF")
                || error.to_string().contains("invalid Codex config TOML")
                || error.to_string().contains("invalid OpenCode settings")
                || error.to_string().contains("invalid Claude settings")
                || error.to_string().contains("invalid JSON")
        );
    }

    pub fn test_save_changes_imports_drift_into_active_profile_before_switching<
        A: HarnessTestAdapter,
    >(
        adapter: &A,
    ) {
        let fixture = HarnessTestFixture::new(adapter.bin_name());
        let active = fixture.profile("active");
        let target = fixture.profile("target");
        let integration = adapter.integration();

        fs::write(
            fixture.home.join("state.json"),
            format!(
                r#"{{"active_profiles":{{"{}":"active"}}}}"#,
                integration.instance_id()
            ),
        )
        .unwrap();

        let paths = integration.paths(&fixture.env).unwrap();
        fs::create_dir_all(&paths.config_dir).unwrap();
        if adapter.supports_skills() {
            fs::create_dir_all(paths.skills_dir.join("newskill")).unwrap();
            fs::write(
                paths.skills_dir.join("newskill").join("SKILL.md"),
                "new skill",
            )
            .unwrap();
        }
        if adapter.supports_commands() {
            fs::create_dir_all(&paths.commands_dir).unwrap();
            adapter.setup_drift_command(&paths);
        }
        if let Some(parent) = paths.instruction_target.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&paths.instruction_target, "drifted").unwrap();

        adapter.setup_drift_native_config(&paths);

        use_profile_for_test(adapter, &fixture, "target", DriftDecision::SaveChanges).unwrap();

        assert_eq!(
            crate::profile::read_profile_instructions(&active).unwrap(),
            "drifted"
        );
        if adapter.supports_skills() {
            assert_eq!(
                fs::read_to_string(active.join("skills").join("newskill").join("SKILL.md"))
                    .unwrap(),
                "new skill"
            );
        }
        if adapter.supports_commands() {
            adapter.assert_drift_command_saved(&active);
        }

        let active_config = fixture
            .store
            .load_config(&ProfileName::parse("active").unwrap())
            .unwrap();
        adapter.assert_drift_saved(&active_config);

        assert_instructions_applied(paths.instruction_target, &target);
    }

    pub fn test_discard_changes_switches_without_updating_active_profile<A: HarnessTestAdapter>(
        adapter: &A,
    ) {
        let fixture = HarnessTestFixture::new(adapter.bin_name());
        let active = fixture.profile("active");
        let target = fixture.profile("target");
        let integration = adapter.integration();

        fs::write(
            fixture.home.join("state.json"),
            format!(
                r#"{{"active_profiles":{{"{}":"active"}}}}"#,
                integration.instance_id()
            ),
        )
        .unwrap();

        let paths = integration.paths(&fixture.env).unwrap();
        fs::create_dir_all(&paths.config_dir).unwrap();
        if adapter.supports_skills() {
            fs::create_dir_all(paths.skills_dir.join("newskill")).unwrap();
            fs::write(paths.skills_dir.join("newskill").join("SKILL.md"), "drift").unwrap();
        }

        use_profile_for_test(adapter, &fixture, "target", DriftDecision::DiscardChanges).unwrap();

        if adapter.supports_skills() {
            assert!(!active.join("skills").join("newskill").exists());
        }
        assert_instructions_applied(paths.instruction_target, &target);
    }

    pub fn test_use_applies_profile_artifacts_preferences_mcp_and_state<A: HarnessTestAdapter>(
        adapter: &A,
    ) {
        let fixture = HarnessTestFixture::new(adapter.bin_name());
        let profile = fixture.profile("work");
        add_skill(&profile, "writer");
        add_command(&profile, "plan.md");
        adapter.write_profile_config(&profile);

        let mcp_profile = if adapter.supports_disabled_mcp() {
            r#"[
  {"name":"local","transport":"stdio","command":"server","args":["--x"],"env":{"TOKEN":{"env":"TOKEN"}}},
  {"name":"remote","transport":"http","url":"https://mcp.example","headers":{"Authorization":{"env":"TOKEN"},"X-Literal":"abc"}},
  {"name":"disabled","enabled":false,"transport":"stdio","command":"draft-server"}
]"#
        } else {
            r#"[
  {"name":"local","transport":"stdio","command":"server","args":["--x"],"env":{"TOKEN":{"env":"TOKEN"}}},
  {"name":"remote","transport":"http","url":"https://mcp.example","headers":{"Authorization":{"env":"TOKEN"},"X-Literal":"abc"}}
]"#
        };
        fs::write(profile.join("mcps.json"), mcp_profile).unwrap();

        let integration = adapter.integration();
        let paths = integration.paths(&fixture.env).unwrap();
        fs::create_dir_all(&paths.config_dir).unwrap();
        adapter.write_existing_native_settings(&paths);

        use_profile_for_test(adapter, &fixture, "work", DriftDecision::DiscardChanges).unwrap();

        assert_instructions_applied(paths.instruction_target.clone(), &profile);
        if adapter.supports_skills() {
            assert_copied_file(
                paths.skills_dir.join("writer/SKILL.md"),
                profile.join("skills/writer/SKILL.md"),
            );
        }
        if adapter.supports_commands() {
            adapter.assert_command_applied(&paths, &profile, "plan.md");
        }

        adapter.assert_applied_native_config(&paths);
        let drift = integration
            .detect_drift(
                &ProfileRef {
                    name: ProfileName::parse("work").unwrap(),
                    path: profile.clone(),
                    harness_id: integration.instance_id().to_string(),
                },
                &paths,
            )
            .unwrap();
        assert!(
            drift.is_clean(),
            "profile should be clean immediately after apply, got {:?}",
            drift.items
        );

        let state_str = fs::read_to_string(fixture.home.join("state.json")).unwrap();
        assert!(state_str.contains(&format!("\"{}\": \"work\"", integration.instance_id())));

        if adapter.supports_skills() {
            fs::write(paths.skills_dir.join("writer/SKILL.md"), "native edit").unwrap();
            assert_eq!(
                fs::read_to_string(profile.join("skills/writer/SKILL.md")).unwrap(),
                "",
                "editing a harness copy must not edit the profile"
            );
            let drift = integration
                .detect_drift(
                    &ProfileRef {
                        name: ProfileName::parse("work").unwrap(),
                        path: profile,
                        harness_id: integration.instance_id().to_string(),
                    },
                    &paths,
                )
                .unwrap();
            assert!(
                !drift.is_clean(),
                "editing a copied skill should produce drift"
            );
        }
    }

    pub fn test_nested_commands_behavior<A: HarnessTestAdapter>(adapter: &A) {
        let fixture = HarnessTestFixture::new(adapter.bin_name());
        let profile = fixture.profile("work");
        add_command(&profile, "nested/cmd.md");

        let integration = adapter.integration();
        let result = use_profile_for_test(adapter, &fixture, "work", DriftDecision::DiscardChanges);

        if !adapter.supports_commands() {
            result.unwrap();
            return;
        }

        if adapter.supports_nested_commands() {
            result.unwrap();
            let paths = integration.paths(&fixture.env).unwrap();
            adapter.assert_command_applied(&paths, &profile, "nested/cmd.md");
        } else {
            let error = result.unwrap_err();
            let err_str = error.to_string();
            assert!(
                err_str.contains("nested")
                    || err_str.contains("not supported")
                    || err_str.contains("flat")
            );
            assert!(!fixture.home.join("state.json").exists());
        }
    }
}
