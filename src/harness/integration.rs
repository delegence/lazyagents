use std::env;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::{json, Value};

use crate::harness::artifact::{
    merge_profile_import, ArtifactContext, HarnessArtifact, HarnessSettings,
};
use crate::harness::drift::DriftReport;
use crate::harness::fs::detect_binary;
use crate::harness::kind::{HarnessInstance, HarnessKind};
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
    pub agents_dir: PathBuf,
    pub settings_file: PathBuf,
    pub mcp_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ProfileRef {
    pub name: ProfileName,
    pub path: PathBuf,
    pub harness_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct ProfileImport {
    pub instruction: Option<String>,
    pub skills: Vec<ImportedDirectory>,
    pub commands: Vec<ImportedFile>,
    pub agents: Option<Vec<ImportedFile>>,
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
    fn instance(&self) -> HarnessInstance {
        let kind = self.kind();
        HarnessInstance {
            id: crate::harness::kind::HarnessInstanceId::parse(kind.id()).unwrap(),
            kind,
            display_name: kind.display_name().to_string(),
            binary: kind.binary_name().to_string(),
            config_dir: PathBuf::new(),
        }
    }
    fn instance_id(&self) -> &str {
        self.kind().id()
    }
    fn display_name(&self) -> &str {
        self.kind().display_name()
    }
    fn supports_skills(&self) -> bool {
        true
    }
    fn supports_commands(&self) -> bool {
        true
    }
    fn supports_mcp(&self) -> bool {
        true
    }
    fn supports_subagents(&self) -> bool {
        true
    }
    fn default_config_dir(&self, env: &AppEnvironment) -> PathBuf;
    fn paths_from_config_dir(&self, config_dir: PathBuf) -> Result<HarnessConfigPaths>;
    fn artifacts(&self) -> Vec<Box<dyn HarnessArtifact>> {
        Vec::new()
    }
    fn settings(&self) -> Option<Box<dyn HarnessSettings>> {
        None
    }
    fn detect(&self, env: &AppEnvironment) -> Result<HarnessDetection> {
        Ok(detect_binary(env, self.kind().binary_name()))
    }
    fn paths(&self, env: &AppEnvironment) -> Result<HarnessConfigPaths> {
        self.paths_from_config_dir(self.default_config_dir(env))
    }
    fn managed_surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface> {
        let mut surfaces = Vec::new();
        for artifact in self.artifacts() {
            extend_unique_surfaces(&mut surfaces, artifact.surfaces(paths));
        }
        if let Some(settings) = self.settings() {
            extend_unique_surfaces(&mut surfaces, settings.surfaces(paths));
        }
        surfaces
    }
    fn preflight(&self, profile: &ProfileRef) -> Result<()> {
        let paths = self.paths_from_config_dir(PathBuf::new())?;
        let ctx = self.artifact_context(None, &paths);
        for artifact in self.artifacts() {
            artifact.preflight(&ctx, profile)?;
        }
        if let Some(settings) = self.settings() {
            settings.preflight(&ctx, profile)?;
        }
        Ok(())
    }
    fn detect_drift(&self, active: &ProfileRef, paths: &HarnessConfigPaths) -> Result<DriftReport> {
        let ctx = self.artifact_context(None, paths);
        let mut items = Vec::new();
        for artifact in self.artifacts() {
            items.extend(artifact.detect_drift(&ctx, active)?);
        }
        if let Some(settings) = self.settings() {
            items.extend(settings.detect_drift(&ctx, active)?);
        }
        Ok(DriftReport { items })
    }
    fn import_from_harness(&self, paths: &HarnessConfigPaths) -> Result<ProfileImport> {
        let ctx = self.artifact_context(None, paths);
        let mut import = ProfileImport::default();
        for artifact in self.artifacts() {
            merge_profile_import(&mut import, artifact.import(&ctx)?);
        }
        if let Some(settings) = self.settings() {
            merge_profile_import(&mut import, settings.import(&ctx)?);
        }
        Ok(import)
    }
    fn apply(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
        std::fs::create_dir_all(&paths.config_dir)?;
        let ctx = self.artifact_context(None, paths);
        for artifact in self.artifacts() {
            artifact.apply(&ctx, profile)?;
        }
        if let Some(settings) = self.settings() {
            settings.apply(&ctx, profile)?;
        }
        Ok(())
    }
    fn verify(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
        let ctx = self.artifact_context(None, paths);
        for artifact in self.artifacts() {
            artifact.verify(&ctx, profile)?;
        }
        if let Some(settings) = self.settings() {
            settings.verify(&ctx, profile)?;
        }
        Ok(())
    }

    fn artifact_context<'a>(
        &'a self,
        env: Option<&'a AppEnvironment>,
        paths: &'a HarnessConfigPaths,
    ) -> ArtifactContext<'a> {
        let context = ArtifactContext {
            env,
            kind: self.kind(),
            display_name: self.display_name(),
            paths,
        };
        let _ = context.env;
        let _ = context.kind;
        context
    }
}

