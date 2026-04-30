use crate::harness::drift::DriftItem;
use crate::harness::fs::{symlink_dir, symlink_file, symlink_points_to};
use crate::harness::integration::{
    HarnessConfigPaths, ImportedDirectory, ImportedFile, ProfileRef,
};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub fn link_skills(profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
    for skill in valid_skills(&profile.path)? {
        let target = paths.skills_dir.join(
            skill
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("invalid skill path {}", skill.display()))?,
        );
        symlink_dir(skill, target)?;
    }
    Ok(())
}

pub fn link_commands(profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
    for command in profile_commands_recursive(&profile.path)? {
        let relative = command.strip_prefix(profile.path.join("commands")).unwrap();
        let target = paths.commands_dir.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        symlink_file(command, target)?;
    }
    Ok(())
}

pub fn collect_directory_link_drift(
    surface: &str,
    expected_sources: Vec<PathBuf>,
    target_dir: &Path,
    items: &mut Vec<DriftItem>,
) -> Result<()> {
    let mut expected_names = BTreeSet::new();
    for source in expected_sources {
        let name = source
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("invalid source path {}", source.display()))?
            .to_string_lossy()
            .into_owned();
        expected_names.insert(name.clone());
        if !symlink_points_to(&target_dir.join(&name), &source) {
            items.push(DriftItem {
                surface: surface.to_string(),
                detail: format!(
                    "{} is not linked to active profile",
                    target_dir.join(&name).display()
                ),
            });
        }
    }
    if target_dir.exists() {
        for entry in fs::read_dir(target_dir)
            .with_context(|| format!("failed to read {}", target_dir.display()))?
        {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if !expected_names.contains(&name) {
                items.push(DriftItem {
                    surface: surface.to_string(),
                    detail: format!("unexpected managed entry {}", entry.path().display()),
                });
            }
        }
    }
    Ok(())
}

pub fn collect_directory_link_drift_recursive(
    surface: &str,
    expected_sources: Vec<PathBuf>,
    target_dir: &Path,
    profile_cmd_dir: &Path,
    items: &mut Vec<DriftItem>,
) -> Result<()> {
    let mut expected_rel_paths = BTreeSet::new();
    for source in expected_sources {
        let rel_path = source.strip_prefix(profile_cmd_dir).unwrap().to_path_buf();
        expected_rel_paths.insert(rel_path.clone());
        let target = target_dir.join(&rel_path);
        if !symlink_points_to(&target, &source) {
            items.push(DriftItem {
                surface: surface.to_string(),
                detail: format!("{} is not linked to active profile", target.display()),
            });
        }
    }
    if target_dir.exists() {
        let actual_files = import_files_recursive(target_dir, target_dir)?;
        for file in actual_files {
            if !expected_rel_paths.contains(&file.relative_path) {
                items.push(DriftItem {
                    surface: surface.to_string(),
                    detail: format!(
                        "unexpected managed entry {}",
                        target_dir.join(&file.relative_path).display()
                    ),
                });
            }
        }
    }
    Ok(())
}

pub fn import_skills(path: &Path) -> Result<Vec<ImportedDirectory>> {
    let mut skills = Vec::new();
    if !path.exists() {
        return Ok(skills);
    }
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        if !entry.path().metadata()?.is_dir() || !entry.path().join("SKILL.md").is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        skills.push(ImportedDirectory {
            name,
            files: import_files_recursive(&entry.path(), &entry.path())?,
        });
    }
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(skills)
}

pub fn import_commands(path: &Path) -> Result<Vec<ImportedFile>> {
    let mut commands = Vec::new();
    if !path.exists() {
        return Ok(commands);
    }
    let files = import_files_recursive(path, path)?;
    for file in files {
        if file
            .relative_path
            .extension()
            .is_some_and(|ext| ext == "md")
        {
            commands.push(file);
        }
    }
    commands.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(commands)
}

pub fn import_files_recursive(root: &Path, path: &Path) -> Result<Vec<ImportedFile>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.metadata()?.is_dir() {
            files.extend(import_files_recursive(root, &path)?);
        } else if path.metadata()?.is_file() {
            files.push(ImportedFile {
                relative_path: path
                    .strip_prefix(root)
                    .with_context(|| format!("{} is not under {}", path.display(), root.display()))?
                    .to_path_buf(),
                contents: fs::read(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?,
            });
        }
    }
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

pub fn valid_skills(profile_path: &Path) -> Result<Vec<PathBuf>> {
    let skills_dir = profile_path.join("skills");
    let mut skills = Vec::new();
    if !skills_dir.exists() {
        return Ok(skills);
    }
    for entry in fs::read_dir(&skills_dir)
        .with_context(|| format!("failed to read {}", skills_dir.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("SKILL.md").is_file() {
            skills.push(entry.path());
        }
    }
    skills.sort();
    Ok(skills)
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
        if entry.file_type()?.is_dir() {
            let has_markdown = contains_markdown_file(&entry.path())?;
            if has_markdown {
                anyhow::bail!(
                    "Codex does not support nested profile commands: {}",
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

pub fn link_flat_commands(profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
    for command in flat_profile_commands(&profile.path)? {
        let target = paths.commands_dir.join(
            command
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("invalid command path {}", command.display()))?,
        );
        symlink_file(command, target)?;
    }
    Ok(())
}

pub fn import_flat_commands(path: &Path) -> Result<Vec<ImportedFile>> {
    let mut commands = Vec::new();
    if !path.exists() {
        return Ok(commands);
    }
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        if entry.path().metadata()?.is_dir() {
            if contains_markdown_file(&entry.path())? {
                anyhow::bail!(
                    "Codex command import does not support nested commands: {}",
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
            });
        }
    }
    commands.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(commands)
}
