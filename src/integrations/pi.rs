use anyhow::Result;
use serde_json::{json, Map, Value};

use crate::harness::artifact::{
    CommandMode, CommandsDirectory, HarnessArtifact, InstructionFile, JsonConfigFile, NativeConfig,
    PreferenceBinding, PreferenceCodec, PreferenceKind, SettingsPreferences, SkillsDirectory,
};
use crate::harness::integration::{
    AppEnvironment, HarnessConfigPaths, HarnessIntegration, ImportedPreference, ProfileRef,
};
use crate::harness::kind::HarnessKind;

pub struct PiIntegration;

impl HarnessIntegration for PiIntegration {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Pi
    }

    fn supports_mcp(&self) -> bool {
        false
    }

    fn supports_subagents(&self) -> bool {
        false
    }

    fn default_config_dir(&self, env: &AppEnvironment) -> std::path::PathBuf {
        env.user_home.join(".pi").join("agent")
    }

    fn paths_from_config_dir(&self, config_dir: std::path::PathBuf) -> Result<HarnessConfigPaths> {
        Ok(HarnessConfigPaths {
            instruction_target: config_dir.join("AGENTS.md"),
            skills_dir: config_dir.join("skills"),
            commands_dir: config_dir.join("prompts"),
            agents_dir: config_dir.join("agents"),
            settings_file: config_dir.join("settings.json"),
            mcp_file: config_dir.join("settings.json"),
            config_dir,
        })
    }

    fn artifacts(&self) -> Vec<Box<dyn HarnessArtifact>> {
        vec![
            Box::new(InstructionFile::new(|paths| &paths.instruction_target)),
            Box::new(SkillsDirectory::new(|paths| &paths.skills_dir)),
            Box::new(CommandsDirectory::new(
                |paths| &paths.commands_dir,
                CommandMode::FlatSymlink,
            )),
        ]
    }

    fn settings(&self) -> Option<Box<dyn crate::harness::artifact::HarnessSettings>> {
        Some(Box::new(
            SettingsPreferences::new(
                JsonConfigFile::new(|paths| &paths.settings_file).label("Pi settings JSON"),
            )
            .model(PreferenceBinding::Custom(Box::new(PiModelCodec))),
        ))
    }
}

struct PiModelCodec;

impl PreferenceCodec for PiModelCodec {
    fn import(&self, config: &NativeConfig) -> Result<ImportedPreference> {
        Ok(ImportedPreference::new(import_pi_model_preference(
            json_config_object(config)?,
        )))
    }

    fn apply(
        &self,
        config: &mut NativeConfig,
        profile: &ProfileRef,
        _preference_kind: PreferenceKind,
    ) -> Result<()> {
        let profile_config = crate::profile::read_profile_config(&profile.path)?;
        if let Some(model) = non_default_string(
            profile_config.model_preference(&profile.harness_id),
            "Pi model preference",
        )? {
            patch_pi_model_preference(json_config_object_mut(config)?, &model);
        }
        Ok(())
    }
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

fn json_config_object(config: &NativeConfig) -> Result<&Map<String, Value>> {
    let NativeConfig::Json(Value::Object(document)) = config else {
        anyhow::bail!("Pi JSON config must be an object");
    };
    Ok(document)
}

fn json_config_object_mut(config: &mut NativeConfig) -> Result<&mut Map<String, Value>> {
    let NativeConfig::Json(value) = config else {
        anyhow::bail!("Pi JSON config must be an object");
    };
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Pi JSON config must be an object"))
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
