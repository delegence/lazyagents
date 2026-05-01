use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::harness::integration::{ImportedDirectory, ImportedFile, ProfileImport};

use crate::profile::config::{ProfileConfig, ProfileConfigStatus};
use crate::profile::inspect::{
    artifact_status, scan_commands, scan_skills, summarize_mcps, ProfileSummary,
};
use crate::profile::ProfileName;

#[derive(Debug, Clone)]
pub struct LazyagentsHome {
    path: PathBuf,
}

impl LazyagentsHome {
    pub fn resolve() -> Result<Self> {
        match env::var_os("LAZYAGENTS_HOME") {
            Some(value) if !value.is_empty() => Ok(Self {
                path: PathBuf::from(value),
            }),
            _ => Ok(Self {
                path: default_home()?,
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) fn from_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

fn default_home() -> Result<PathBuf> {
    let home =
        env::var_os("HOME").context("HOME is not set and LAZYAGENTS_HOME was not provided")?;
    Ok(PathBuf::from(home).join(".lazyagents"))
}

#[derive(Debug, Clone)]
pub struct ProfileStore {
    home: LazyagentsHome,
}

impl ProfileStore {
    pub fn new(home: LazyagentsHome) -> Self {
        Self { home }
    }

    pub fn profile_dir(&self, name: &ProfileName) -> PathBuf {
        self.home.path().join("profiles").join(name.as_str())
    }

    pub fn profile_dir_for_raw_name(&self, name: &str) -> Result<PathBuf> {
        ensure_single_path_component(name)?;
        Ok(self.home.path().join("profiles").join(name))
    }

    pub fn create_skeleton(&self, name: &ProfileName) -> Result<PathBuf> {
        let profiles_dir = self.home.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).with_context(|| {
            format!(
                "failed to create profiles directory at {}",
                profiles_dir.display()
            )
        })?;

        let target_dir = self.profile_dir(name);
        if target_dir.exists() {
            anyhow::bail!("profile {name} already exists at {}", target_dir.display());
        }

        let temp_dir = tempfile::Builder::new()
            .prefix(&format!(".{}-", name.as_str()))
            .tempdir_in(&profiles_dir)
            .with_context(|| {
                format!(
                    "failed to create temporary profile directory under {}",
                    profiles_dir.display()
                )
            })?;

        write_default_skeleton(temp_dir.path(), name)?;
        std::fs::rename(temp_dir.path(), &target_dir).with_context(|| {
            format!(
                "failed to move new profile into place at {}",
                target_dir.display()
            )
        })?;

        Ok(target_dir)
    }

    pub fn load_config(&self, name: &ProfileName) -> Result<ProfileConfig> {
        let path = self.profile_dir(name).join("config.json");
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!("missing or unreadable profile config at {}", path.display())
        })?;
        serde_json::from_str(&text)
            .with_context(|| format!("invalid profile config at {}", path.display()))
    }

    pub fn normalize_optional_artifacts(&self, name: &ProfileName) -> Result<()> {
        let profile_dir = self.profile_dir(name);
        if !profile_dir.is_dir() {
            anyhow::bail!("profile {name} does not exist at {}", profile_dir.display());
        }
        std::fs::create_dir_all(profile_dir.join("skills")).with_context(|| {
            format!("failed to create {}", profile_dir.join("skills").display())
        })?;
        std::fs::create_dir_all(profile_dir.join("commands")).with_context(|| {
            format!(
                "failed to create {}",
                profile_dir.join("commands").display()
            )
        })?;
        create_file_if_missing(&profile_dir.join("AGENTS.md"), "")?;
        create_file_if_missing(&profile_dir.join("mcps.json"), "")?;
        Ok(())
    }

    pub fn apply_import(
        &self,
        name: &ProfileName,
        harness_kind: crate::harness::kind::HarnessKind,
        imported: ProfileImport,
    ) -> Result<()> {
        self.normalize_optional_artifacts(name)?;
        let profile_dir = self.profile_dir(name);
        let profiles_dir = profile_dir.parent().ok_or_else(|| {
            anyhow::anyhow!("profile path has no parent: {}", profile_dir.display())
        })?;

        let staged_dir = tempfile::Builder::new()
            .prefix(&format!(".{}-import-", name.as_str()))
            .tempdir_in(profiles_dir)
            .with_context(|| {
                format!(
                    "failed to create temporary profile import directory under {}",
                    profiles_dir.display()
                )
            })?;
        copy_dir_contents(&profile_dir, staged_dir.path())?;
        apply_import_to_dir(staged_dir.path(), harness_kind, imported)?;

        let backup_dir = profiles_dir.join(format!(
            ".{}-rollback-{}",
            name.as_str(),
            std::process::id()
        ));
        if backup_dir.exists() {
            std::fs::remove_dir_all(&backup_dir)
                .with_context(|| format!("failed to remove {}", backup_dir.display()))?;
        }

        std::fs::rename(&profile_dir, &backup_dir).with_context(|| {
            format!(
                "failed to move {} to {}",
                profile_dir.display(),
                backup_dir.display()
            )
        })?;

        match std::fs::rename(staged_dir.path(), &profile_dir) {
            Ok(()) => {
                std::fs::remove_dir_all(&backup_dir)
                    .with_context(|| format!("failed to remove {}", backup_dir.display()))?;
                Ok(())
            }
            Err(error) => {
                let _ = std::fs::rename(&backup_dir, &profile_dir);
                Err(error).with_context(|| {
                    format!(
                        "failed to move staged profile import into place at {}",
                        profile_dir.display()
                    )
                })
            }
        }
    }

    pub fn list_profiles(&self) -> Result<Vec<ProfileListItem>> {
        let profiles_dir = self.home.path().join("profiles");
        if !profiles_dir.exists() {
            return Ok(Vec::new());
        }

        let mut items = Vec::new();
        for entry in std::fs::read_dir(&profiles_dir)
            .with_context(|| format!("failed to read {}", profiles_dir.display()))?
        {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if !file_type.is_dir() {
                continue;
            }

            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(profile_name) = ProfileName::parse(name) else {
                continue;
            };

            let config_path = entry.path().join("config.json");
            let config_status = match std::fs::read_to_string(&config_path) {
                Ok(text) => match serde_json::from_str::<ProfileConfig>(&text) {
                    Ok(_) => ProfileConfigStatus::Valid,
                    Err(error) => ProfileConfigStatus::Invalid(error.to_string()),
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    ProfileConfigStatus::Missing
                }
                Err(error) => ProfileConfigStatus::Invalid(error.to_string()),
            };

            items.push(ProfileListItem {
                name: profile_name.clone(),
                config_status,
            });
        }

        items.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(items)
    }

    pub fn summarize(&self, name: &ProfileName) -> Result<ProfileSummary> {
        let path = self.profile_dir(name);
        if !path.exists() {
            anyhow::bail!("profile {name} does not exist at {}", path.display());
        }

        let config = self
            .load_config(name)
            .unwrap_or_else(|_| crate::profile::ProfileConfig::default_for(name));
        let instruction_source = artifact_status(path.join("AGENTS.md"));
        let (valid_skills, ignored_skills) = scan_skills(&path.join("skills"))?;
        let (commands, ignored_command_files) = scan_commands(&path.join("commands"))?;
        let mcp_summary = summarize_mcps(&path.join("mcps.json"))?;

        let validation_issues = crate::profile::validation::validate_profile(&path);

        Ok(ProfileSummary {
            name: name.clone(),
            path,
            display_name: config.name,
            description: config.description,
            instruction_source,
            valid_skills,
            ignored_skills,
            commands,
            ignored_command_files,
            mcp_summary,
            models: config.models,
            permissions: config.permissions,
            validation_issues,
        })
    }

    pub fn get_path(&self, name: &str) -> Result<PathBuf> {
        let path = self.profile_dir_for_raw_name(name)?;
        if !path.is_dir() {
            anyhow::bail!("profile {name} does not exist at {}", path.display());
        }
        Ok(path)
    }
}

fn apply_import_to_dir(
    profile_dir: &Path,
    harness_kind: crate::harness::kind::HarnessKind,
    imported: ProfileImport,
) -> Result<()> {
    if let Some(instruction) = imported.instruction {
        std::fs::write(profile_dir.join("AGENTS.md"), instruction).with_context(|| {
            format!(
                "failed to write {}",
                profile_dir.join("AGENTS.md").display()
            )
        })?;
    }

    replace_imported_directories(&profile_dir.join("skills"), imported.skills)?;
    replace_imported_files(&profile_dir.join("commands"), imported.commands)?;

    if let Some(mcps) = imported.mcp_definitions {
        std::fs::write(profile_dir.join("mcps.json"), mcps).with_context(|| {
            format!(
                "failed to write {}",
                profile_dir.join("mcps.json").display()
            )
        })?;
    }

    let mut config = load_config_from_dir(profile_dir)?;
    config.models.insert(
        harness_kind.id().to_string(),
        imported.model_preference.into_value(),
    );
    config.permissions.insert(
        harness_kind.id().to_string(),
        imported.permission_preference.into_value(),
    );
    write_config_to_dir(profile_dir, &config)
}

fn load_config_from_dir(profile_dir: &Path) -> Result<ProfileConfig> {
    let path = profile_dir.join("config.json");
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("missing or unreadable profile config at {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("invalid profile config at {}", path.display()))
}

fn write_config_to_dir(profile_dir: &Path, config: &ProfileConfig) -> Result<()> {
    let path = profile_dir.join("config.json");
    let text = serde_json::to_string_pretty(config)?;
    std::fs::write(&path, format!("{text}\n"))
        .with_context(|| format!("failed to write profile config at {}", path.display()))
}

fn replace_imported_directories(root: &Path, directories: Vec<ImportedDirectory>) -> Result<()> {
    if root.exists() {
        std::fs::remove_dir_all(root)
            .with_context(|| format!("failed to clear {}", root.display()))?;
    }
    std::fs::create_dir_all(root)
        .with_context(|| format!("failed to create {}", root.display()))?;
    for directory in directories {
        ensure_single_path_component(&directory.name)?;
        let dir = root.join(directory.name);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
        write_imported_files(&dir, directory.files)?;
    }
    Ok(())
}

fn copy_dir_contents(source: &Path, target: &Path) -> Result<()> {
    std::fs::create_dir_all(target)
        .with_context(|| format!("failed to create {}", target.display()))?;
    for entry in
        std::fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let metadata = std::fs::metadata(&source_path)
            .with_context(|| format!("failed to inspect {}", source_path.display()))?;
        if metadata.is_dir() {
            copy_dir_contents(&source_path, &target_path)?;
        } else if metadata.is_file() {
            std::fs::copy(&source_path, &target_path)
                .with_context(|| format!("failed to copy {}", source_path.display()))?;
        }
    }
    Ok(())
}

fn replace_imported_files(root: &Path, files: Vec<ImportedFile>) -> Result<()> {
    if root.exists() {
        std::fs::remove_dir_all(root)
            .with_context(|| format!("failed to clear {}", root.display()))?;
    }
    std::fs::create_dir_all(root)
        .with_context(|| format!("failed to create {}", root.display()))?;
    write_imported_files(root, files)
}

fn write_imported_files(root: &Path, files: Vec<ImportedFile>) -> Result<()> {
    for file in files {
        ensure_relative_path(&file.relative_path)?;
        let path = root.join(file.relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&path, file.contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(())
}

fn ensure_relative_path(path: &Path) -> Result<()> {
    if path.is_absolute() {
        anyhow::bail!("import path must be relative: {}", path.display());
    }
    if path
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        anyhow::bail!(
            "import path contains unsupported components: {}",
            path.display()
        );
    }
    Ok(())
}

fn create_file_if_missing(path: &Path, contents: &str) -> Result<()> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            file.write_all(contents.as_bytes())?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to create {}", path.display())),
    }
}

