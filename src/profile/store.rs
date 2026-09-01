use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::harness::integration::{ImportedDirectory, ImportedFile, ProfileImport};

use crate::profile::config::{
    default_profile_document, read_profile_config, read_profile_document, write_profile_document,
    ProfileConfig, ProfileConfigStatus, ProfileDocument, PROFILE_FILE_NAME,
};
use crate::profile::inspect::{
    artifact_status, scan_commands, scan_skills, summarize_mcps, ProfileSummary,
};
use crate::profile::ProfileName;

#[derive(serde::Serialize, serde::Deserialize)]
struct ProfileTransactionMarker {
    rollback: String,
    phase: ProfileTransactionPhase,
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ProfileTransactionPhase {
    Prepared,
    Committed,
}

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

    pub fn create_skeleton(&self, name: &ProfileName) -> Result<PathBuf> {
        let profiles_dir = self.home.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).with_context(|| {
            format!(
                "failed to create profiles directory at {}",
                profiles_dir.display()
            )
        })?;
        self.recover_profile_rollback(name)?;

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
        self.recover_profile_rollback(name)?;
        read_profile_config(&self.profile_dir(name))
    }

    pub fn normalize_optional_artifacts(&self, name: &ProfileName) -> Result<()> {
        self.recover_profile_rollback(name)?;
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
        std::fs::create_dir_all(profile_dir.join("agents")).with_context(|| {
            format!("failed to create {}", profile_dir.join("agents").display())
        })?;
        create_file_if_missing(&profile_dir.join("mcps.json"), "")?;
        Ok(())
    }

    pub fn apply_import(
        &self,
        name: &ProfileName,
        harness_id: &str,
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
        apply_import_to_dir(staged_dir.path(), harness_id, imported)?;
        sync_tree(staged_dir.path())?;

        let backup_holder = tempfile::Builder::new()
            .prefix(&format!(".{}-rollback-", name.as_str()))
            .tempdir_in(profiles_dir)?;
        let backup_dir = backup_holder.path().to_path_buf();
        backup_holder.close()?;

        let marker_path = profile_transaction_marker_path(profiles_dir, name);
        let rollback_name = backup_dir
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow::anyhow!("profile rollback path has an invalid name"))?
            .to_string();
        write_profile_transaction_marker(
            &marker_path,
            &ProfileTransactionMarker {
                rollback: rollback_name,
                phase: ProfileTransactionPhase::Prepared,
            },
        )?;

        if let Err(error) = publish_staged_profile(
            &profile_dir,
            staged_dir.path(),
            &backup_dir,
            profiles_dir,
            |source, target| std::fs::rename(source, target),
        ) {
            if profile_dir.exists() {
                let _ = std::fs::remove_file(&marker_path);
                let _ = sync_directory(profiles_dir);
            }
            return Err(error);
        }
        // The canonical profile now contains the staged data. Persist this fact
        // before cleanup so recovery can never treat the old rollback as current.
        let _ = write_profile_transaction_marker(
            &marker_path,
            &ProfileTransactionMarker {
                rollback: backup_dir
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                phase: ProfileTransactionPhase::Committed,
            },
        );
        if std::fs::remove_dir_all(&backup_dir).is_ok() {
            std::fs::remove_file(&marker_path)?;
            sync_directory(profiles_dir)?;
        }
        Ok(())
    }

    fn recover_profile_rollback(&self, name: &ProfileName) -> Result<()> {
        let profile = self.profile_dir(name);
        let profile_exists = profile.exists();
        let profiles = self.home.path().join("profiles");
        if !profiles.is_dir() {
            return Ok(());
        }
        let prefix = format!(".{}-rollback-", name.as_str());
        let mut candidates = Vec::new();
        for entry in std::fs::read_dir(&profiles)? {
            let entry = entry?;
            if !entry.file_name().to_string_lossy().starts_with(&prefix) {
                continue;
            }
            let kind = entry.file_type()?;
            if !kind.is_dir() || kind.is_symlink() {
                anyhow::bail!(
                    "rollback candidate {} for profile {name} is not a real directory",
                    entry.path().display()
                );
            }
            candidates.push(entry.path());
        }
        candidates.sort();
        let marker_path = profile_transaction_marker_path(&profiles, name);
        let marker = match std::fs::read_to_string(&marker_path) {
            Ok(text) => Some(
                serde_json::from_str::<ProfileTransactionMarker>(&text).with_context(|| {
                    format!(
                        "invalid profile transaction marker {}",
                        marker_path.display()
                    )
                })?,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if profile_exists {
            return match (candidates.as_slice(), marker) {
                ([], None) => Ok(()),
                ([], Some(_)) => {
                    std::fs::remove_file(marker_path)?;
                    sync_directory(&profiles)
                }
                ([rollback], Some(marker))
                    if rollback.file_name().and_then(|value| value.to_str())
                        == Some(marker.rollback.as_str()) =>
                {
                    // A canonical profile plus its rollback means staged data was
                    // published, even if the process stopped before phase update.
                    std::fs::remove_dir_all(rollback)?;
                    std::fs::remove_file(marker_path)?;
                    sync_directory(&profiles)
                }
                (_, None) => Ok(()),
                _ => anyhow::bail!(
                    "profile transaction data for {name} is inconsistent; manual recovery is required"
                ),
            };
        }
        match candidates.as_slice() {
            [] => {
                if marker.is_some() {
                    anyhow::bail!(
                        "profile transaction marker exists for {name}, but its rollback data is missing"
                    );
                }
                Ok(())
            }
            [rollback] => {
                let marker = marker.ok_or_else(|| {
                    anyhow::anyhow!(
                        "unmarked rollback data exists for profile {name}; manual recovery is required"
                    )
                })?;
                if rollback.file_name().and_then(|value| value.to_str())
                    != Some(marker.rollback.as_str())
                {
                    anyhow::bail!("profile transaction marker for {name} does not match rollback data");
                }
                if marker.phase == ProfileTransactionPhase::Committed {
                    std::fs::remove_dir_all(rollback)?;
                    std::fs::remove_file(&marker_path)?;
                    return sync_directory(&profiles);
                }
                read_profile_config(rollback).with_context(|| {
                    format!("rollback data for profile {name} is invalid")
                })?;
                std::fs::rename(rollback, &profile).with_context(|| {
                    format!("failed to recover interrupted profile {name}")
                })?;
                std::fs::remove_file(marker_path)?;
                sync_directory(&profiles)
            }
            _ => anyhow::bail!(
                "multiple rollback directories exist for profile {name}; manual recovery is required"
            ),
        }
    }

    pub fn list_profiles(&self) -> Result<Vec<ProfileListItem>> {
        let profiles_dir = self.home.path().join("profiles");
        if !profiles_dir.exists() {
            return Ok(Vec::new());
        }

        self.recover_all_profile_rollbacks()?;
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

            let config_status = match read_profile_config(&entry.path()) {
                Ok(_) => ProfileConfigStatus::Valid,
                Err(error) => {
                    let profile_path = entry.path().join(PROFILE_FILE_NAME);
                    if !profile_path.exists() {
                        ProfileConfigStatus::Missing
                    } else {
                        ProfileConfigStatus::Invalid(error.to_string())
                    }
                }
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
        self.recover_profile_rollback(name)?;
        let path = self.profile_dir(name);
        if !path.exists() {
            anyhow::bail!("profile {name} does not exist at {}", path.display());
        }

        let config = self
            .load_config(name)
            .unwrap_or_else(|_| crate::profile::ProfileConfig::default_for(name));
        let instruction_source = artifact_status(path.join(PROFILE_FILE_NAME));
        let (valid_skills, ignored_skills) = scan_skills(&path.join("skills"))?;
        let (commands, ignored_command_files) = scan_commands(&path.join("commands"))?;
        let (agents, ignored_agent_files) =
            crate::harness::agents::scan_agents(&path.join("agents"))?;
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
            agents,
            ignored_agent_files,
            mcp_summary,
            models: config.models,
            permissions: config.permissions,
            validation_issues,
        })
    }

    fn recover_all_profile_rollbacks(&self) -> Result<()> {
        let profiles = self.home.path().join("profiles");
        if !profiles.is_dir() {
            return Ok(());
        }
        let mut names = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir(&profiles)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(rest) = name.strip_prefix('.') else {
                continue;
            };
            let Some((profile, _)) = rest.split_once("-rollback-") else {
                continue;
            };
            if let Ok(profile) = ProfileName::parse(profile.to_string()) {
                names.insert(profile);
            }
        }
        for name in names {
            self.recover_profile_rollback(&name)?;
        }
        Ok(())
    }
}

