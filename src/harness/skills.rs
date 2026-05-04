use crate::harness::fs::{import_files_recursive, symlink_dir};
use crate::harness::integration::{HarnessConfigPaths, ImportedDirectory, ProfileRef};
use anyhow::{Context, Result};
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
        if entry
            .file_name()
            .to_str()
            .map(|name| name.starts_with('.'))
            .unwrap_or(false)
        {
            continue;
        }
        if entry.file_type()?.is_dir() && entry.path().join("SKILL.md").is_file() {
            skills.push(entry.path());
        }
    }
    skills.sort();
    Ok(skills)
}