fn extend_unique_surfaces(surfaces: &mut Vec<ManagedSurface>, additions: Vec<ManagedSurface>) {
    for surface in additions {
        if !surfaces
            .iter()
            .any(|existing| existing.path == surface.path && existing.kind == surface.kind)
        {
            surfaces.push(surface);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::artifact::{ArtifactContext, HarnessArtifact, HarnessSettings};
    use crate::harness::drift::DriftItem;
    use crate::harness::managed::ManagedSurface;
    use crate::profile::ProfileName;
    use anyhow::Result;
    use serde_json::json;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Clone)]
    struct RecordingArtifact {
        name: &'static str,
        events: Rc<RefCell<Vec<String>>>,
        surface: PathBuf,
    }

    impl RecordingArtifact {
        fn new(name: &'static str, events: Rc<RefCell<Vec<String>>>) -> Self {
            Self {
                name,
                events,
                surface: PathBuf::from(format!("/{name}")),
            }
        }
    }

    impl HarnessArtifact for RecordingArtifact {
        fn surfaces(&self, _paths: &HarnessConfigPaths) -> Vec<ManagedSurface> {
            vec![ManagedSurface::file(&self.surface)]
        }

        fn preflight(&self, _ctx: &ArtifactContext<'_>, _profile: &ProfileRef) -> Result<()> {
            self.events
                .borrow_mut()
                .push(format!("{}:preflight", self.name));
            Ok(())
        }

        fn detect_drift(
            &self,
            _ctx: &ArtifactContext<'_>,
            _profile: &ProfileRef,
        ) -> Result<Vec<DriftItem>> {
            self.events
                .borrow_mut()
                .push(format!("{}:drift", self.name));
            Ok(vec![DriftItem {
                surface: self.name.to_string(),
                detail: "changed".to_string(),
            }])
        }

        fn import(&self, _ctx: &ArtifactContext<'_>) -> Result<ProfileImport> {
            self.events
                .borrow_mut()
                .push(format!("{}:import", self.name));
            Ok(ProfileImport {
                instruction: Some(self.name.to_string()),
                model_preference: ImportedPreference::new(json!(self.name)),
                ..ProfileImport::default()
            })
        }

        fn apply(&self, _ctx: &ArtifactContext<'_>, _profile: &ProfileRef) -> Result<()> {
            self.events
                .borrow_mut()
                .push(format!("{}:apply", self.name));
            Ok(())
        }

        fn verify(&self, _ctx: &ArtifactContext<'_>, _profile: &ProfileRef) -> Result<()> {
            self.events
                .borrow_mut()
                .push(format!("{}:verify", self.name));
            Ok(())
        }
    }

    impl HarnessSettings for RecordingArtifact {}

    struct DummyIntegration {
        events: Rc<RefCell<Vec<String>>>,
    }

    impl HarnessIntegration for DummyIntegration {
        fn kind(&self) -> HarnessKind {
            HarnessKind::Codex
        }

        fn default_config_dir(&self, _env: &AppEnvironment) -> PathBuf {
            PathBuf::from("/config")
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
            vec![
                Box::new(RecordingArtifact::new("first", self.events.clone())),
                Box::new(RecordingArtifact::new("second", self.events.clone())),
            ]
        }

        fn settings(&self) -> Option<Box<dyn HarnessSettings>> {
            Some(Box::new(RecordingArtifact::new(
                "settings",
                self.events.clone(),
            )))
        }
    }

    fn profile_ref() -> ProfileRef {
        ProfileRef {
            name: ProfileName::parse("work").unwrap(),
            path: PathBuf::from("/profile"),
            harness_id: "codex".to_string(),
        }
    }

    #[test]
    fn default_lifecycle_runs_artifacts_then_settings_in_declared_order() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let integration = DummyIntegration {
            events: events.clone(),
        };
        let paths = integration
            .paths_from_config_dir(tempfile::tempdir().unwrap().path().join("config"))
            .unwrap();
        let profile = profile_ref();

        let surfaces = integration.managed_surfaces(&paths);
        assert_eq!(surfaces.len(), 3);

        integration.preflight(&profile).unwrap();
        assert_eq!(
            events.borrow().as_slice(),
            ["first:preflight", "second:preflight", "settings:preflight"]
        );
        events.borrow_mut().clear();

        let drift = integration.detect_drift(&profile, &paths).unwrap();
        assert_eq!(drift.items.len(), 3);
        assert_eq!(
            events.borrow().as_slice(),
            ["first:drift", "second:drift", "settings:drift"]
        );
        events.borrow_mut().clear();

        let import = integration.import_from_harness(&paths).unwrap();
        assert_eq!(import.instruction.as_deref(), Some("settings"));
        assert_eq!(import.model_preference.into_value(), json!("settings"));
        assert_eq!(
            events.borrow().as_slice(),
            ["first:import", "second:import", "settings:import"]
        );
        events.borrow_mut().clear();

        integration.apply(&profile, &paths).unwrap();
        integration.verify(&profile, &paths).unwrap();
        assert_eq!(
            events.borrow().as_slice(),
            [
                "first:apply",
                "second:apply",
                "settings:apply",
                "first:verify",
                "second:verify",
                "settings:verify",
            ]
        );
    }
}