fn profile_transaction_marker_path(profiles: &Path, name: &ProfileName) -> PathBuf {
    profiles.join(format!(".{}-transaction.json", name.as_str()))
}

fn write_profile_transaction_marker(path: &Path, marker: &ProfileTransactionMarker) -> Result<()> {
    crate::file_system::write_text_atomic(path, &format!("{}\n", serde_json::to_string(marker)?))
}

fn publish_staged_profile(
    profile: &Path,
    staged: &Path,
    rollback: &Path,
    profiles: &Path,
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    rename(profile, rollback).with_context(|| {
        format!(
            "failed to move {} to {}",
            profile.display(),
            rollback.display()
        )
    })?;
    sync_directory(profiles)?;
    if let Err(publish_error) = rename(staged, profile) {
        if let Err(restore_error) = rename(rollback, profile) {
            anyhow::bail!(
                "failed to publish staged profile at {}: {publish_error}; failed to restore rollback: {restore_error}",
                profile.display()
            );
        }
        sync_directory(profiles)?;
        return Err(publish_error).with_context(|| {
            format!(
                "failed to move staged profile import into place at {}",
                profile.display()
            )
        });
    }
    sync_directory(profiles)
}

fn apply_import_to_dir(
    profile_dir: &Path,
    harness_id: &str,
    imported: ProfileImport,
) -> Result<()> {
    let mut document = load_document_from_dir(profile_dir)?;
    if let Some(instruction) = imported.instruction {
        document.instructions = instruction;
    }

    replace_imported_directories(&profile_dir.join("skills"), imported.skills)?;
    replace_imported_files(&profile_dir.join("commands"), imported.commands)?;
    if let Some(agents) = imported.agents {
        replace_imported_files(&profile_dir.join("agents"), agents)?;
    }

    if let Some(mcps) = imported.mcp_definitions {
        std::fs::write(profile_dir.join("mcps.json"), mcps).with_context(|| {
            format!(
                "failed to write {}",
                profile_dir.join("mcps.json").display()
            )
        })?;
    }

    document.config.models.insert(
        harness_id.to_string(),
        imported.model_preference.into_value(),
    );
    document.config.permissions.insert(
        harness_id.to_string(),
        imported.permission_preference.into_value(),
    );
    write_document_to_dir(profile_dir, &document)
}

