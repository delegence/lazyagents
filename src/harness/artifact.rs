use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::harness::agents::{
    apply_rendered_agents, collect_rendered_agent_drift, profile_agents, sub_agent_import_file,
    verify_rendered_agents, RenderedAgent, SubAgent,
};
use crate::harness::commands::{
    collect_commands_drift_recursive, collect_flat_commands_drift, copy_commands,
    copy_flat_commands, flat_profile_commands, import_commands, import_flat_commands,
    profile_commands_recursive,
};
use crate::harness::drift::DriftItem;
use crate::harness::fs::{
    collect_instruction_content_drift, read_optional_string, verify_profile_instructions,
    write_profile_instructions,
};
use crate::harness::integration::{
    HarnessConfigPaths, ImportedPreference, ProfileImport, ProfileRef,
};
use crate::harness::managed::ManagedSurface;
use crate::harness::skills::{collect_skills_drift, copy_skills, import_skills};
use crate::profile::mcp::{canonical_mcp_json, read_mcp_definitions, McpDefinition};
use crate::profile::read_profile_config;

pub type PathSelector = for<'a> fn(&'a HarnessConfigPaths) -> &'a Path;

pub struct ArtifactContext<'a> {
    pub display_name: &'a str,
    pub paths: &'a HarnessConfigPaths,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ArtifactKind {
    Instructions,
    Skills,
    Commands,
    Subagents,
    Mcp,
    Settings,
}

pub trait HarnessArtifact {
    fn kind(&self) -> ArtifactKind;
    fn surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface>;
    fn preflight(&self, _ctx: &ArtifactContext<'_>, _profile: &ProfileRef) -> Result<()> {
        Ok(())
    }
    fn detect_drift(
        &self,
        _ctx: &ArtifactContext<'_>,
        _profile: &ProfileRef,
    ) -> Result<Vec<DriftItem>> {
        Ok(Vec::new())
    }
    fn import(&self, _ctx: &ArtifactContext<'_>) -> Result<ProfileImport> {
        Ok(ProfileImport::default())
    }
    fn apply(&self, _ctx: &ArtifactContext<'_>, _profile: &ProfileRef) -> Result<()> {
        Ok(())
    }
    fn verify(&self, _ctx: &ArtifactContext<'_>, _profile: &ProfileRef) -> Result<()> {
        Ok(())
    }
}

pub enum NativeConfig {
    Json(Value),
    Toml(toml_edit::DocumentMut),
}

impl NativeConfig {
    pub fn json_object(&self, label: &str) -> Result<&serde_json::Map<String, Value>> {
        let Self::Json(Value::Object(document)) = self else {
            anyhow::bail!("{label} must be an object");
        };
        Ok(document)
    }

    pub fn json_object_mut(&mut self, label: &str) -> Result<&mut serde_json::Map<String, Value>> {
        let Self::Json(Value::Object(document)) = self else {
            anyhow::bail!("{label} must be an object");
        };
        Ok(document)
    }
}

pub trait NativeConfigFile {
    fn path<'a>(&self, paths: &'a HarnessConfigPaths) -> &'a Path;
    fn read(&self, paths: &HarnessConfigPaths) -> Result<NativeConfig>;
    fn write(&self, paths: &HarnessConfigPaths, config: &NativeConfig) -> Result<()>;
}

pub struct JsonConfigFile {
    path: PathSelector,
    label: &'static str,
}

impl JsonConfigFile {
    pub fn new(path: PathSelector) -> Self {
        Self {
            path,
            label: "JSON",
        }
    }

    pub fn label(mut self, label: &'static str) -> Self {
        self.label = label;
        self
    }
}

