use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use crate::harness::artifacts::{
    collect_directory_link_drift, flat_profile_commands, import_flat_commands, import_skills,
    link_flat_commands, link_skills, valid_skills,
};
use crate::harness::drift::{DriftItem, DriftReport};
use crate::harness::fs::{
    detect_binary, read_json, read_optional_string, symlink_file, symlink_points_to,
};
use crate::harness::integration::{
    AppEnvironment, HarnessConfigPaths, HarnessDetection, HarnessIntegration, ImportedPreference,
    ProfileImport, ProfileRef,
};
use crate::harness::kind::HarnessKind;
use crate::harness::managed::{write_text_atomic, ManagedSurface};
use crate::profile::ProfileConfig;

pub struct PiIntegration;

impl HarnessIntegration for PiIntegration {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Pi
    }

    fn supports_mcp(&self) -> bool {
        false
    }

    fn detect(&self, env: &AppEnvironment) -> Result<HarnessDetection> {
        Ok(detect_binary(env, self.kind().binary_name()))
    }

    fn default_config_dir(&self, env: &AppEnvironment) -> std::path::PathBuf {
        env.user_home.join(".pi").join("agent")
    }

    fn paths_from_config_dir(&self, config_dir: std::path::PathBuf) -> Result<HarnessConfigPaths> {
        Ok(HarnessConfigPaths {
            instruction_target: config_dir.join("AGENTS.md"),
            skills_dir: config_dir.join("skills"),
            commands_dir: config_dir.join("prompts"),
            settings_file: config_dir.join("settings.json"),
            mcp_file: config_dir.join("settings.json"),
            config_dir,
        })
    }

    fn paths(&self, env: &AppEnvironment) -> Result<HarnessConfigPaths> {
        self.paths_from_config_dir(self.default_config_dir(env))
    }

    fn managed_surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface> {
        vec![
            ManagedSurface::file(&paths.instruction_target),
            ManagedSurface::directory(&paths.skills_dir),
            ManagedSurface::directory(&paths.commands_dir),
            ManagedSurface::preserved_file(&paths.settings_file),
        ]
    }

    fn preflight(&self, profile: &ProfileRef) -> Result<()> {
        flat_profile_commands(&profile.path).map(|_| ())
    }

    fn detect_drift(&self, active: &ProfileRef, paths: &HarnessConfigPaths) -> Result<DriftReport> {
        let mut items = Vec::new();
        let instruction_source = active.path.join("AGENTS.md");
        if !symlink_points_to(&paths.instruction_target, &instruction_source) {
            items.push(DriftItem {
                surface: "instructions".to_string(),
                detail: format!(
                    "{} is not linked to active profile",
                    paths.instruction_target.display()
                ),
            });
        }
        collect_directory_link_drift(
            "skills",
            valid_skills(&active.path)?,
            &paths.skills_dir,
            &mut items,
        )?;
        collect_directory_link_drift(
            "commands",
            flat_profile_commands(&active.path)?,
            &paths.commands_dir,
            &mut items,
        )?;
        Ok(DriftReport { items })
    }

    fn import_from_harness(&self, paths: &HarnessConfigPaths) -> Result<ProfileImport> {
        let settings = read_pi_settings(&paths.settings_file)?;
        Ok(ProfileImport {
            instruction: read_optional_string(&paths.instruction_target)?,
            skills: import_skills(&paths.skills_dir)?,
            commands: import_flat_commands(&paths.commands_dir)?,
            mcp_definitions: None,
            model_preference: ImportedPreference::new(import_pi_model_preference(&settings)),
            permission_preference: ImportedPreference::default_value(),
        })
    }

    fn apply(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
        fs::create_dir_all(&paths.config_dir)
            .with_context(|| format!("failed to create {}", paths.config_dir.display()))?;
        fs::create_dir_all(&paths.skills_dir)
            .with_context(|| format!("failed to create {}", paths.skills_dir.display()))?;
        fs::create_dir_all(&paths.commands_dir)
            .with_context(|| format!("failed to create {}", paths.commands_dir.display()))?;

        symlink_file(profile.path.join("AGENTS.md"), &paths.instruction_target)?;
        link_skills(profile, paths)?;
        link_flat_commands(profile, paths)?;
        patch_pi_settings(profile, paths)?;
        Ok(())
    }

    fn verify(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
        let instruction_source = profile.path.join("AGENTS.md");
        if !symlink_points_to(&paths.instruction_target, &instruction_source) {
            anyhow::bail!(
                "Pi instruction target {} does not point to {}",
                paths.instruction_target.display(),
                instruction_source.display()
            );
        }

        for skill in valid_skills(&profile.path)? {
            let target = paths.skills_dir.join(
                skill
                    .file_name()
                    .ok_or_else(|| anyhow::anyhow!("invalid skill path {}", skill.display()))?,
            );
            if !symlink_points_to(&target, &skill) {
                anyhow::bail!("Pi skill link {} was not applied", target.display());
            }
        }

        for command in flat_profile_commands(&profile.path)? {
            let target =
                paths.commands_dir.join(command.file_name().ok_or_else(|| {
                    anyhow::anyhow!("invalid command path {}", command.display())
                })?);
            if !symlink_points_to(&target, &command) {
                anyhow::bail!("Pi prompt link {} was not applied", target.display());
            }
        }

        let _ = read_pi_settings(&paths.settings_file)?;
        Ok(())
    }
}