fn ensure_single_path_component(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("profile name cannot be empty");
    }

    let path = Path::new(name);
    let mut components = path.components();
    let Some(component) = components.next() else {
        anyhow::bail!("profile name cannot be empty");
    };
    if components.next().is_some() || !matches!(component, std::path::Component::Normal(_)) {
        anyhow::bail!("profile name must be a single directory name");
    }
    Ok(())
}

fn write_default_skeleton(path: &Path, name: &ProfileName) -> Result<()> {
    std::fs::create_dir(path.join("skills"))
        .with_context(|| format!("failed to create {}", path.join("skills").display()))?;
    std::fs::create_dir(path.join("commands"))
        .with_context(|| format!("failed to create {}", path.join("commands").display()))?;

    std::fs::write(
        path.join("AGENTS.md"),
        format!("# {}\n\nAdd profile instructions here.\n", name),
    )
    .with_context(|| format!("failed to write {}", path.join("AGENTS.md").display()))?;

    let config = ProfileConfig::default_for(name);
    let config_text = serde_json::to_string_pretty(&config)?;
    std::fs::write(path.join("config.json"), format!("{config_text}\n"))
        .with_context(|| format!("failed to write {}", path.join("config.json").display()))?;

    std::fs::write(path.join("mcps.json"), "")
        .with_context(|| format!("failed to write {}", path.join("mcps.json").display()))?;

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileListItem {
    pub name: ProfileName,
    pub config_status: ProfileConfigStatus,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::harness_registry::BuiltInHarnessRegistry;
    use crate::harness::integration::AppEnvironment;
    use crate::profile::inspect::ArtifactStatus;
    use crate::profile::mcp::parse_mcp_definitions;
    use crate::profile::mcp::McpSummary;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn lazyagents_home_uses_environment_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        env::set_var("LAZYAGENTS_HOME", temp.path());

        let home = LazyagentsHome::resolve().unwrap();

        assert_eq!(home.path(), temp.path());
        env::remove_var("LAZYAGENTS_HOME");
    }

    #[test]
    fn lazyagents_home_defaults_to_home_subdirectory() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        env::remove_var("LAZYAGENTS_HOME");
        env::set_var("HOME", temp.path());

        let home = LazyagentsHome::resolve().unwrap();

        assert_eq!(home.path(), &temp.path().join(".lazyagents"));
    }

    #[test]
    fn profile_store_loads_required_config() {
        let temp = tempfile::tempdir().unwrap();
        let profile_name = ProfileName::parse("work").unwrap();
        let profile_dir = temp.path().join("profiles").join(profile_name.as_str());
        std::fs::create_dir_all(&profile_dir).unwrap();
        std::fs::write(
            profile_dir.join("config.json"),
            r#"{"name":"Work","models":{"codex":"default"},"permissions":{"codex":"default"}}"#,
        )
        .unwrap();

        let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));
        let config = store.load_config(&profile_name).unwrap();

        assert_eq!(config.name.as_deref(), Some("Work"));
        assert_eq!(config.models["codex"], "default");
        assert_eq!(config.permissions["codex"], "default");
    }

    #[test]
    fn profile_store_fails_when_config_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let profile_name = ProfileName::parse("work").unwrap();
        let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));

        assert!(store.load_config(&profile_name).is_err());
    }

    #[test]
    fn create_skeleton_writes_default_profile_contract() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));
        let profile_name = ProfileName::parse("work").unwrap();

        let profile_dir = store.create_skeleton(&profile_name).unwrap();

        assert_eq!(profile_dir, temp.path().join("profiles").join("work"));
        assert!(profile_dir.join("AGENTS.md").is_file());
        assert!(profile_dir.join("skills").is_dir());
        assert!(profile_dir.join("commands").is_dir());

        let config = store.load_config(&profile_name).unwrap();
        assert_eq!(config.name.as_deref(), Some("Work"));
        assert_eq!(config.description.as_deref(), Some(""));
        for harness in [
            crate::harness::kind::HarnessKind::Codex,
            crate::harness::kind::HarnessKind::Claude,
            crate::harness::kind::HarnessKind::OpenCode,
        ] {
            assert_eq!(config.model_preference(harness), "default");
            assert_eq!(config.permission_preference(harness), "default");
        }

        let mcps = std::fs::read_to_string(profile_dir.join("mcps.json")).unwrap();
        assert!(parse_mcp_definitions(&mcps).unwrap().is_empty());
    }

    #[test]
    fn create_skeleton_fails_without_overwriting_existing_profile() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));
        let profile_name = ProfileName::parse("work").unwrap();
        let profile_dir = store.create_skeleton(&profile_name).unwrap();
        std::fs::write(profile_dir.join("AGENTS.md"), "existing").unwrap();

        let error = store.create_skeleton(&profile_name).unwrap_err();

        assert!(error.to_string().contains("already exists"));
        assert_eq!(
            std::fs::read_to_string(profile_dir.join("AGENTS.md")).unwrap(),
            "existing"
        );
    }

    #[test]
    fn list_profiles_is_sorted_marks_config_status_and_ignores_invalid_names() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));
        store
            .create_skeleton(&ProfileName::parse("beta").unwrap())
            .unwrap();
        store
            .create_skeleton(&ProfileName::parse("alpha").unwrap())
            .unwrap();

        let profiles_dir = temp.path().join("profiles");
        std::fs::create_dir_all(profiles_dir.join("bad_name")).unwrap();
        std::fs::create_dir_all(profiles_dir.join("missing")).unwrap();
        std::fs::create_dir_all(profiles_dir.join("invalid")).unwrap();
        std::fs::write(profiles_dir.join("invalid").join("config.json"), "{").unwrap();
        let items = store.list_profiles().unwrap();

        assert_eq!(
            items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta", "invalid", "missing"]
        );
        assert_eq!(items[0].config_status, ProfileConfigStatus::Valid);
        assert!(matches!(
            items[2].config_status,
            ProfileConfigStatus::Invalid(_)
        ));
        assert_eq!(items[3].config_status, ProfileConfigStatus::Missing);
    }

    #[test]
    fn summarize_reports_artifacts_and_does_not_mutate_profile() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));
        let profile_name = ProfileName::parse("work").unwrap();
        let profile_dir = store.create_skeleton(&profile_name).unwrap();
        std::fs::create_dir(profile_dir.join("skills").join("valid")).unwrap();
        std::fs::write(
            profile_dir.join("skills").join("valid").join("SKILL.md"),
            "",
        )
        .unwrap();
        std::fs::create_dir(profile_dir.join("skills").join("ignored")).unwrap();
        std::fs::write(profile_dir.join("skills").join("notes.txt"), "").unwrap();
        std::fs::write(profile_dir.join("skills").join(".DS_Store"), "").unwrap();
        std::fs::create_dir(profile_dir.join("commands").join("nested")).unwrap();
        std::fs::write(profile_dir.join("commands").join("run.md"), "").unwrap();
        std::fs::write(
            profile_dir.join("commands").join("nested").join("build.md"),
            "",
        )
        .unwrap();
        std::fs::write(profile_dir.join("commands").join("draft.txt"), "").unwrap();
        std::fs::write(profile_dir.join("commands").join(".DS_Store"), "").unwrap();
        std::fs::write(
            profile_dir.join("mcps.json"),
            r#"[{"name":"enabled","transport":"stdio","command":"x"},{"name":"draft","enabled":false,"transport":"stdio","command":"draft-server"}]"#,
        )
        .unwrap();

        let before = snapshot_files(&profile_dir);
        let summary = store.summarize(&profile_name).unwrap();
        let after = snapshot_files(&profile_dir);

        assert_eq!(before, after);
        assert_eq!(summary.instruction_source, ArtifactStatus::Present);
        assert_eq!(summary.valid_skills, vec!["valid"]);
        assert_eq!(summary.ignored_skills, vec!["ignored", "notes.txt"]);
        assert_eq!(summary.commands, vec!["nested/build.md", "run.md"]);
        assert_eq!(summary.ignored_command_files, vec!["draft.txt"]);
        assert_eq!(
            summary.mcp_summary,
            McpSummary::Servers(vec!["draft (disabled)".to_string(), "enabled".to_string()])
        );
    }

    #[test]
    fn summarize_reports_invalid_mcp_file_without_failing_show_summary() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));
        let profile_name = ProfileName::parse("work").unwrap();
        let profile_dir = store.create_skeleton(&profile_name).unwrap();
        std::fs::write(profile_dir.join("mcps.json"), "not json").unwrap();

        let summary = store.summarize(&profile_name).unwrap();

        assert!(matches!(summary.mcp_summary, McpSummary::Invalid(_)));
    }

    #[test]
    fn get_path_accepts_invalid_profile_names_without_normalizing() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        env::remove_var("EDITOR");
        let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));
        let profile_dir = temp.path().join("profiles").join("bad_name");
        std::fs::create_dir_all(&profile_dir).unwrap();

        let target = store.get_path("bad_name").unwrap();

        assert_eq!(target, profile_dir);
    }

    #[test]
    fn delete_profile_allows_invalid_inactive_profiles_and_preserves_backups() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));
        let profile_dir = temp.path().join("profiles").join("bad_name");
        let backup_file = temp.path().join("backups").join("codex").join("AGENTS.md");
        std::fs::create_dir_all(&profile_dir).unwrap();
        std::fs::create_dir_all(backup_file.parent().unwrap()).unwrap();
        std::fs::write(&backup_file, "backup").unwrap();
        let env = test_runtime_env(temp.path());

        let deleted = crate::app::delete_profile::delete_profile(
            &BuiltInHarnessRegistry,
            &env,
            &store,
            "bad_name",
        )
        .unwrap();

        assert_eq!(deleted, profile_dir);
        assert!(!deleted.exists());
        assert_eq!(std::fs::read_to_string(backup_file).unwrap(), "backup");
    }

    #[test]
    fn delete_profile_blocks_when_state_marks_profile_active() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));
        let profile = ProfileName::parse("work").unwrap();
        store.create_skeleton(&profile).unwrap();
        std::fs::write(
            temp.path().join("state.json"),
            r#"{"active_profiles":{"codex":"work"}}"#,
        )
        .unwrap();
        let env = test_runtime_env(temp.path());

        let error = crate::app::delete_profile::delete_profile(
            &BuiltInHarnessRegistry,
            &env,
            &store,
            "work",
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("state marks it active for codex"));
        assert!(store.profile_dir(&profile).exists());
    }

    #[test]
    fn delete_profile_blocks_when_harness_symlink_points_into_profile() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("lazyagents");
        let user_home = temp.path().join("user");
        let store = ProfileStore::new(LazyagentsHome::from_path(&home));
        let profile = ProfileName::parse("work").unwrap();
        let profile_dir = store.create_skeleton(&profile).unwrap();
        let codex_dir = user_home.join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        create_symlink(profile_dir.join("AGENTS.md"), codex_dir.join("AGENTS.md"));
        let env = AppEnvironment {
            lazyagents_home: home,
            user_home,
            path_entries: Vec::new(),
        };

        let error = crate::app::delete_profile::delete_profile(
            &BuiltInHarnessRegistry,
            &env,
            &store,
            "work",
        )
        .unwrap_err();

        assert!(error.to_string().contains("Codex config links to it"));
        assert!(profile_dir.exists());
    }

    #[test]
    fn raw_profile_names_reject_path_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));

        assert!(store.profile_dir_for_raw_name("../work").is_err());
        assert!(store.profile_dir_for_raw_name("nested/work").is_err());
    }

    fn snapshot_files(root: &Path) -> BTreeMap<String, String> {
        let mut files = BTreeMap::new();
        snapshot_files_in(root, root, &mut files);
        files
    }

    fn snapshot_files_in(root: &Path, path: &Path, files: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                snapshot_files_in(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                    std::fs::read_to_string(path).unwrap(),
                );
            }
        }
    }

    fn test_runtime_env(home: &Path) -> AppEnvironment {
        AppEnvironment {
            lazyagents_home: home.to_path_buf(),
            user_home: home.join("user"),
            path_entries: Vec::new(),
        }
    }

    #[cfg(unix)]
    fn create_symlink(source: impl AsRef<Path>, target: impl AsRef<Path>) {
        std::os::unix::fs::symlink(source, target).unwrap();
    }

    #[cfg(windows)]
    fn create_symlink(source: impl AsRef<Path>, target: impl AsRef<Path>) {
        std::os::windows::fs::symlink_file(source, target).unwrap();
    }
}
