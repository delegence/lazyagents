use std::env;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::{json, Value};

use crate::harness::artifact::{
    merge_profile_import, ArtifactContext, ArtifactKind, HarnessArtifact,
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImportedDirectory {
    pub name: String,
    pub unix_mode: Option<u32>,
    pub files: Vec<ImportedFile>,
    pub directories: Vec<ImportedDirectoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImportedDirectoryEntry {
    pub relative_path: PathBuf,
    pub unix_mode: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImportedFile {
    pub relative_path: PathBuf,
    pub contents: Vec<u8>,
    pub unix_mode: Option<u32>,
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
        self.supports_artifact(ArtifactKind::Skills)
    }
    fn supports_commands(&self) -> bool {
        self.supports_artifact(ArtifactKind::Commands)
    }
    fn supports_mcp(&self) -> bool {
        self.supports_artifact(ArtifactKind::Mcp)
    }
    fn supports_subagents(&self) -> bool {
        self.supports_artifact(ArtifactKind::Subagents)
    }
    fn default_config_dir(&self, env: &AppEnvironment) -> PathBuf;
    fn paths_from_config_dir(&self, config_dir: PathBuf) -> Result<HarnessConfigPaths>;
    fn paths_from_custom_config_dir(&self, config_dir: PathBuf) -> Result<HarnessConfigPaths> {
        self.paths_from_config_dir(config_dir)
    }
    fn artifacts(&self) -> Vec<Box<dyn HarnessArtifact>> {
        Vec::new()
    }
    fn supports_artifact(&self, kind: ArtifactKind) -> bool {
        self.artifacts()
            .iter()
            .any(|artifact| artifact.kind() == kind)
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
        surfaces
    }
    fn preflight(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
        let ctx = self.artifact_context(paths);
        for artifact in self.checked_artifacts()? {
            artifact.preflight(&ctx, profile)?;
        }
        Ok(())
    }
    fn detect_drift(&self, active: &ProfileRef, paths: &HarnessConfigPaths) -> Result<DriftReport> {
        let ctx = self.artifact_context(paths);
        let mut items = Vec::new();
        for artifact in self.checked_artifacts()? {
            items.extend(artifact.detect_drift(&ctx, active)?);
        }
        Ok(DriftReport { items })
    }
    fn import_from_harness(&self, paths: &HarnessConfigPaths) -> Result<ProfileImport> {
        let ctx = self.artifact_context(paths);
        let mut import = ProfileImport::default();
        for artifact in self.checked_artifacts()? {
            merge_profile_import(&mut import, artifact.import(&ctx)?);
        }
        Ok(import)
    }
    fn apply(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
        std::fs::create_dir_all(&paths.config_dir)?;
        let ctx = self.artifact_context(paths);
        for artifact in self.checked_artifacts()? {
            artifact.apply(&ctx, profile)?;
        }
        Ok(())
    }
    fn verify(&self, profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
        let ctx = self.artifact_context(paths);
        for artifact in self.checked_artifacts()? {
            artifact.verify(&ctx, profile)?;
        }
        Ok(())
    }

    fn artifact_context<'a>(&'a self, paths: &'a HarnessConfigPaths) -> ArtifactContext<'a> {
        ArtifactContext {
            display_name: self.display_name(),
            paths,
        }
    }

    fn checked_artifacts(&self) -> Result<Vec<Box<dyn HarnessArtifact>>> {
        let artifacts = self.artifacts();
        let mut kinds = std::collections::BTreeSet::new();
        for artifact in &artifacts {
            if !kinds.insert(artifact.kind()) {
                anyhow::bail!(
                    "{} declares more than one {:?} artifact",
                    self.kind(),
                    artifact.kind()
                );
            }
        }
        Ok(artifacts)
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
    use crate::harness::artifact::{ArtifactContext, ArtifactKind, HarnessArtifact};
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
        fn kind(&self) -> ArtifactKind {
            match self.name {
                "first" => ArtifactKind::Instructions,
                "second" => ArtifactKind::Skills,
                _ => ArtifactKind::Settings,
            }
        }

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

    struct DummyIntegration {
        events: Rc<RefCell<Vec<String>>>,
        duplicate_settings: bool,
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
            let mut artifacts: Vec<Box<dyn HarnessArtifact>> = vec![
                Box::new(RecordingArtifact::new("first", self.events.clone())),
                Box::new(RecordingArtifact::new("second", self.events.clone())),
                Box::new(RecordingArtifact::new("settings", self.events.clone())),
            ];
            if self.duplicate_settings {
                artifacts.push(Box::new(RecordingArtifact::new(
                    "settings-copy",
                    self.events.clone(),
                )));
            }
            artifacts
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
    fn default_lifecycle_runs_artifacts_in_declared_order() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let integration = DummyIntegration {
            events: events.clone(),
            duplicate_settings: false,
        };
        let paths = integration
            .paths_from_config_dir(tempfile::tempdir().unwrap().path().join("config"))
            .unwrap();
        let profile = profile_ref();

        let surfaces = integration.managed_surfaces(&paths);
        assert_eq!(surfaces.len(), 3);

        integration.preflight(&profile, &paths).unwrap();
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

    #[test]
    fn duplicate_artifact_kinds_are_rejected() {
        let integration = DummyIntegration {
            events: Rc::new(RefCell::new(Vec::new())),
            duplicate_settings: true,
        };
        let paths = integration
            .paths_from_config_dir(tempfile::tempdir().unwrap().path().join("config"))
            .unwrap();

        let error = integration.preflight(&profile_ref(), &paths).unwrap_err();

        assert!(error
            .to_string()
            .contains("declares more than one Settings artifact"));
    }
}
