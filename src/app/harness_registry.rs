use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::harness::fs::{detect_binary, is_executable_file};
use crate::harness::integration::{
    AppEnvironment, HarnessConfigPaths, HarnessDetection, HarnessIntegration, ProfileImport,
    ProfileRef,
};
use crate::harness::kind::{HarnessInstance, HarnessInstanceId, HarnessKind};
use crate::harness::managed::ManagedSurface;
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
        let key = instance.alias_key();
        Ok(self
            .all(env)?
            .into_iter()
            .map(|integration| integration.instance())
            .filter(|candidate| candidate.alias_key() == key)
            .map(|candidate| candidate.id.as_str().to_string())
            .collect())
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
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&settings)?),
    )
    .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

impl HarnessRegistry for BuiltInHarnessRegistry {
    fn all(&self, env: &AppEnvironment) -> Result<Vec<Box<dyn HarnessIntegration>>> {
        let settings = HarnessSettings::load_or_create(env)?;
        settings
            .harnesses
            .into_iter()
            .map(|(id, configured)| {
                let kind = HarnessKind::parse(&configured.harness_type)
                    .with_context(|| format!("invalid harness {id} in settings.json"))?;
                let base = integration_for_kind(kind);
                let config_dir = expand_config_dir(&configured.config_dir, env)
                    .with_context(|| format!("invalid configDir for harness {id}"))?;
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
                Ok(Box::new(ConfiguredIntegration { base, instance })
                    as Box<dyn HarnessIntegration>)
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
    fn load_or_create(env: &AppEnvironment) -> Result<Self> {
        let path = settings_path(env);
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("invalid settings file at {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let settings = Self::default_settings(env);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("failed to create {}", parent.display()))?;
                }
                fs::write(
                    &path,
                    format!("{}\n", serde_json::to_string_pretty(&settings)?),
                )
                .with_context(|| format!("failed to write {}", path.display()))?;
                Ok(settings)
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

    fn supports_skills(&self) -> bool {
        self.base.supports_skills()
    }

    fn supports_commands(&self) -> bool {
        self.base.supports_commands()
    }

    fn supports_mcp(&self) -> bool {
        self.base.supports_mcp()
    }

    fn supports_subagents(&self) -> bool {
        self.base.supports_subagents()
    }

    fn default_config_dir(&self, _env: &AppEnvironment) -> PathBuf {
        self.instance.config_dir.clone()
    }

    fn paths_from_config_dir(&self, config_dir: PathBuf) -> Result<HarnessConfigPaths> {
        self.base.paths_from_config_dir(config_dir)
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

    fn paths(&self, _env: &AppEnvironment) -> Result<HarnessConfigPaths> {
        self.base
            .paths_from_config_dir(self.instance.config_dir.clone())
    }

    fn managed_surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface> {
        self.base.managed_surfaces(paths)
    }

    fn preflight(&self, profile: &ProfileRef) -> Result<()> {
        self.base.preflight(profile)
    }

    fn detect_drift(
        &self,
        active: &ProfileRef,
        paths: &HarnessConfigPaths,
    ) -> Result<crate::harness::drift::DriftReport> {
        self.base.detect_drift(active, paths)
    }

    fn import_from_harness(&self, paths: &HarnessConfigPaths) -> Result<ProfileImport> {
        self.base.import_from_harness(paths)
    }

    fn apply(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
        self.base.apply(profile, paths)
    }

    fn verify(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
        self.base.verify(profile, paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_pi_preserves_unsupported_subagents_flag() {
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
}