fn load_document_from_dir(profile_dir: &Path) -> Result<ProfileDocument> {
    read_profile_document(profile_dir)
}

fn write_document_to_dir(profile_dir: &Path, document: &ProfileDocument) -> Result<()> {
    write_profile_document(profile_dir, document)
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
        for entry in &directory.directories {
            ensure_relative_path(&entry.relative_path)?;
            std::fs::create_dir_all(dir.join(&entry.relative_path))?;
        }
        write_imported_files(&dir, directory.files)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut entries = directory.directories;
            entries
                .sort_by_key(|entry| std::cmp::Reverse(entry.relative_path.components().count()));
            for entry in entries {
                if let Some(mode) = entry.unix_mode {
                    std::fs::set_permissions(
                        dir.join(entry.relative_path),
                        std::fs::Permissions::from_mode(mode),
                    )?;
                }
            }
            if let Some(mode) = directory.unix_mode {
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode))?;
            }
        }
    }
    Ok(())
}

fn copy_dir_contents(source: &Path, target: &Path) -> Result<()> {
    let root = std::fs::canonicalize(source)
        .with_context(|| format!("failed to resolve {}", source.display()))?;
    copy_dir_contents_at(
        source,
        target,
        &root,
        &mut std::collections::BTreeSet::new(),
    )
}

