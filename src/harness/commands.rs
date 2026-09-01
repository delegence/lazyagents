use crate::harness::drift::DriftItem;
use crate::harness::fs::{
    collect_file_content_drift, copy_file, has_visible_entries, import_files_recursive_filtered,
    is_hidden_name,
};
use crate::harness::integration::{HarnessConfigPaths, ImportedFile, ProfileRef};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub fn copy_commands(profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
    for command in profile_commands_recursive(&profile.path)? {
        let relative = command.strip_prefix(profile.path.join("commands")).unwrap();
        copy_file(&command, &paths.commands_dir.join(relative))?;
    }
    Ok(())
}

pub fn collect_commands_drift_recursive(
    expected_sources: Vec<PathBuf>,
    target_dir: &Path,
    profile_cmd_dir: &Path,
    items: &mut Vec<DriftItem>,
) -> Result<()> {
    if report_wrong_command_root(target_dir, items)? {
        return Ok(());
    }
    let mut expected_files = BTreeSet::new();
    let mut expected_dirs = BTreeSet::new();
    for source in expected_sources {
        let relative = source.strip_prefix(profile_cmd_dir).unwrap().to_path_buf();
        for ancestor in relative.ancestors().skip(1) {
            if !ancestor.as_os_str().is_empty() {
                expected_dirs.insert(ancestor.to_path_buf());
            }
        }
        expected_files.insert(relative.clone());
        collect_file_content_drift("commands", &source, &target_dir.join(relative), items)?;
    }
    collect_unexpected_command_entries(
        target_dir,
        target_dir,
        &expected_files,
        &expected_dirs,
        items,
    )
}

fn collect_unexpected_command_entries(
    root: &Path,
    path: &Path,
    expected_files: &BTreeSet<PathBuf>,
    expected_dirs: &BTreeSet<PathBuf>,
    items: &mut Vec<DriftItem>,
) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            items.push(DriftItem {
                surface: "commands".to_string(),
                detail: format!(
                    "managed commands root has the wrong type: {}",
                    path.display()
                ),
            });
            return Ok(());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()))
        }
    }
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        if is_hidden_name(&entry.file_name()) {
            continue;
        }
        let entry_path = entry.path();
        let relative = entry_path
            .strip_prefix(root)
            .with_context(|| format!("{} is not under {}", entry_path.display(), root.display()))?
            .to_path_buf();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if expected_dirs.contains(&relative) {
                collect_unexpected_command_entries(
                    root,
                    &entry_path,
                    expected_files,
                    expected_dirs,
                    items,
                )?;
            } else if has_visible_entries(&entry_path)? {
                items.push(DriftItem {
                    surface: "commands".to_string(),
                    detail: format!("unexpected harness entry {}", entry_path.display()),
                });
            }
        } else if !expected_files.contains(&relative) {
            items.push(DriftItem {
                surface: "commands".to_string(),
                detail: format!("unexpected harness entry {}", entry_path.display()),
            });
        }
    }
    Ok(())
}

pub fn import_commands(path: &Path) -> Result<Vec<ImportedFile>> {
    let mut commands = Vec::new();
    if !path.exists() {
        return Ok(commands);
    }
    commands = import_files_recursive_filtered(path, path, &|relative| {
        relative.extension().is_some_and(|ext| ext == "md")
    })?;
    commands.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(commands)
}

pub fn profile_commands_recursive(profile_path: &Path) -> Result<Vec<PathBuf>> {
    let commands_dir = profile_path.join("commands");
    let mut commands = Vec::new();
    if !commands_dir.exists() {
        return Ok(commands);
    }
    collect_commands_recursive(&commands_dir, &mut commands)?;
    commands.sort();
    Ok(commands)
}

fn collect_commands_recursive(dir: &Path, commands: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .map(|name| name.starts_with('.'))
            .unwrap_or(false)
        {
            continue;
        }
        if entry.file_type()?.is_dir() {
            collect_commands_recursive(&entry.path(), commands)?;
        } else if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "md")
        {
            commands.push(entry.path());
        }
    }
    Ok(())
}

pub fn flat_profile_commands(profile_path: &Path) -> Result<Vec<PathBuf>> {
    let commands_dir = profile_path.join("commands");
    let mut commands = Vec::new();
    if !commands_dir.exists() {
        return Ok(commands);
    }
    for entry in fs::read_dir(&commands_dir)
        .with_context(|| format!("failed to read {}", commands_dir.display()))?
    {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .map(|name| name.starts_with('.'))
            .unwrap_or(false)
        {
            continue;
        }
        if entry.file_type()?.is_dir() {
            let has_markdown = contains_markdown_file(&entry.path())?;
            if has_markdown {
                anyhow::bail!(
                    "nested profile commands are not supported by this harness: {}",
                    entry.path().display()
                );
            }
        } else if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "md")
        {
            commands.push(entry.path());
        }
    }
    commands.sort();
    Ok(commands)
}