impl NativeConfigFile for JsonConfigFile {
    fn path<'a>(&self, paths: &'a HarnessConfigPaths) -> &'a Path {
        (self.path)(paths)
    }

    fn read(&self, paths: &HarnessConfigPaths) -> Result<NativeConfig> {
        let path = self.path(paths);
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(NativeConfig::Json(Value::Object(Default::default())));
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        if text.trim().is_empty() {
            return Ok(NativeConfig::Json(Value::Object(Default::default())));
        }
        let value = serde_json::from_str(&text)
            .with_context(|| format!("invalid JSON at {} ({})", path.display(), self.label))?;
        Ok(NativeConfig::Json(value))
    }

    fn write(&self, paths: &HarnessConfigPaths, config: &NativeConfig) -> Result<()> {
        let NativeConfig::Json(value) = config else {
            anyhow::bail!("cannot write TOML config to {}", self.path(paths).display());
        };
        crate::file_system::write_text_atomic(
            self.path(paths),
            &serde_json::to_string_pretty(value)?,
        )
        .with_context(|| format!("failed to write {}", self.path(paths).display()))
    }
}

pub struct TomlConfigFile {
    path: PathSelector,
    label: &'static str,
}

impl TomlConfigFile {
    pub fn new(path: PathSelector) -> Self {
        Self {
            path,
            label: "TOML",
        }
    }

    pub fn label(mut self, label: &'static str) -> Self {
        self.label = label;
        self
    }
}

impl NativeConfigFile for TomlConfigFile {
    fn path<'a>(&self, paths: &'a HarnessConfigPaths) -> &'a Path {
        (self.path)(paths)
    }

    fn read(&self, paths: &HarnessConfigPaths) -> Result<NativeConfig> {
        let path = self.path(paths);
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(NativeConfig::Toml(toml_edit::DocumentMut::new()));
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let document = text
            .parse::<toml_edit::DocumentMut>()
            .with_context(|| format!("invalid {} at {}", self.label, path.display()))?;
        Ok(NativeConfig::Toml(document))
    }

    fn write(&self, paths: &HarnessConfigPaths, config: &NativeConfig) -> Result<()> {
        let NativeConfig::Toml(document) = config else {
            anyhow::bail!("cannot write JSON config to {}", self.path(paths).display());
        };
        crate::file_system::write_text_atomic(self.path(paths), &document.to_string())
            .with_context(|| format!("failed to write {}", self.path(paths).display()))
    }
}

pub trait McpCodec {
    fn import(&self, config: &NativeConfig) -> Result<Vec<McpDefinition>>;
    fn apply(&self, config: &mut NativeConfig, definitions: &[McpDefinition]) -> Result<()>;
    fn preflight_apply(
        &self,
        _config: &NativeConfig,
        _definitions: &[McpDefinition],
    ) -> Result<()> {
        Ok(())
    }
}

pub struct McpConfig<C> {
    config_file: Box<dyn NativeConfigFile>,
    codec: C,
}

impl<C> McpConfig<C> {
    pub fn new(config_file: impl NativeConfigFile + 'static, codec: C) -> Self {
        Self {
            config_file: Box::new(config_file),
            codec,
        }
    }
}

impl<C: McpCodec> HarnessArtifact for McpConfig<C> {
    fn kind(&self) -> ArtifactKind {
        ArtifactKind::Mcp
    }

    fn surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface> {
        vec![ManagedSurface::preserved_file(self.config_file.path(paths))]
    }

    fn preflight(&self, ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
        let config = self.config_file.read(ctx.paths)?;
        let definitions = read_mcp_definitions(&profile.path)?;
        self.codec.preflight_apply(&config, &definitions)
    }

    fn detect_drift(
        &self,
        ctx: &ArtifactContext<'_>,
        profile: &ProfileRef,
    ) -> Result<Vec<DriftItem>> {
        let config = self.config_file.read(ctx.paths)?;
        let native_mcps = self.codec.import(&config)?;
        let profile_mcps = read_mcp_definitions(&profile.path)?;
        if canonical_mcp_json(&native_mcps)? == canonical_mcp_json(&profile_mcps)? {
            return Ok(Vec::new());
        }
        Ok(vec![DriftItem {
            surface: "mcp".to_string(),
            detail: format!("{} MCP list differs from active profile", ctx.display_name),
        }])
    }

    fn import(&self, ctx: &ArtifactContext<'_>) -> Result<ProfileImport> {
        let config = self.config_file.read(ctx.paths)?;
        Ok(ProfileImport {
            mcp_definitions: Some(canonical_mcp_json(&self.codec.import(&config)?)?),
            ..ProfileImport::default()
        })
    }