fn copy_dir_contents_at(
    source: &Path,
    target: &Path,
    root: &Path,
    active: &mut std::collections::BTreeSet<PathBuf>,
) -> Result<()> {
    let identity = std::fs::canonicalize(source)
        .with_context(|| format!("failed to resolve {}", source.display()))?;
    if !identity.starts_with(root) {
        anyhow::bail!(
            "directory {} resolves outside its profile",
            source.display()
        );
    }
    if !active.insert(identity.clone()) {
        anyhow::bail!("directory cycle detected at {}", source.display());
    }
    std::fs::create_dir_all(target)
        .with_context(|| format!("failed to create {}", target.display()))?;
    for entry in
        std::fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let link_metadata = std::fs::symlink_metadata(&source_path)?;
        let metadata = std::fs::metadata(&source_path)
            .with_context(|| format!("failed to inspect {}", source_path.display()))?;
        if link_metadata.file_type().is_symlink() {
            let resolved = std::fs::canonicalize(&source_path)?;
            if !resolved.starts_with(root) {
                anyhow::bail!(
                    "symlink {} resolves outside its profile",
                    source_path.display()
                );
            }
        }
        if metadata.is_dir() {
            copy_dir_contents_at(&source_path, &target_path, root, active)?;
        } else if metadata.is_file() {
            std::fs::copy(&source_path, &target_path)
                .with_context(|| format!("failed to copy {}", source_path.display()))?;
        } else {
            anyhow::bail!("unsupported filesystem entry {}", source_path.display());
        }
    }
    active.remove(&identity);
    Ok(())
}

#[cfg(unix)]
fn sync_tree(path: &Path) -> Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            sync_tree(&entry.path())?;
        } else if metadata.is_file() {
            std::fs::File::open(entry.path())?.sync_all()?;
        }
    }
    sync_directory(path)
}