fn contains_markdown_file(dir: &Path) -> Result<bool> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .map(|name| name.starts_with('.'))
            .unwrap_or(false)
        {
            continue;
        }
        if entry.file_type()?.is_dir() {
            if contains_markdown_file(&entry.path())? {
                return Ok(true);
            }
        } else if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "md")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn copy_flat_commands(profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
    for command in flat_profile_commands(&profile.path)? {
        let target = paths.commands_dir.join(
            command
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("invalid command path {}", command.display()))?,
        );
        copy_file(&command, &target)?;
    }
    Ok(())
}

pub fn collect_flat_commands_drift(
    profile_path: &Path,
    target_dir: &Path,
    items: &mut Vec<DriftItem>,
) -> Result<()> {
    if report_wrong_command_root(target_dir, items)? {
        return Ok(());
    }
    let commands = flat_profile_commands(profile_path)?;
    let expected_names = commands
        .iter()
        .filter_map(|command| command.file_name().map(ToOwned::to_owned))
        .collect::<BTreeSet<_>>();
    for command in commands {
        let name = command
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("invalid command path {}", command.display()))?;
        collect_file_content_drift("commands", &command, &target_dir.join(name), items)?;
    }
    if target_dir.exists() {
        for entry in fs::read_dir(target_dir)
            .with_context(|| format!("failed to read {}", target_dir.display()))?
        {
            let entry = entry?;
            if is_hidden_name(&entry.file_name()) || expected_names.contains(&entry.file_name()) {
                continue;
            }
            if entry.file_type()?.is_dir() && !has_visible_entries(&entry.path())? {
                continue;
            }
            items.push(DriftItem {
                surface: "commands".to_string(),
                detail: format!("unexpected harness entry {}", entry.path().display()),
            });
        }
    }
    Ok(())
}

fn report_wrong_command_root(path: &Path, items: &mut Vec<DriftItem>) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            items.push(DriftItem {
                surface: "commands".to_string(),
                detail: format!(
                    "managed commands root has the wrong type: {}",
                    path.display()
                ),
            });
            Ok(true)
        }
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

pub fn import_flat_commands(path: &Path) -> Result<Vec<ImportedFile>> {
    let mut commands = Vec::new();
    if !path.exists() {
        return Ok(commands);
    }
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .map(|name| name.starts_with('.'))
            .unwrap_or(false)
        {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            anyhow::bail!(
                "flat command import does not support symlink {}",
                entry.path().display()
            );
        }
        if metadata.is_dir() {
            if contains_markdown_file(&entry.path())? {
                anyhow::bail!(
                    "nested command import is not supported by this harness: {}",
                    entry.path().display()
                );
            }
            continue;
        }
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "md")
        {
            commands.push(ImportedFile {
                relative_path: PathBuf::from(entry.file_name()),
                contents: fs::read(entry.path())
                    .with_context(|| format!("failed to read {}", entry.path().display()))?,
                unix_mode: None,
            });
        }
    }
    commands.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(commands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_command_errors_are_harness_neutral() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        let commands = profile.join("commands");
        fs::create_dir_all(commands.join("nested")).unwrap();
        fs::write(commands.join("nested/run.md"), "run").unwrap();

        let profile_error = flat_profile_commands(&profile).unwrap_err().to_string();
        let import_error = import_flat_commands(&commands).unwrap_err().to_string();

        assert!(profile_error.contains("not supported by this harness"));
        assert!(import_error.contains("not supported by this harness"));
        assert!(!profile_error.contains("Codex"));
        assert!(!import_error.contains("Codex"));
    }

    #[test]
    fn wrong_command_roots_are_reported_as_drift() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("commands");
        fs::write(&target, "wrong type").unwrap();
        let mut recursive = Vec::new();
        collect_commands_drift_recursive(Vec::new(), &target, temp.path(), &mut recursive).unwrap();
        let mut flat = Vec::new();
        collect_flat_commands_drift(temp.path(), &target, &mut flat).unwrap();
        assert!(recursive
            .iter()
            .any(|item| item.detail.contains("wrong type")));
        assert!(flat.iter().any(|item| item.detail.contains("wrong type")));
    }
}