fn patch_pi_settings(profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
    let profile_config = read_profile_config(&profile.path)?;
    let mut document = read_pi_settings(&paths.settings_file)?;

    if let Some(model) = non_default_string(
        profile_config.model_preference(&profile.harness_id),
        "Pi model preference",
    )? {
        patch_pi_model_preference(&mut document, &model);
    }

    write_text_atomic(
        &paths.settings_file,
        &serde_json::to_string_pretty(&document)?,
    )
    .with_context(|| format!("failed to write {}", paths.settings_file.display()))
}

fn read_profile_config(profile_path: &Path) -> Result<ProfileConfig> {
    let path = profile_path.join("config.json");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("missing or unreadable profile config at {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("invalid profile config at {}", path.display()))
}

fn read_pi_settings(path: &Path) -> Result<Map<String, Value>> {
    read_json(path)
}

fn import_pi_model_preference(settings: &Map<String, Value>) -> Value {
    let provider = settings.get("defaultProvider").and_then(Value::as_str);
    let model = settings.get("defaultModel").and_then(Value::as_str);
    match (provider, model) {
        (Some(provider), Some(model)) => json!(format!("{provider}/{model}")),
        (None, Some(model)) => json!(model),
        _ => json!("default"),
    }
}

fn patch_pi_model_preference(document: &mut Map<String, Value>, model: &str) {
    if let Some((provider, model)) = model.split_once('/') {
        if !provider.trim().is_empty() && !model.trim().is_empty() {
            document.insert("defaultProvider".to_string(), json!(provider));
            document.insert("defaultModel".to_string(), json!(model));
            return;
        }
    }
    document.insert("defaultModel".to_string(), json!(model));
}

fn non_default_string(value: Value, label: &str) -> Result<Option<String>> {
    match value {
        Value::String(value) if value == "default" => Ok(None),
        Value::String(value) => Ok(Some(value)),
        other => anyhow::bail!("{label} must be a string, got {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::integration::{HarnessConfigPaths, HarnessIntegration, ProfileImport};
    use crate::integrations::test_suite::template::HarnessTestAdapter;
    use crate::profile::ProfileConfig;
    use std::fs;
    use std::path::Path;

    #[derive(Default)]
    struct PiAdapter;

    impl HarnessTestAdapter for PiAdapter {
        fn integration(&self) -> Box<dyn HarnessIntegration> {
            Box::new(PiIntegration)
        }
        fn bin_name(&self) -> &'static str {
            "pi"
        }
        fn assert_mcp_cleared(&self, _paths: &HarnessConfigPaths) {}
        fn write_malformed_native_config(&self, paths: &HarnessConfigPaths) {
            fs::write(&paths.settings_file, "{ malformed }").unwrap();
        }
        fn supports_nested_commands(&self) -> bool {
            false
        }
        fn write_existing_native_settings(&self, paths: &HarnessConfigPaths) {
            fs::write(
                &paths.settings_file,
                r#"{"theme":"dark","defaultProvider":"anthropic","defaultModel":"old"}"#,
            )
            .unwrap();
        }
        fn assert_native_settings_preserved(&self, paths: &HarnessConfigPaths) {
            let config = fs::read_to_string(&paths.settings_file).unwrap_or_default();
            assert!(config.contains(r#""theme":"dark""#) || config.contains(r#""theme": "dark""#));
            assert!(config.contains("old"));
        }
        fn setup_native_config_for_import(&self, paths: &HarnessConfigPaths) {
            fs::write(
                &paths.settings_file,
                r#"{"defaultProvider":"anthropic","defaultModel":"claude-sonnet","theme":"dark"}"#,
            )
            .unwrap();
        }
        fn assert_imported_native_config(&self, import: &ProfileImport) {
            assert_eq!(
                import.model_preference.clone().into_value(),
                serde_json::json!("anthropic/claude-sonnet")
            );
            assert_eq!(import.permission_preference.clone().into_value(), "default");
            assert!(import.mcp_definitions.is_none());
        }
        fn setup_drift_native_config(&self, paths: &HarnessConfigPaths) {
            fs::write(
                &paths.settings_file,
                r#"{"defaultProvider":"openai","defaultModel":"drift-model"}"#,
            )
            .unwrap();
        }
        fn assert_drift_saved(&self, config: &ProfileConfig) {
            assert_eq!(config.model_preference("pi"), "openai/drift-model");
            assert_eq!(config.permission_preference("pi"), "default");
        }
        fn write_profile_config(&self, profile: &Path) {
            crate::integrations::test_suite::template::write_config(
                profile,
                r#"{
  "name": "work",
  "description": "",
  "models": {"pi": "anthropic/claude-sonnet-4"},
  "permissions": {"pi": "ignored"}
}"#,
            );
        }
        fn assert_applied_native_config(&self, paths: &HarnessConfigPaths) {
            let config = fs::read_to_string(&paths.settings_file).unwrap();
            assert!(config.contains("anthropic"));
            assert!(config.contains("claude-sonnet-4"));
            assert!(!config.contains("mcp"));
            assert!(!config.contains("ignored"));
        }
    }

    crate::define_standard_harness_tests!(PiAdapter);

    #[test]
    fn model_without_provider_preserves_existing_provider() {
        let mut document = serde_json::Map::new();
        document.insert("defaultProvider".to_string(), json!("anthropic"));

        patch_pi_model_preference(&mut document, "gpt-5");

        assert_eq!(document.get("defaultProvider"), Some(&json!("anthropic")));
        assert_eq!(document.get("defaultModel"), Some(&json!("gpt-5")));
    }
}