#[cfg(not(unix))]
fn sync_tree(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("failed to sync directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
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
    let mut paths = std::collections::BTreeSet::new();
    for file in files {
        ensure_relative_path(&file.relative_path)?;
        if !paths.insert(file.relative_path.clone()) {
            anyhow::bail!(
                "import contains duplicate path {}",
                file.relative_path.display()
            );
        }
        let path = root.join(file.relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&path, file.contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
        #[cfg(unix)]
        if let Some(mode) = file.unix_mode {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                .with_context(|| format!("failed to restore permissions for {}", path.display()))?;
        }
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
    std::fs::create_dir(path.join("agents"))
        .with_context(|| format!("failed to create {}", path.join("agents").display()))?;

    let document = default_profile_document(name);
    write_profile_document(path, &document)?;

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
            profile_dir.join(PROFILE_FILE_NAME),
            "---\nname: Work\nmodels:\n  codex: default\npermissions:\n  codex: default\n---\nInstructions\n",
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
    fn optional_artifact_normalization_does_not_create_required_profile_file() {
        let temp = tempfile::tempdir().unwrap();
        let profile_name = ProfileName::parse("work").unwrap();
        let profile_dir = temp.path().join("profiles/work");
        std::fs::create_dir_all(&profile_dir).unwrap();
        let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));

        store.normalize_optional_artifacts(&profile_name).unwrap();

        assert!(!profile_dir.join(PROFILE_FILE_NAME).exists());
        assert!(profile_dir.join("skills").is_dir());
        assert!(profile_dir.join("commands").is_dir());
        assert!(profile_dir.join("agents").is_dir());
        assert!(profile_dir.join("mcps.json").is_file());
    }

    #[test]
    fn imported_files_reject_duplicate_normalized_paths() {
        let temp = tempfile::tempdir().unwrap();
        let duplicate = ImportedFile {
            relative_path: PathBuf::from("reviewer.md"),
            contents: b"first".to_vec(),
            unix_mode: None,
        };

        let error =
            write_imported_files(temp.path(), vec![duplicate.clone(), duplicate]).unwrap_err();

        assert!(error.to_string().contains("duplicate path reviewer.md"));
    }

    #[test]
    fn load_recovers_a_lone_profile_rollback_directory() {
        let temp = tempfile::tempdir().unwrap();
        let home = LazyagentsHome::from_path(temp.path());
        let store = ProfileStore::new(home);
        let name = ProfileName::parse("work").unwrap();
        let profile = store.create_skeleton(&name).unwrap();
        let rollback = profile.parent().unwrap().join(".work-rollback-crash");
        std::fs::rename(&profile, &rollback).unwrap();
        write_profile_transaction_marker(
            &profile_transaction_marker_path(profile.parent().unwrap(), &name),
            &ProfileTransactionMarker {
                rollback: ".work-rollback-crash".to_string(),
                phase: ProfileTransactionPhase::Prepared,
            },
        )
        .unwrap();

        assert_eq!(
            store.load_config(&name).unwrap().name.as_deref(),
            Some("Work")
        );
        assert!(profile.is_dir());
        assert!(!rollback.exists());
    }

    #[test]
    fn create_skeleton_writes_default_profile_contract() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));
        let profile_name = ProfileName::parse("work").unwrap();

        let profile_dir = store.create_skeleton(&profile_name).unwrap();

        assert_eq!(profile_dir, temp.path().join("profiles").join("work"));
        assert!(profile_dir.join(PROFILE_FILE_NAME).is_file());
        assert!(profile_dir.join("skills").is_dir());
        assert!(profile_dir.join("commands").is_dir());

        let config = store.load_config(&profile_name).unwrap();
        assert_eq!(config.name.as_deref(), Some("Work"));
        assert_eq!(config.description.as_deref(), Some(""));
        assert!(crate::profile::read_profile_instructions(&profile_dir)
            .unwrap()
            .is_empty());
        for harness in ["codex", "claude", "opencode"] {
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
        std::fs::write(profile_dir.join(PROFILE_FILE_NAME), "existing").unwrap();

        let error = store.create_skeleton(&profile_name).unwrap_err();

        assert!(error.to_string().contains("already exists"));
        assert_eq!(
            std::fs::read_to_string(profile_dir.join(PROFILE_FILE_NAME)).unwrap(),
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
        std::fs::write(profiles_dir.join("invalid").join(PROFILE_FILE_NAME), "{").unwrap();
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
    fn edit_profile_rejects_invalid_legacy_profile_names() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        env::remove_var("EDITOR");
        let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));
        let profile_dir = temp.path().join("profiles").join("bad_name");
        std::fs::create_dir_all(&profile_dir).unwrap();

        let error = crate::app::edit_profile::edit_profile_path(&store, "bad_name").unwrap_err();

        assert!(error
            .to_string()
            .contains("profile name may contain only ASCII letters"));
        assert!(profile_dir.exists());
    }

    #[test]
    fn delete_profile_rejects_invalid_legacy_profile_names_and_preserves_backups() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));
        let profile_dir = temp.path().join("profiles").join("bad_name");
        let backup_file = temp.path().join("backups").join("codex").join("AGENTS.md");
        std::fs::create_dir_all(&profile_dir).unwrap();
        std::fs::create_dir_all(backup_file.parent().unwrap()).unwrap();
        std::fs::write(&backup_file, "backup").unwrap();
        let env = test_runtime_env(temp.path());

        let error = crate::app::delete_profile::delete_profile(
            &BuiltInHarnessRegistry,
            &env,
            &store,
            "bad_name",
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("profile name may contain only ASCII letters"));
        assert!(profile_dir.exists());
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
        let skill_dir = profile_dir.join("skills").join("writer");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "").unwrap();
        std::fs::create_dir_all(codex_dir.join("skills")).unwrap();
        create_symlink(&skill_dir, codex_dir.join("skills").join("writer"));
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

    #[test]
    fn profile_publication_failures_restore_or_preserve_recoverable_data() {
        for failure_mode in [1usize, 2, 3] {
            let temp = tempfile::tempdir().unwrap();
            let profile = temp.path().join("work");
            let staged = temp.path().join("staged");
            let rollback = temp.path().join(".work-rollback-test");
            std::fs::create_dir(&profile).unwrap();
            std::fs::write(profile.join("PROFILE.md"), "old").unwrap();
            std::fs::create_dir(&staged).unwrap();
            std::fs::write(staged.join("PROFILE.md"), "new").unwrap();
            let mut calls = 0usize;
            let result = publish_staged_profile(
                &profile,
                &staged,
                &rollback,
                temp.path(),
                |source, target| {
                    calls += 1;
                    if calls == failure_mode || (failure_mode == 3 && calls == 2) {
                        return Err(std::io::Error::other("injected rename failure"));
                    }
                    std::fs::rename(source, target)
                },
            );
            assert!(result.is_err());
            if failure_mode < 3 {
                assert_eq!(
                    std::fs::read_to_string(profile.join("PROFILE.md")).unwrap(),
                    "old"
                );
            } else {
                assert_eq!(
                    std::fs::read_to_string(rollback.join("PROFILE.md")).unwrap(),
                    "old"
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
#[cfg(unix)]
#[test]
fn imported_skill_directory_modes_are_preserved() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));
    let name = ProfileName::parse("work").unwrap();
    store.create_skeleton(&name).unwrap();
    let imported = ProfileImport {
        skills: vec![ImportedDirectory {
            name: "private".to_string(),
            unix_mode: Some(0o750),
            directories: vec![crate::harness::integration::ImportedDirectoryEntry {
                relative_path: PathBuf::from("parent"),
                unix_mode: Some(0o700),
            }],
            files: vec![ImportedFile {
                relative_path: PathBuf::from("parent/data.txt"),
                contents: b"data".to_vec(),
                unix_mode: Some(0o644),
            }],
        }],
        ..ProfileImport::default()
    };
    store.apply_import(&name, "codex", imported).unwrap();

    let skill = store.profile_dir(&name).join("skills/private");
    assert_eq!(
        std::fs::metadata(&skill).unwrap().permissions().mode() & 0o777,
        0o750
    );
    assert_eq!(
        std::fs::metadata(skill.join("parent"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(skill.join("parent/data.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
}

#[test]
fn committed_profile_rollback_is_cleaned_instead_of_restored() {
    let temp = tempfile::tempdir().unwrap();
    let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));
    let name = ProfileName::parse("work").unwrap();
    let profiles = temp.path().join("profiles");
    let rollback = profiles.join(".work-rollback-stale");
    std::fs::create_dir_all(&rollback).unwrap();
    write_default_skeleton(&rollback, &name).unwrap();
    write_profile_transaction_marker(
        &profile_transaction_marker_path(&profiles, &name),
        &ProfileTransactionMarker {
            rollback: ".work-rollback-stale".to_string(),
            phase: ProfileTransactionPhase::Committed,
        },
    )
    .unwrap();

    assert!(store.load_config(&name).is_err());
    assert!(!rollback.exists());
    assert!(!store.profile_dir(&name).exists());
}

#[test]
fn unmarked_profile_rollback_requires_manual_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));
    let name = ProfileName::parse("work").unwrap();
    let rollback = temp.path().join("profiles/.work-rollback-unmarked");
    std::fs::create_dir_all(&rollback).unwrap();
    write_default_skeleton(&rollback, &name).unwrap();

    let error = store.load_config(&name).unwrap_err().to_string();
    assert!(error.contains("unmarked rollback data"));
    assert!(rollback.is_dir());
    assert!(!store.profile_dir(&name).exists());
}

#[test]
fn canonical_profile_makes_a_prepared_rollback_committed_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let store = ProfileStore::new(LazyagentsHome::from_path(temp.path()));
    let name = ProfileName::parse("work").unwrap();
    let profile = store.create_skeleton(&name).unwrap();
    let rollback = temp.path().join("profiles/.work-rollback-old");
    std::fs::create_dir_all(&rollback).unwrap();
    write_default_skeleton(&rollback, &name).unwrap();
    write_profile_transaction_marker(
        &profile_transaction_marker_path(profile.parent().unwrap(), &name),
        &ProfileTransactionMarker {
            rollback: ".work-rollback-old".to_string(),
            phase: ProfileTransactionPhase::Prepared,
        },
    )
    .unwrap();

    store.load_config(&name).unwrap();

    assert!(profile.is_dir());
    assert!(!rollback.exists());
    assert!(!profile_transaction_marker_path(profile.parent().unwrap(), &name).exists());
}