    fn apply(&self, ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
        let mut config = self.config_file.read(ctx.paths)?;
        let definitions = read_mcp_definitions(&profile.path)?;
        self.codec.apply(&mut config, &definitions)?;
        self.config_file.write(ctx.paths, &config)
    }

    fn verify(&self, ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
        let config = self.config_file.read(ctx.paths)?;
        let native_mcps = self.codec.import(&config)?;
        let profile_mcps = read_mcp_definitions(&profile.path)?;
        if canonical_mcp_json(&native_mcps)? != canonical_mcp_json(&profile_mcps)? {
            anyhow::bail!(
                "{} MCP config does not match profile MCP definitions",
                ctx.display_name
            );
        }
        Ok(())
    }
}

pub trait PreferenceCodec {
    fn import(&self, config: &NativeConfig) -> Result<ImportedPreference>;
    fn apply(
        &self,
        config: &mut NativeConfig,
        profile: &ProfileRef,
        preference_kind: PreferenceKind,
    ) -> Result<()>;
    fn preflight(&self, _expected: Value) -> Result<()> {
        Ok(())
    }
    fn verify(&self, config: &NativeConfig, expected: Value) -> Result<()> {
        let Some(expected) = non_default_value(expected) else {
            return Ok(());
        };
        if self.import(config)?.into_value() != expected {
            anyhow::bail!("applied custom preference does not match the profile");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PreferenceKind {
    Model,
    Permission,
}

pub enum PreferenceBinding {
    JsonStringPointer { pointer: &'static str },
    TomlKey { key: &'static str },
    Custom(Box<dyn PreferenceCodec>),
}

pub struct SettingsPreferences {
    config_file: Box<dyn NativeConfigFile>,
    model: Option<PreferenceBinding>,
    permission: Option<PreferenceBinding>,
}

impl SettingsPreferences {
    pub fn new(config_file: impl NativeConfigFile + 'static) -> Self {
        Self {
            config_file: Box::new(config_file),
            model: None,
            permission: None,
        }
    }

    pub fn model(mut self, binding: PreferenceBinding) -> Self {
        self.model = Some(binding);
        self
    }

    pub fn permission(mut self, binding: PreferenceBinding) -> Self {
        self.permission = Some(binding);
        self
    }
}

impl HarnessArtifact for SettingsPreferences {
    fn kind(&self) -> ArtifactKind {
        ArtifactKind::Settings
    }

    fn surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface> {
        vec![ManagedSurface::preserved_file(self.config_file.path(paths))]
    }

    fn import(&self, ctx: &ArtifactContext<'_>) -> Result<ProfileImport> {
        let config = self.config_file.read(ctx.paths)?;
        Ok(ProfileImport {
            model_preference: import_preference(self.model.as_ref(), &config)?,
            permission_preference: import_preference(self.permission.as_ref(), &config)?,
            ..ProfileImport::default()
        })
    }

    fn apply(&self, ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
        let mut config = self.config_file.read(ctx.paths)?;
        let profile_config = read_profile_config(&profile.path)?;
        if let Some(binding) = &self.model {
            let value = profile_config.model_preference(&profile.harness_id);
            apply_preference(binding, &mut config, profile, PreferenceKind::Model, value)?;
        }
        if let Some(binding) = &self.permission {
            let value = profile_config.permission_preference(&profile.harness_id);
            apply_preference(
                binding,
                &mut config,
                profile,
                PreferenceKind::Permission,
                value,
            )?;
        }
        self.config_file.write(ctx.paths, &config)
    }

    fn preflight(&self, _ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
        let config = read_profile_config(&profile.path)?;
        for (binding, value, label) in [
            (
                self.model.as_ref(),
                config.model_preference(&profile.harness_id),
                "model preference",
            ),
            (
                self.permission.as_ref(),
                config.permission_preference(&profile.harness_id),
                "permission preference",
            ),
        ] {
            if let Some(PreferenceBinding::Custom(codec)) = binding {
                codec.preflight(value.clone())?;
            }
            if matches!(
                binding,
                Some(
                    PreferenceBinding::JsonStringPointer { .. } | PreferenceBinding::TomlKey { .. }
                )
            ) {
                non_default_string(value, label)?;
            }
        }
        Ok(())
    }

    fn verify(&self, ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
        let native = self.config_file.read(ctx.paths)?;
        let profile_config = read_profile_config(&profile.path)?;
        verify_direct_preference(
            self.model.as_ref(),
            &native,
            profile_config.model_preference(&profile.harness_id),
            "model preference",
        )?;
        verify_direct_preference(
            self.permission.as_ref(),
            &native,
            profile_config.permission_preference(&profile.harness_id),
            "permission preference",
        )
    }
}

fn import_preference(
    binding: Option<&PreferenceBinding>,
    config: &NativeConfig,
) -> Result<ImportedPreference> {
    let Some(binding) = binding else {
        return Ok(ImportedPreference::default_value());
    };
    match binding {
        PreferenceBinding::JsonStringPointer { pointer } => {
            let NativeConfig::Json(value) = config else {
                anyhow::bail!("JSON pointer preference requires JSON config");
            };
            match value.pointer(pointer) {
                Some(Value::String(value)) => {
                    Ok(ImportedPreference::new(Value::String(value.clone())))
                }
                Some(other) => {
                    anyhow::bail!("native preference at {pointer} must be a string, got {other}")
                }
                None => Ok(ImportedPreference::default_value()),
            }
        }
        PreferenceBinding::TomlKey { key } => {
            let NativeConfig::Toml(document) = config else {
                anyhow::bail!("TOML key preference requires TOML config");
            };
            match document.get(key) {
                Some(item) => item
                    .as_str()
                    .map(|value| ImportedPreference::new(Value::String(value.to_string())))
                    .ok_or_else(|| {
                        anyhow::anyhow!("native TOML preference {key} must be a string")
                    }),
                None => Ok(ImportedPreference::default_value()),
            }
        }
        PreferenceBinding::Custom(codec) => codec.import(config),
    }
}

fn apply_preference(
    binding: &PreferenceBinding,
    config: &mut NativeConfig,
    profile: &ProfileRef,
    kind: PreferenceKind,
    value: Value,
) -> Result<()> {
    match binding {
        PreferenceBinding::JsonStringPointer { pointer } => {
            let Some(value) = non_default_string(value, "JSON preference")? else {
                return Ok(());
            };
            let NativeConfig::Json(config) = config else {
                anyhow::bail!("JSON pointer preference requires JSON config");
            };
            set_json_pointer(config, pointer, Value::String(value))
        }
        PreferenceBinding::TomlKey { key } => {
            let Some(value) = non_default_string(value, "TOML preference")? else {
                return Ok(());
            };
            let NativeConfig::Toml(document) = config else {
                anyhow::bail!("TOML key preference requires TOML config");
            };
            document[key] = toml_edit::value(value);
            Ok(())
        }
        PreferenceBinding::Custom(codec) => codec.apply(config, profile, kind),
    }
}

fn verify_direct_preference(
    binding: Option<&PreferenceBinding>,
    config: &NativeConfig,
    expected: Value,
    label: &str,
) -> Result<()> {
    let Some(binding) = binding else {
        return Ok(());
    };
    if non_default_value(expected.clone()).is_none() {
        return Ok(());
    }
    if let PreferenceBinding::Custom(codec) = binding {
        return codec.verify(config, expected);
    }
    let actual = import_preference(Some(binding), config)?.into_value();
    if actual != expected {
        anyhow::bail!("applied {label} does not match the profile");
    }
    Ok(())
}

pub fn non_default_value(value: Value) -> Option<Value> {
    match value {
        Value::String(value) if value == "default" => None,
        other => Some(other),
    }
}

pub fn non_default_string(value: Value, label: &str) -> Result<Option<String>> {
    match value {
        Value::String(value) if value == "default" => Ok(None),
        Value::String(value) => Ok(Some(value)),
        other => anyhow::bail!("{label} must be a string or \"default\", got {other}"),
    }
}

fn set_json_pointer(config: &mut Value, pointer: &str, value: Value) -> Result<()> {
    let parts = pointer
        .strip_prefix('/')
        .ok_or_else(|| anyhow::anyhow!("JSON pointer must start with /"))?
        .split('/')
        .map(unescape_json_pointer)
        .collect::<Vec<_>>();
    if parts.is_empty() {
        *config = value;
        return Ok(());
    }

    let mut current = config;
    for key in &parts[..parts.len() - 1] {
        if !current.is_object() {
            *current = Value::Object(Default::default());
        }
        let object = current
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("failed to create JSON object for {pointer}"))?;
        current = object
            .entry(key.clone())
            .or_insert_with(|| Value::Object(Default::default()));
    }
    if !current.is_object() {
        *current = Value::Object(Default::default());
    }
    let object = current
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("failed to create JSON object for {pointer}"))?;
    object.insert(parts[parts.len() - 1].clone(), value);
    Ok(())
}

fn unescape_json_pointer(part: &str) -> String {
    part.replace("~1", "/").replace("~0", "~")
}

pub fn merge_profile_import(base: &mut ProfileImport, fragment: ProfileImport) {
    if fragment.instruction.is_some() {
        base.instruction = fragment.instruction;
    }
    base.skills.extend(fragment.skills);
    base.commands.extend(fragment.commands);
    if fragment.agents.is_some() {
        base.agents = fragment.agents;
    }
    if fragment.mcp_definitions.is_some() {
        base.mcp_definitions = fragment.mcp_definitions;
    }
    if fragment.model_preference != ImportedPreference::default_value() {
        base.model_preference = fragment.model_preference;
    }
    if fragment.permission_preference != ImportedPreference::default_value() {
        base.permission_preference = fragment.permission_preference;
    }
}

pub struct InstructionFile {
    path: PathSelector,
}

impl InstructionFile {
    pub fn new(path: PathSelector) -> Self {
        Self { path }
    }
}

impl HarnessArtifact for InstructionFile {
    fn kind(&self) -> ArtifactKind {
        ArtifactKind::Instructions
    }

    fn surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface> {
        vec![ManagedSurface::file((self.path)(paths))]
    }

    fn detect_drift(
        &self,
        ctx: &ArtifactContext<'_>,
        profile: &ProfileRef,
    ) -> Result<Vec<DriftItem>> {
        let mut items = Vec::new();
        collect_instruction_content_drift(&profile.path, (self.path)(ctx.paths), &mut items)?;
        Ok(items)
    }

    fn import(&self, ctx: &ArtifactContext<'_>) -> Result<ProfileImport> {
        Ok(ProfileImport {
            instruction: read_optional_string((self.path)(ctx.paths))?,
            ..ProfileImport::default()
        })
    }

    fn apply(&self, ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
        write_profile_instructions(&profile.path, (self.path)(ctx.paths))
    }

    fn verify(&self, ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
        verify_profile_instructions(ctx.display_name, &profile.path, (self.path)(ctx.paths))
    }
}

pub struct SkillsDirectory {
    path: PathSelector,
}

impl SkillsDirectory {
    pub fn new(path: PathSelector) -> Self {
        Self { path }
    }
}

impl HarnessArtifact for SkillsDirectory {
    fn kind(&self) -> ArtifactKind {
        ArtifactKind::Skills
    }

    fn surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface> {
        vec![ManagedSurface::directory((self.path)(paths))]
    }

    fn detect_drift(
        &self,
        ctx: &ArtifactContext<'_>,
        profile: &ProfileRef,
    ) -> Result<Vec<DriftItem>> {
        let mut items = Vec::new();
        collect_skills_drift(&profile.path, (self.path)(ctx.paths), &mut items)?;
        Ok(items)
    }

    fn import(&self, ctx: &ArtifactContext<'_>) -> Result<ProfileImport> {
        Ok(ProfileImport {
            skills: import_skills((self.path)(ctx.paths))?,
            ..ProfileImport::default()
        })
    }

    fn apply(&self, ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
        fs::create_dir_all((self.path)(ctx.paths))
            .with_context(|| format!("failed to create {}", (self.path)(ctx.paths).display()))?;
        copy_skills(profile, ctx.paths)
    }

    fn verify(&self, ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
        let mut items = Vec::new();
        collect_skills_drift(&profile.path, (self.path)(ctx.paths), &mut items)?;
        if let Some(item) = items.first() {
            anyhow::bail!(
                "{} skills do not match the profile: {}",
                ctx.display_name,
                item.detail
            );
        }
        Ok(())
    }
}

pub enum CommandMode {
    FlatCopy,
    RecursiveCopy,
    Rendered(Box<dyn CommandCodec>),
}

pub trait CommandCodec {
    fn import(&self, path: &Path) -> Result<Vec<crate::harness::integration::ImportedFile>>;
    fn apply(&self, profile: &ProfileRef, target_dir: &Path) -> Result<()>;
    fn detect_drift(&self, profile: &ProfileRef, target_dir: &Path) -> Result<Vec<DriftItem>>;
    fn verify(&self, profile: &ProfileRef, target_dir: &Path, display_name: &str) -> Result<()>;
}

pub struct CommandsDirectory {
    path: PathSelector,
    mode: CommandMode,
}

impl CommandsDirectory {
    pub fn new(path: PathSelector, mode: CommandMode) -> Self {
        Self { path, mode }
    }
}

impl HarnessArtifact for CommandsDirectory {
    fn kind(&self) -> ArtifactKind {
        ArtifactKind::Commands
    }

    fn surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface> {
        vec![ManagedSurface::directory((self.path)(paths))]
    }

    fn preflight(&self, _ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
        if matches!(self.mode, CommandMode::FlatCopy) {
            flat_profile_commands(&profile.path)?;
        }
        Ok(())
    }

    fn detect_drift(
        &self,
        ctx: &ArtifactContext<'_>,
        profile: &ProfileRef,
    ) -> Result<Vec<DriftItem>> {
        match &self.mode {
            CommandMode::FlatCopy => {
                let mut items = Vec::new();
                collect_flat_commands_drift(&profile.path, (self.path)(ctx.paths), &mut items)?;
                Ok(items)
            }
            CommandMode::RecursiveCopy => {
                let mut items = Vec::new();
                collect_commands_drift_recursive(
                    profile_commands_recursive(&profile.path)?,
                    (self.path)(ctx.paths),
                    &profile.path.join("commands"),
                    &mut items,
                )?;
                Ok(items)
            }
            CommandMode::Rendered(codec) => codec.detect_drift(profile, (self.path)(ctx.paths)),
        }
    }

    fn import(&self, ctx: &ArtifactContext<'_>) -> Result<ProfileImport> {
        let commands = match &self.mode {
            CommandMode::FlatCopy => import_flat_commands((self.path)(ctx.paths))?,
            CommandMode::RecursiveCopy => import_commands((self.path)(ctx.paths))?,
            CommandMode::Rendered(codec) => codec.import((self.path)(ctx.paths))?,
        };
        Ok(ProfileImport {
            commands,
            ..ProfileImport::default()
        })
    }

    fn apply(&self, ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
        fs::create_dir_all((self.path)(ctx.paths))
            .with_context(|| format!("failed to create {}", (self.path)(ctx.paths).display()))?;
        match &self.mode {
            CommandMode::FlatCopy => copy_flat_commands(profile, ctx.paths),
            CommandMode::RecursiveCopy => copy_commands(profile, ctx.paths),
            CommandMode::Rendered(codec) => codec.apply(profile, (self.path)(ctx.paths)),
        }
    }

    fn verify(&self, ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
        match &self.mode {
            CommandMode::FlatCopy => {
                let mut items = Vec::new();
                collect_flat_commands_drift(&profile.path, (self.path)(ctx.paths), &mut items)?;
                verify_no_command_drift(ctx.display_name, &items)
            }
            CommandMode::RecursiveCopy => {
                let mut items = Vec::new();
                collect_commands_drift_recursive(
                    profile_commands_recursive(&profile.path)?,
                    (self.path)(ctx.paths),
                    &profile.path.join("commands"),
                    &mut items,
                )?;
                verify_no_command_drift(ctx.display_name, &items)
            }
            CommandMode::Rendered(codec) => {
                codec.verify(profile, (self.path)(ctx.paths), ctx.display_name)
            }
        }
    }
}

fn verify_no_command_drift(display_name: &str, items: &[DriftItem]) -> Result<()> {
    if let Some(item) = items.first() {
        anyhow::bail!(
            "{display_name} commands do not match the profile: {}",
            item.detail
        );
    }
    Ok(())
}

pub trait SubagentCodec {
    fn native_file_name(&self, agent: &SubAgent) -> String;
    fn render(&self, agent: &SubAgent) -> Result<String>;
    fn should_import(&self, _path: &Path) -> bool {
        true
    }
    fn parse(&self, path: &Path, contents: &str) -> Result<SubAgent>;
}

pub struct SubagentsDirectory {
    path: PathSelector,
    codec: Box<dyn SubagentCodec>,
}

impl SubagentsDirectory {
    pub fn new(path: PathSelector, codec: impl SubagentCodec + 'static) -> Self {
        Self {
            path,
            codec: Box::new(codec),
        }
    }

    fn rendered(&self, profile: &ProfileRef) -> Result<Vec<RenderedAgent>> {
        profile_agents(&profile.path)?
            .into_iter()
            .map(|agent| {
                let agent_name = agent.name.clone();
                Ok(RenderedAgent {
                    relative_path: PathBuf::from(self.codec.native_file_name(&agent)),
                    contents: self.codec.render(&agent).with_context(|| {
                        format!(
                            "profile {} agent {agent_name} cannot be rendered",
                            profile.name
                        )
                    })?,
                })
            })
            .collect()
    }
}

impl HarnessArtifact for SubagentsDirectory {
    fn kind(&self) -> ArtifactKind {
        ArtifactKind::Subagents
    }

    fn surfaces(&self, paths: &HarnessConfigPaths) -> Vec<ManagedSurface> {
        vec![ManagedSurface::directory((self.path)(paths))]
    }

    fn preflight(&self, _ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
        self.rendered(profile).map(|_| ())
    }

    fn detect_drift(
        &self,
        ctx: &ArtifactContext<'_>,
        profile: &ProfileRef,
    ) -> Result<Vec<DriftItem>> {
        let mut items = Vec::new();
        collect_rendered_agent_drift(&self.rendered(profile)?, (self.path)(ctx.paths), &mut items)?;
        Ok(items)
    }

    fn import(&self, ctx: &ArtifactContext<'_>) -> Result<ProfileImport> {
        let path = (self.path)(ctx.paths);
        if !path.exists() {
            return Ok(ProfileImport {
                agents: Some(Vec::new()),
                ..ProfileImport::default()
            });
        }
        let mut imported = Vec::new();
        for file in crate::harness::fs::import_files_recursive_filtered(path, path, &|relative| {
            self.codec.should_import(relative)
        })? {
            let text = String::from_utf8(file.contents)
                .with_context(|| format!("agent {} is not UTF-8", file.relative_path.display()))?;
            let agent = self.codec.parse(&file.relative_path, &text)?;
            imported.push(sub_agent_import_file(&agent));
        }
        imported.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        Ok(ProfileImport {
            agents: Some(imported),
            ..ProfileImport::default()
        })
    }

    fn apply(&self, ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
        apply_rendered_agents(&self.rendered(profile)?, (self.path)(ctx.paths))
    }

    fn verify(&self, ctx: &ArtifactContext<'_>, profile: &ProfileRef) -> Result<()> {
        verify_rendered_agents(&self.rendered(profile)?, (self.path)(ctx.paths))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutable_json_config_rejects_non_object_roots() {
        let mut config = NativeConfig::Json(serde_json::json!(["valid", "but", "wrong"]));

        let error = config.json_object_mut("settings JSON").unwrap_err();

        assert!(error.to_string().contains("must be an object"));
    }

    #[test]
    fn typed_preferences_reject_present_wrong_native_types() {
        let json = NativeConfig::Json(serde_json::json!({"model": {"name": "wrong"}}));
        let error = import_preference(
            Some(&PreferenceBinding::JsonStringPointer { pointer: "/model" }),
            &json,
        )
        .unwrap_err();
        assert!(error.to_string().contains("must be a string"));

        let toml = NativeConfig::Toml("model = true".parse().unwrap());
        let error = import_preference(Some(&PreferenceBinding::TomlKey { key: "model" }), &toml)
            .unwrap_err();
        assert!(error.to_string().contains("must be a string"));
    }
}
