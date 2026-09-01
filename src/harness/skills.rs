use crate::harness::drift::DriftItem;
use crate::harness::fs::{
    collect_directory_content_drift, copy_directory, has_visible_entries, import_files_recursive,
    import_tree_lossless,
};
use crate::harness::integration::{HarnessConfigPaths, ImportedDirectory, ProfileRef};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub fn copy_skills(profile: &ProfileRef, paths: &HarnessConfigPaths) -> Result<()> {
    for skill in valid_skills(&profile.path)? {
        let target = paths.skills_dir.join(
            skill
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("invalid skill path {}", skill.display()))?,
        );
        copy_directory(&skill, &target)?;
    }
    Ok(())
}

pub fn collect_skills_drift(
    profile_path: &Path,
    target_dir: &Path,
    items: &mut Vec<DriftItem>,
) -> Result<()> {
    let skills = valid_skills(profile_path)?;
    match fs::symlink_metadata(target_dir) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            items.push(DriftItem {
                surface: "skills".to_string(),
                detail: format!(
                    "managed skills root has the wrong type: {}",
                    target_dir.display()
                ),
            });
            return Ok(());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect {}", target_dir.display()))
        }
    }
    let expected_names = skills
        .iter()
        .filter_map(|skill| skill.file_name().map(ToOwned::to_owned))
        .collect::<std::collections::BTreeSet<_>>();

    for skill in skills {
        let name = skill
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("invalid skill path {}", skill.display()))?;
        collect_directory_content_drift("skills", &skill, &target_dir.join(name), items)?;
    }

    if target_dir.exists() {
        for entry in fs::read_dir(target_dir)
            .with_context(|| format!("failed to read {}", target_dir.display()))?
        {
            let entry = entry?;
            if crate::harness::fs::is_hidden_name(&entry.file_name())
                || expected_names.contains(&entry.file_name())
            {
                continue;
            }
            if entry.file_type()?.is_dir() && !has_visible_entries(&entry.path())? {
                continue;
            }
            items.push(DriftItem {
                surface: "skills".to_string(),
                detail: format!("unexpected harness entry {}", entry.path().display()),
            });
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
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if !entry.path().metadata()?.is_dir() || !entry.path().join("SKILL.md").is_file() {
            continue;
        }
        skills.push(ImportedDirectory {
            name,
            unix_mode: None,
            files: import_files_recursive(&entry.path(), &entry.path())?,
            directories: Vec::new(),
        });
    }
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(skills)
}

pub fn import_shared_skills(path: &Path) -> Result<Vec<ImportedDirectory>> {
    let mut skills = Vec::new();
    if !path.exists() {
        return Ok(skills);
    }
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || !entry.path().join("SKILL.md").is_file()
        {
            continue;
        }
        let tree = import_tree_lossless(&entry.path())?;
        skills.push(ImportedDirectory {
            name,
            unix_mode: tree.root_unix_mode,
            files: tree.files,
            directories: tree.directories,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn import_skills_ignores_hidden_root_skill_dirs() {
        let temp = tempfile::tempdir().unwrap();
        let skills_dir = temp.path();

        fs::create_dir_all(skills_dir.join("visible")).unwrap();
        fs::write(skills_dir.join("visible").join("SKILL.md"), "visible").unwrap();

        fs::create_dir_all(skills_dir.join(".hidden")).unwrap();
        fs::write(skills_dir.join(".hidden").join("SKILL.md"), "hidden").unwrap();

        let skills = import_skills(skills_dir).unwrap();
        let names = skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["visible"]);
    }

    #[test]
    fn wrong_skills_root_type_is_reported_as_drift() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        fs::create_dir_all(profile.join("skills/tool")).unwrap();
        fs::write(profile.join("skills/tool/SKILL.md"), "skill").unwrap();
        let target = temp.path().join("target-skills");
        fs::write(&target, "wrong type").unwrap();
        let mut items = Vec::new();
        collect_skills_drift(&profile, &target, &mut items).unwrap();
        assert!(items.iter().any(|item| item.detail.contains("wrong type")));
    }

    #[cfg(unix)]
    #[test]
    fn shared_skill_import_preserves_hidden_files_and_executable_mode() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join("tool");
        fs::create_dir_all(skill.join("nested/.config")).unwrap();
        fs::write(skill.join("SKILL.md"), "skill").unwrap();
        fs::write(skill.join("nested/.config/.env"), "TOKEN=x").unwrap();
        fs::write(skill.join("run.sh"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(skill.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&skill, fs::Permissions::from_mode(0o750)).unwrap();
        fs::set_permissions(skill.join("nested"), fs::Permissions::from_mode(0o700)).unwrap();

        let imported = import_shared_skills(temp.path()).unwrap();
        let file = imported[0]
            .files
            .iter()
            .find(|file| file.relative_path == Path::new("nested/.config/.env"))
            .unwrap();
        assert_eq!(file.contents, b"TOKEN=x");
        let script = imported[0]
            .files
            .iter()
            .find(|file| file.relative_path == Path::new("run.sh"))
            .unwrap();
        assert_eq!(script.unix_mode, Some(0o755));
        assert_eq!(imported[0].unix_mode, Some(0o750));
        assert!(imported[0].directories.iter().any(|directory| {
            directory.relative_path == Path::new("nested") && directory.unix_mode == Some(0o700)
        }));
    }

    #[cfg(unix)]
    #[test]
    fn applied_skill_preserves_directory_modes() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let profile_path = temp.path().join("profile");
        let skill = profile_path.join("skills/private");
        fs::create_dir_all(skill.join("parent")).unwrap();
        fs::write(skill.join("SKILL.md"), "skill").unwrap();
        fs::write(skill.join("parent/data.txt"), "data").unwrap();
        fs::set_permissions(&skill, fs::Permissions::from_mode(0o750)).unwrap();
        fs::set_permissions(skill.join("parent"), fs::Permissions::from_mode(0o700)).unwrap();
        let target = temp.path().join("target");
        let paths = HarnessConfigPaths {
            config_dir: target.clone(),
            instruction_target: target.join("AGENTS.md"),
            skills_dir: target.join("skills"),
            commands_dir: target.join("commands"),
            agents_dir: target.join("agents"),
            settings_file: target.join("settings.json"),
            mcp_file: target.join("mcp.json"),
        };
        copy_skills(
            &ProfileRef {
                name: crate::profile::ProfileName::parse("work").unwrap(),
                path: profile_path,
                harness_id: "codex".to_string(),
            },
            &paths,
        )
        .unwrap();

        assert_eq!(
            fs::metadata(paths.skills_dir.join("private"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
        assert_eq!(
            fs::metadata(paths.skills_dir.join("private/parent"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn shared_skill_import_rejects_symlinks_without_mutation() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join("tool");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "skill").unwrap();
        fs::write(skill.join("target"), "data").unwrap();
        symlink("target", skill.join("link")).unwrap();

        assert!(import_shared_skills(temp.path()).is_err());
        assert!(skill.join("link").is_symlink());
        assert_eq!(fs::read_to_string(skill.join("target")).unwrap(), "data");
    }

    #[cfg(unix)]
    #[test]
    fn shared_skill_import_rejects_fifo_without_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join("tool");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "skill").unwrap();
        let status = std::process::Command::new("mkfifo")
            .arg(skill.join("pipe"))
            .status()
            .unwrap();
        assert!(status.success());

        assert!(import_shared_skills(temp.path()).is_err());
        assert!(skill.join("pipe").exists());
    }
}
