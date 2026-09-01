use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::harness::artifact::HarnessArtifact;
use crate::harness::fs::{detect_binary, is_executable_file};
use crate::harness::integration::{
    AppEnvironment, HarnessConfigPaths, HarnessDetection, HarnessIntegration,
};
use crate::harness::kind::{HarnessInstance, HarnessInstanceId, HarnessKind};
use crate::integrations::{built_in_integrations, integration_for_kind};

pub trait HarnessRegistry {
    fn all(&self, env: &AppEnvironment) -> Result<Vec<Box<dyn HarnessIntegration>>>;

    fn get(&self, env: &AppEnvironment, id: &str) -> Result<Option<Box<dyn HarnessIntegration>>> {
        Ok(self
            .all(env)?
            .into_iter()
            .find(|integration| integration.instance_id() == id))
    }

    fn supported_ids(&self, env: &AppEnvironment) -> Result<Vec<String>> {
        Ok(self
            .all(env)?
            .into_iter()
            .map(|integration| integration.instance_id().to_string())
            .collect())
    }

    fn require_id(&self, env: &AppEnvironment, id: &str) -> Result<String> {
        let ids = self.supported_ids(env)?;
        if ids.iter().any(|supported| supported == id) {
            Ok(id.to_string())
        } else {
            anyhow::bail!(
                "unsupported harness {id}; supported harnesses: {}",
                ids.join(", ")
            )
        }
    }

