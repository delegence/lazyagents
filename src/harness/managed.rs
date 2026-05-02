use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSurface {
    pub path: PathBuf,
    pub kind: ManagedSurfaceKind,
}

impl ManagedSurface {
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: ManagedSurfaceKind::File,
        }
    }

    pub fn directory(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: ManagedSurfaceKind::Directory,
        }
    }

    pub fn preserved_file(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: ManagedSurfaceKind::PreservedFile,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSurfaceKind {
    File,
    Directory,
    PreservedFile,
}

#[derive(Debug)]
pub struct ManagedBackup {
    backup_dir: PathBuf,
}

impl ManagedBackup {
    pub fn capture(
        lazyagents_home: &Path,
        harness_id: &str,
        surfaces: &[ManagedSurface],
    ) -> Result<Self> {
        let backups_root = lazyagents_home.join("backups");
        fs::create_dir_all(&backups_root)
            .with_context(|| format!("failed to create backups dir {}", backups_root.display()))?;

        let backup_dir = backups_root.join(harness_id);
        let temp_dir = backups_root.join(format!(".{harness_id}.tmp"));

        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)
                .with_context(|| format!("failed to remove temp dir {}", temp_dir.display()))?;
        }
        fs::create_dir_all(&temp_dir)
            .with_context(|| format!("failed to create temp dir {}", temp_dir.display()))?;

        let mut manifest = BackupManifest {
            surfaces: Vec::new(),
        };
        let mut used_backup_entries = BTreeSet::from(["metadata.json".to_string()]);

        for surface in surfaces {
            match fs::metadata(&surface.path) {
                Ok(metadata) if metadata.is_file() => {
                    let backup_entry =
                        unique_backup_entry(&surface.path, &mut used_backup_entries)?;
                    let backup_path = temp_dir.join(&backup_entry);
                    fs::copy(&surface.path, &backup_path).with_context(|| {
                        format!("failed to copy file {}", surface.path.display())
                    })?;
                    manifest.surfaces.push(BackupSurface {
                        original_path: surface.path.clone(),
                        surface_kind: surface.kind,
                        original_state: BackupSurfaceState::File,
                        backup_entry: Some(backup_entry),
                    });
                }
                Ok(metadata) if metadata.is_dir() => {
                    let backup_entry =
                        unique_backup_entry(&surface.path, &mut used_backup_entries)?;
                    let backup_path = temp_dir.join(&backup_entry);
                    copy_dir_all(&surface.path, &backup_path).with_context(|| {
                        format!("failed to copy directory {}", surface.path.display())
                    })?;
                    manifest.surfaces.push(BackupSurface {
                        original_path: surface.path.clone(),
                        surface_kind: surface.kind,
                        original_state: BackupSurfaceState::Directory,
                        backup_entry: Some(backup_entry),
                    });
                }
                Ok(_) => {
                    manifest.surfaces.push(BackupSurface {
                        original_path: surface.path.clone(),
                        surface_kind: surface.kind,
                        original_state: BackupSurfaceState::Other,
                        backup_entry: None,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    manifest.surfaces.push(BackupSurface {
                        original_path: surface.path.clone(),
                        surface_kind: surface.kind,
                        original_state: BackupSurfaceState::Missing,
                        backup_entry: None,
                    });
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to inspect {}", surface.path.display()));
                }
            };
        }

        write_manifest(&temp_dir, &manifest)?;

        if backup_dir.exists() {
            fs::remove_dir_all(&backup_dir).with_context(|| {
                format!("failed to remove old backup dir {}", backup_dir.display())
            })?;
        }
        fs::rename(&temp_dir, &backup_dir).with_context(|| {
            format!(
                "failed to rename temp dir {} to backup dir {}",
                temp_dir.display(),
                backup_dir.display()
            )
        })?;

        Ok(Self { backup_dir })
    }

    pub fn restore(&self, _surfaces: &[ManagedSurface]) -> Result<()> {
        if !self.backup_dir.exists() {
            return Ok(());
        }

        let manifest = read_manifest(&self.backup_dir)?;
        for entry in manifest.surfaces {
            let surface = ManagedSurface {
                path: entry.original_path.clone(),
                kind: entry.surface_kind,
            };
            remove_surface(&surface)?;

            match entry.original_state {
                BackupSurfaceState::Missing | BackupSurfaceState::Other => {}
                BackupSurfaceState::File => {
                    let backup_path = manifest_backup_path(&self.backup_dir, &entry)?;
                    if let Some(parent) = surface.path.parent() {
                        fs::create_dir_all(parent)
                            .with_context(|| format!("failed to create {}", parent.display()))?;
                    }
                    ensure_backup_kind(&backup_path, BackupSurfaceState::File)?;
                    fs::copy(&backup_path, &surface.path)
                        .with_context(|| format!("failed to restore {}", surface.path.display()))?;
                }
                BackupSurfaceState::Directory => {
                    let backup_path = manifest_backup_path(&self.backup_dir, &entry)?;
                    if let Some(parent) = surface.path.parent() {
                        fs::create_dir_all(parent)
                            .with_context(|| format!("failed to create {}", parent.display()))?;
                    }
                    ensure_backup_kind(&backup_path, BackupSurfaceState::Directory)?;
                    copy_dir_all(&backup_path, &surface.path)
                        .with_context(|| format!("failed to restore {}", surface.path.display()))?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct BackupManifest {
    surfaces: Vec<BackupSurface>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BackupSurface {
    original_path: PathBuf,
    surface_kind: ManagedSurfaceKind,
    original_state: BackupSurfaceState,
    backup_entry: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BackupSurfaceState {
    Missing,
    File,
    Directory,
    Other,
}

fn unique_backup_entry(path: &Path, used: &mut BTreeSet<String>) -> Result<String> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("surface path has no file name: {}", path.display()))?;

    if used.insert(file_name.to_string()) {
        return Ok(file_name.to_string());
    }

    let file_name_path = Path::new(file_name);
    let stem = file_name_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(file_name);
    let extension = file_name_path.extension().and_then(|ext| ext.to_str());

    for suffix in 1usize.. {
        let candidate = match extension {
            Some(extension) => format!("{stem}-{suffix}.{extension}"),
            None => format!("{file_name}-{suffix}"),
        };
        if used.insert(candidate.clone()) {
            return Ok(candidate);
        }
    }

    unreachable!("unbounded suffix loop always returns")
}

fn write_manifest(backup_dir: &Path, manifest: &BackupManifest) -> Result<()> {
    let path = backup_dir.join("metadata.json");
    let text = serde_json::to_string_pretty(manifest)?;
    fs::write(&path, format!("{text}\n"))
        .with_context(|| format!("failed to write backup manifest {}", path.display()))
}

fn read_manifest(backup_dir: &Path) -> Result<BackupManifest> {
    let path = backup_dir.join("metadata.json");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed to read backup manifest {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("invalid backup manifest {}", path.display()))
}

fn manifest_backup_path(backup_dir: &Path, entry: &BackupSurface) -> Result<PathBuf> {
    let backup_entry = entry.backup_entry.as_deref().with_context(|| {
        format!(
            "backup manifest entry for {} is missing backup_entry",
            entry.original_path.display()
        )
    })?;
    Ok(backup_dir.join(backup_entry))
}

fn ensure_backup_kind(path: &Path, expected: BackupSurfaceState) -> Result<()> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to inspect {}", path.display()))?;
    match expected {
        BackupSurfaceState::File if metadata.is_file() => Ok(()),
        BackupSurfaceState::Directory if metadata.is_dir() => Ok(()),
        _ => anyhow::bail!("backup entry {} has unexpected type", path.display()),
    }
}

pub fn clear_surfaces(surfaces: &[ManagedSurface]) -> Result<()> {
    for surface in surfaces {
        if matches!(surface.kind, ManagedSurfaceKind::PreservedFile) {
            continue;
        }
        remove_surface(surface)?;
        if matches!(surface.kind, ManagedSurfaceKind::Directory) {
            fs::create_dir_all(&surface.path)
                .with_context(|| format!("failed to create {}", surface.path.display()))?;
        }
    }
    Ok(())
}

pub fn write_text_atomic(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let temp_path = temp_write_path(path);
    {
        let mut file = fs::File::create(&temp_path)
            .with_context(|| format!("failed to create {}", temp_path.display()))?;
        file.write_all(contents.as_bytes())
            .with_context(|| format!("failed to write {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to flush {}", temp_path.display()))?;
    }

    fs::rename(&temp_path, path).with_context(|| {
        format!(
            "failed to rename {} to {}",
            temp_path.display(),
            path.display()
        )
    })
}

fn temp_write_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("lazyagents-config");
    path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()))
}

fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> Result<()> {
    fs::create_dir_all(&dst)
        .with_context(|| format!("failed to create directory {}", dst.as_ref().display()))?;
    for entry in fs::read_dir(&src)
        .with_context(|| format!("failed to read directory {}", src.as_ref().display()))?
    {
        let entry = entry?;
        let is_hidden = entry
            .file_name()
            .to_str()
            .map(|s| s.starts_with('.'))
            .unwrap_or(false);
        if is_hidden {
            continue;
        }

        let path = entry.path();
        let metadata = fs::metadata(&path)
            .with_context(|| format!("failed to get metadata for {}", path.display()))?;
        let dst_path = dst.as_ref().join(entry.file_name());
        if metadata.is_dir() {
            copy_dir_all(&path, dst_path)?;
        } else if metadata.is_file() {
            fs::copy(&path, &dst_path)
                .with_context(|| format!("failed to copy to {}", dst_path.display()))?;
        }
    }
    Ok(())
}

fn remove_surface(surface: &ManagedSurface) -> Result<()> {
    match fs::symlink_metadata(&surface.path) {
        Ok(metadata) => {
            if metadata.is_dir() && surface.kind == ManagedSurfaceKind::Directory {
                for entry in fs::read_dir(&surface.path).with_context(|| {
                    format!("failed to read directory {}", surface.path.display())
                })? {
                    let entry = entry?;
                    let is_hidden = entry
                        .file_name()
                        .to_str()
                        .map(|s| s.starts_with('.'))
                        .unwrap_or(false);
                    if !is_hidden {
                        let path = entry.path();
                        let entry_metadata = fs::symlink_metadata(&path).with_context(|| {
                            format!("failed to get metadata for {}", path.display())
                        })?;
                        if entry_metadata.is_dir() {
                            fs::remove_dir_all(&path).with_context(|| {
                                format!("failed to remove dir {}", path.display())
                            })?;
                        } else {
                            fs::remove_file(&path).with_context(|| {
                                format!("failed to remove file {}", path.display())
                            })?;
                        }
                    }
                }
                Ok(())
            } else if metadata.is_dir() {
                fs::remove_dir_all(&surface.path)
                    .with_context(|| format!("failed to remove {}", surface.path.display()))
            } else {
                fs::remove_file(&surface.path)
                    .with_context(|| format!("failed to remove {}", surface.path.display()))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to inspect {}", surface.path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn managed_backup_captures_and_restores_surfaces_on_disk() {
        let temp = tempfile::tempdir().unwrap();
        let lazyagents_home = temp.path().join("lazyagents");
        let harness_kind = crate::harness::kind::HarnessKind::Codex;

        let surfaces_dir = temp.path().join("surfaces");
        fs::create_dir_all(&surfaces_dir).unwrap();

        let file_path = surfaces_dir.join("test_file.txt");
        fs::write(&file_path, "file contents").unwrap();

        let dir_path = surfaces_dir.join("test_dir");
        fs::create_dir_all(&dir_path).unwrap();
        fs::write(dir_path.join("nested.txt"), "nested contents").unwrap();

        let missing_path = surfaces_dir.join("missing.txt");

        let surfaces = vec![
            ManagedSurface::file(&file_path),
            ManagedSurface::directory(&dir_path),
            ManagedSurface::file(&missing_path),
        ];

        // Capture
        let backup =
            ManagedBackup::capture(&lazyagents_home, harness_kind.id(), &surfaces).unwrap();

        // Assert backup is on disk
        let backup_dir = lazyagents_home.join("backups").join(harness_kind.id());
        assert!(backup_dir.exists());
        assert!(backup_dir.join("metadata.json").exists());
        assert!(backup_dir.join("test_file.txt").exists());
        assert!(backup_dir.join("test_dir").join("nested.txt").exists());
        assert!(!backup_dir.join("missing.txt").exists());
        let manifest: BackupManifest =
            serde_json::from_str(&fs::read_to_string(backup_dir.join("metadata.json")).unwrap())
                .unwrap();
        assert_eq!(manifest.surfaces.len(), 3);
        assert_eq!(manifest.surfaces[0].original_path, file_path);
        assert_eq!(manifest.surfaces[0].surface_kind, ManagedSurfaceKind::File);
        assert_eq!(
            manifest.surfaces[0].original_state,
            BackupSurfaceState::File
        );
        assert_eq!(
            manifest.surfaces[0].backup_entry.as_deref(),
            Some("test_file.txt")
        );
        assert_eq!(
            manifest.surfaces[1].original_state,
            BackupSurfaceState::Directory
        );
        assert_eq!(
            manifest.surfaces[1].backup_entry.as_deref(),
            Some("test_dir")
        );
        assert_eq!(
            manifest.surfaces[2].original_state,
            BackupSurfaceState::Missing
        );
        assert_eq!(manifest.surfaces[2].backup_entry, None);

        // Assert no temp dir remains
        let temp_dir = lazyagents_home
            .join("backups")
            .join(format!(".{}.tmp", harness_kind.id()));
        assert!(!temp_dir.exists());

        // Modify surfaces
        fs::write(&file_path, "changed contents").unwrap();
        fs::remove_dir_all(&dir_path).unwrap();
        fs::write(&missing_path, "suddenly exists").unwrap();

        // Restore
        backup.restore(&surfaces).unwrap();

        // Assert states reverted correctly
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "file contents");
        assert_eq!(
            fs::read_to_string(dir_path.join("nested.txt")).unwrap(),
            "nested contents"
        );
        assert!(!missing_path.exists());
    }

    #[test]
    fn managed_backup_suffixes_duplicate_backup_entry_names() {
        let temp = tempfile::tempdir().unwrap();
        let lazyagents_home = temp.path().join("lazyagents");
        let harness_kind = crate::harness::kind::HarnessKind::Codex;

        let first_dir = temp.path().join("first");
        let second_dir = temp.path().join("second");
        fs::create_dir_all(&first_dir).unwrap();
        fs::create_dir_all(&second_dir).unwrap();
        let first = first_dir.join("settings.json");
        let second = second_dir.join("settings.json");
        fs::write(&first, "first").unwrap();
        fs::write(&second, "second").unwrap();

        let surfaces = vec![ManagedSurface::file(&first), ManagedSurface::file(&second)];

        ManagedBackup::capture(&lazyagents_home, harness_kind.id(), &surfaces).unwrap();

        let backup_dir = lazyagents_home.join("backups").join(harness_kind.id());
        assert_eq!(
            fs::read_to_string(backup_dir.join("settings.json")).unwrap(),
            "first"
        );
        assert_eq!(
            fs::read_to_string(backup_dir.join("settings-1.json")).unwrap(),
            "second"
        );
        let manifest: BackupManifest =
            serde_json::from_str(&fs::read_to_string(backup_dir.join("metadata.json")).unwrap())
                .unwrap();
        assert_eq!(
            manifest.surfaces[0].backup_entry.as_deref(),
            Some("settings.json")
        );
        assert_eq!(
            manifest.surfaces[1].backup_entry.as_deref(),
            Some("settings-1.json")
        );
    }

    #[test]
    fn managed_surface_directory_ignores_hidden_files() {
        let temp = tempfile::tempdir().unwrap();
        let lazyagents_home = temp.path().join("lazyagents");
        let harness_kind = crate::harness::kind::HarnessKind::Codex;

        let dir_path = temp.path().join("skills");
        fs::create_dir_all(&dir_path).unwrap();
        fs::write(dir_path.join("visible.txt"), "visible").unwrap();
        fs::write(dir_path.join(".hidden.txt"), "hidden").unwrap();

        let hidden_dir = dir_path.join(".system");
        fs::create_dir_all(&hidden_dir).unwrap();
        fs::write(hidden_dir.join("system_skill.txt"), "system").unwrap();

        let surfaces = vec![ManagedSurface::directory(&dir_path)];

        // Capture backup
        let backup =
            ManagedBackup::capture(&lazyagents_home, harness_kind.id(), &surfaces).unwrap();

        // Check backup dir doesn't contain hidden files
        let backup_dir = lazyagents_home.join("backups").join(harness_kind.id());
        assert!(backup_dir.join("skills").join("visible.txt").exists());
        assert!(!backup_dir.join("skills").join(".hidden.txt").exists());
        assert!(!backup_dir.join("skills").join(".system").exists());

        // Clear surfaces (simulating profile apply)
        super::clear_surfaces(&surfaces).unwrap();

        // Assert original visible is gone, but hidden ones remain
        assert!(!dir_path.join("visible.txt").exists());
        assert!(dir_path.join(".hidden.txt").exists());
        assert!(dir_path.join(".system").join("system_skill.txt").exists());

        // Restore backup
        backup.restore(&surfaces).unwrap();

        // Assert visible is back, hidden still remain unchanged
        assert_eq!(
            fs::read_to_string(dir_path.join("visible.txt")).unwrap(),
            "visible"
        );
        assert_eq!(
            fs::read_to_string(dir_path.join(".hidden.txt")).unwrap(),
            "hidden"
        );
        assert_eq!(
            fs::read_to_string(dir_path.join(".system").join("system_skill.txt")).unwrap(),
            "system"
        );
    }

    #[test]
    fn managed_backup_restore_fails_clearly_for_malformed_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let lazyagents_home = temp.path().join("lazyagents");
        let harness_kind = crate::harness::kind::HarnessKind::Codex;
        let file_path = temp.path().join("settings.json");
        fs::write(&file_path, "before").unwrap();
        let surfaces = vec![ManagedSurface::file(&file_path)];

        let backup =
            ManagedBackup::capture(&lazyagents_home, harness_kind.id(), &surfaces).unwrap();
        fs::write(
            lazyagents_home
                .join("backups")
                .join(harness_kind.id())
                .join("metadata.json"),
            "not json",
        )
        .unwrap();

        let error = backup.restore(&surfaces).unwrap_err();
        assert!(format!("{error:#}").contains("invalid backup manifest"));
    }

    #[test]
    fn write_text_atomic_uses_same_directory_temp_file_and_replaces_content() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("config.json");

        write_text_atomic(&target, "{\"old\":true}").unwrap();
        write_text_atomic(&target, "{\"new\":true}").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "{\"new\":true}");

        let leftovers = fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(leftovers, vec!["config.json"]);

        let temp_path = temp_write_path(&target);
        assert_eq!(temp_path.parent(), target.parent());
        assert_eq!(
            temp_path.file_name().unwrap().to_string_lossy(),
            ".config.json.".to_owned() + &std::process::id().to_string() + ".tmp"
        );
    }
}
