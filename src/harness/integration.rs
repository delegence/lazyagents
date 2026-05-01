use std::env;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::{json, Value};

use crate::harness::drift::DriftReport;
use crate::harness::kind::HarnessKind;
use crate::harness::managed::ManagedSurface;
use crate::profile::ProfileName;

#[derive(Debug, Clone)]
pub struct AppEnvironment {
    pub lazyagents_home: PathBuf,
    pub user_home: PathBuf,
    pub path_entries: Vec<PathBuf>,
}

impl AppEnvironment {
    pub fn resolve(lazyagents_home: PathBuf) -> Result<Self> {
        let user_home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
        let path_entries = env::var_os("PATH")
            .map(|path| env::split_paths(&path).collect())
            .unwrap_or_default();

        Ok(Self {
            lazyagents_home,
            user_home,
            path_entries,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessDetection {
    Detected { binary_path: PathBuf },
    NotDetected,
}

#[derive(Debug, Clone)]
pub struct HarnessConfigPaths {
    pub config_dir: PathBuf,
    pub instruction_target: PathBuf,
    pub skills_dir: PathBuf,
    pub commands_dir: PathBuf,
    pub settings_file: PathBuf,
    pub mcp_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ProfileRef {
    pub name: ProfileName,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct ProfileImport {
    pub instruction: Option<String>,
    pub skills: Vec<ImportedDirectory>,
    pub commands: Vec<ImportedFile>,
    pub mcp_definitions: Option<String>,
    pub model_preference: ImportedPreference,
    pub permission_preference: ImportedPreference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedDirectory {
    pub name: String,
    pub files: Vec<ImportedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedFile {
    pub relative_path: PathBuf,
    pub contents: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportedPreference(Value);

impl ImportedPreference {
    pub fn new(value: Value) -> Self {
        Self(value)
    }

    pub fn default_value() -> Self {
        Self(json!("default"))
    }

    pub fn into_value(self) -> Value {
        self.0
    }
}

impl Default for ImportedPreference {
    fn default() -> Self {
        Self::default_value()
    }
}

pub trait HarnessIntegration {
    fn kind(&self) -> HarnessKind;
    fn supports_skills(&self) -> bool {
        true
    }
    fn supports_commands(&self) -> bool {
        true
    }
    fn supports_mcp(&self) -> bool {
        true
    }
    fn detect(&self, env: &AppEnvironment) -> Result<HarnessDetection>;
    fn paths(&self, env: &AppEnvironment) -> Result<HarnessConfigPaths>;
    fn managed_surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface>;
    fn preflight(&self, profile: &ProfileRef) -> Result<()>;
    fn detect_drift(&self, active: &ProfileRef, paths: &HarnessConfigPaths) -> Result<DriftReport>;
    fn import_from_harness(&self, paths: &HarnessConfigPaths) -> Result<ProfileImport>;
    fn apply(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()>;
    fn verify(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()>;
}