    fn aliases_for(
        &self,
        env: &AppEnvironment,
        integration: &dyn HarnessIntegration,
    ) -> Result<Vec<String>> {
        let instance = integration.instance();
        let key = instance.alias_key()?;
        let mut aliases = Vec::new();
        for candidate in self
            .all(env)?
            .into_iter()
            .map(|integration| integration.instance())
        {
            if candidate.alias_key()? == key {
                aliases.push(candidate.id.as_str().to_string());
            }
        }
        Ok(aliases)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct BuiltInHarnessRegistry;

pub fn settings_path(env: &AppEnvironment) -> PathBuf {
    env.lazyagents_home.join("settings.json")
}

pub fn reset_settings(env: &AppEnvironment) -> Result<PathBuf> {
    let path = settings_path(env);
    let settings = HarnessSettings::default_settings(env);
    crate::file_system::write_text_atomic(
        &path,
        &format!("{}\n", serde_json::to_string_pretty(&settings)?),
    )
    .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

pub fn ensure_settings(env: &AppEnvironment) -> Result<PathBuf> {
    let path = settings_path(env);
    if path.exists() {
        Ok(path)
    } else {
        reset_settings(env)
    }
}

impl HarnessRegistry for BuiltInHarnessRegistry {
    fn all(&self, env: &AppEnvironment) -> Result<Vec<Box<dyn HarnessIntegration>>> {
        let settings = HarnessSettings::load(env)?;
        settings
            .harnesses
            .into_iter()
            .map(|(id, configured)| {
                let kind = HarnessKind::parse(&configured.harness_type)
                    .with_context(|| format!("invalid harness {id} in settings.json"))?;
                let base = integration_for_kind(kind);
                let config_dir = expand_config_dir(&configured.config_dir, env)
                    .with_context(|| format!("invalid configDir for harness {id}"))?;
                let configured_identity = crate::file_system::resolve_path_identity(&config_dir)
                    .with_context(|| format!("failed to resolve configDir for harness {id}"))?;
                let default_config_dir = base.default_config_dir(env);
                let default_identity =
                    crate::file_system::resolve_path_identity(&default_config_dir)?;
                let custom_config_dir = configured_identity != default_identity;
                let config_dir = if custom_config_dir {
                    config_dir
                } else {
                    default_config_dir
                };
                let instance = HarnessInstance {
                    id: HarnessInstanceId::parse(id)?,
                    kind,
                    display_name: configured
                        .display_name
                        .unwrap_or_else(|| kind.display_name().to_string()),
                    binary: configured
                        .binary
                        .unwrap_or_else(|| kind.binary_name().to_string()),
                    config_dir,
                };
                Ok(Box::new(ConfiguredIntegration {
                    base,
                    instance,
                    custom_config_dir,
                }) as Box<dyn HarnessIntegration>)
            })
            .collect()
    }
}

fn expand_config_dir(value: &str, env: &AppEnvironment) -> Result<PathBuf> {
    if value == "~" {
        return Ok(env.user_home.clone());
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return Ok(env.user_home.join(rest));
    }
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        anyhow::bail!("configDir must be absolute or start with ~/")
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct HarnessSettings {
    harnesses: BTreeMap<String, ConfiguredHarness>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ConfiguredHarness {
    #[serde(rename = "type")]
    harness_type: String,
    #[serde(rename = "displayName", skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    binary: Option<String>,
    #[serde(rename = "configDir")]
    config_dir: String,
}

impl HarnessSettings {
    fn load(env: &AppEnvironment) -> Result<Self> {
        let path = settings_path(env);
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("invalid settings file at {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::default_settings(env))
            }
            Err(error) => Err(error)
                .with_context(|| format!("failed to read settings file at {}", path.display())),
        }
    }

    fn default_settings(env: &AppEnvironment) -> Self {
        let harnesses = built_in_integrations()
            .into_iter()
            .map(|integration| {
                let kind = integration.kind();
                (
                    kind.id().to_string(),
                    ConfiguredHarness::default_for(
                        kind,
                        config_dir_setting_value(&integration.default_config_dir(env), env),
                    ),
                )
            })
            .collect();
        Self { harnesses }
    }
}

impl ConfiguredHarness {
    fn default_for(kind: HarnessKind, config_dir: String) -> Self {
        Self {
            harness_type: kind.id().to_string(),
            display_name: Some(kind.display_name().to_string()),
            binary: Some(kind.binary_name().to_string()),
            config_dir,
        }
    }
}

fn config_dir_setting_value(path: &Path, env: &AppEnvironment) -> String {
    if let Ok(relative) = path.strip_prefix(&env.user_home) {
        if relative.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", relative.display())
        }
    } else {
        path.display().to_string()
    }
}

struct ConfiguredIntegration {
    base: Box<dyn HarnessIntegration>,
    instance: HarnessInstance,
    custom_config_dir: bool,
}

impl HarnessIntegration for ConfiguredIntegration {
    fn kind(&self) -> HarnessKind {
        self.instance.kind
    }

    fn instance(&self) -> HarnessInstance {
        self.instance.clone()
    }

    fn instance_id(&self) -> &str {
        self.instance.id.as_str()
    }

    fn display_name(&self) -> &str {
        &self.instance.display_name
    }

    fn default_config_dir(&self, _env: &AppEnvironment) -> PathBuf {
        self.instance.config_dir.clone()
    }

    fn paths_from_config_dir(&self, config_dir: PathBuf) -> Result<HarnessConfigPaths> {
        if self.custom_config_dir {
            self.base.paths_from_custom_config_dir(config_dir)
        } else {
            self.base.paths_from_config_dir(config_dir)
        }
    }

    fn detect(&self, env: &AppEnvironment) -> Result<HarnessDetection> {
        if Path::new(&self.instance.binary).is_absolute() {
            let path = PathBuf::from(&self.instance.binary);
            if is_executable_file(&path) {
                Ok(HarnessDetection::Detected { binary_path: path })
            } else {
                Ok(HarnessDetection::NotDetected)
            }
        } else {
            Ok(detect_binary(env, &self.instance.binary))
        }
    }

    fn artifacts(&self) -> Vec<Box<dyn HarnessArtifact>> {
        self.base.artifacts()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::artifact::{ArtifactContext, ArtifactKind};
    use crate::profile::ProfileName;

    struct ContextArtifact;

    impl HarnessArtifact for ContextArtifact {
        fn kind(&self) -> ArtifactKind {
            ArtifactKind::Instructions
        }

        fn surfaces(
            &self,
            _paths: &HarnessConfigPaths,
        ) -> Vec<crate::harness::managed::ManagedSurface> {
            Vec::new()
        }

        fn verify(
            &self,
            ctx: &ArtifactContext<'_>,
            _profile: &crate::harness::integration::ProfileRef,
        ) -> Result<()> {
            anyhow::bail!("{} verification failed", ctx.display_name)
        }
    }

    struct ContextIntegration;

    impl HarnessIntegration for ContextIntegration {
        fn kind(&self) -> HarnessKind {
            HarnessKind::Codex
        }

        fn default_config_dir(&self, env: &AppEnvironment) -> PathBuf {
            env.user_home.join("context")
        }

        fn paths_from_config_dir(&self, config_dir: PathBuf) -> Result<HarnessConfigPaths> {
            Ok(HarnessConfigPaths {
                instruction_target: config_dir.join("instructions"),
                skills_dir: config_dir.join("skills"),
                commands_dir: config_dir.join("commands"),
                agents_dir: config_dir.join("agents"),
                settings_file: config_dir.join("settings"),
                mcp_file: config_dir.join("mcp"),
                config_dir,
            })
        }

        fn artifacts(&self) -> Vec<Box<dyn HarnessArtifact>> {
            vec![Box::new(ContextArtifact)]
        }
    }

    #[test]
    fn configured_pi_derives_unsupported_subagents_capability() {
        let temp = tempfile::tempdir().unwrap();
        let env = AppEnvironment {
            lazyagents_home: temp.path().join("lazyagents"),
            user_home: temp.path().join("user"),
            path_entries: Vec::new(),
        };
        let registry = BuiltInHarnessRegistry;

        let pi = registry
            .all(&env)
            .unwrap()
            .into_iter()
            .find(|integration| integration.instance_id() == "pi")
            .unwrap();

        assert!(!pi.supports_subagents());
    }

    #[test]
    fn registry_uses_in_memory_defaults_without_creating_settings() {
        let temp = tempfile::tempdir().unwrap();
        let env = AppEnvironment {
            lazyagents_home: temp.path().join("lazyagents"),
            user_home: temp.path().join("user"),
            path_entries: Vec::new(),
        };

        let integrations = BuiltInHarnessRegistry.all(&env).unwrap();

        assert!(!integrations.is_empty());
        assert!(!settings_path(&env).exists());
    }

    #[test]
    fn ensure_settings_materializes_in_memory_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let env = AppEnvironment {
            lazyagents_home: temp.path().join("lazyagents"),
            user_home: temp.path().join("user"),
            path_entries: Vec::new(),
        };

        let path = ensure_settings(&env).unwrap();

        assert_eq!(path, settings_path(&env));
        assert!(path.is_file());
        assert_eq!(BuiltInHarnessRegistry.all(&env).unwrap().len(), 5);
    }

    #[cfg(unix)]
    #[test]
    fn claude_default_path_spellings_keep_the_default_mcp_layout() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let env = AppEnvironment {
            lazyagents_home: temp.path().join("lazyagents"),
            user_home: temp.path().join("user"),
            path_entries: Vec::new(),
        };
        fs::create_dir_all(env.user_home.join(".claude")).unwrap();
        symlink(
            env.user_home.join(".claude"),
            temp.path().join("claude-link"),
        )
        .unwrap();
        fs::create_dir_all(&env.lazyagents_home).unwrap();
        let default_mcp = env.user_home.join(".claude.json");
        let spellings = vec![
            "~/.claude".to_string(),
            env.user_home.join(".claude").display().to_string(),
            env.user_home
                .join("unused/../.claude")
                .display()
                .to_string(),
            temp.path().join("claude-link").display().to_string(),
        ];
        for config_dir in spellings {
            fs::write(
                settings_path(&env),
                serde_json::json!({"harnesses":{"claude":{"type":"claude","configDir":config_dir}}})
                    .to_string(),
            )
            .unwrap();
            let claude = BuiltInHarnessRegistry.get(&env, "claude").unwrap().unwrap();
            assert_eq!(claude.paths(&env).unwrap().mcp_file, default_mcp);
        }

        let custom = temp.path().join("claude.work.v2");
        fs::write(
            settings_path(&env),
            serde_json::json!({"harnesses":{"claude":{"type":"claude","configDir":custom}}})
                .to_string(),
        )
        .unwrap();
        let claude = BuiltInHarnessRegistry.get(&env, "claude").unwrap().unwrap();
        assert_eq!(
            claude.paths(&env).unwrap().mcp_file,
            custom.join(".claude.json")
        );
    }

    #[test]
    fn configured_lifecycle_uses_instance_display_name() {
        let temp = tempfile::tempdir().unwrap();
        let integration = ConfiguredIntegration {
            base: Box::new(ContextIntegration),
            instance: HarnessInstance {
                id: HarnessInstanceId::parse("codex-work").unwrap(),
                kind: HarnessKind::Codex,
                display_name: "Work Codex".to_string(),
                binary: "codex".to_string(),
                config_dir: temp.path().join("codex"),
            },
            custom_config_dir: true,
        };
        let paths = integration
            .paths_from_config_dir(temp.path().join("codex"))
            .unwrap();
        let profile = crate::harness::integration::ProfileRef {
            name: ProfileName::parse("work").unwrap(),
            path: temp.path().join("profile"),
            harness_id: "codex-work".to_string(),
        };

        let error = integration.verify(&profile, &paths).unwrap_err();

        assert_eq!(error.to_string(), "Work Codex verification failed");
    }
}
