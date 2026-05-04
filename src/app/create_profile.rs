use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::app::harness_registry::HarnessRegistry;
use crate::harness::integration::{AppEnvironment, HarnessDetection, ImportedDirectory};
use crate::harness::skills::import_skills;
use crate::profile::{ProfileName, ProfileStore};

pub enum CreateProfileResult {
    Created {
        profile: ProfileName,
        path: PathBuf,
    },
    Imported {
        profile: ProfileName,
        harness: String,
        path: PathBuf,
    },
}

pub fn create_profile(
    registry: &dyn HarnessRegistry,
    env: &AppEnvironment,
    store: &ProfileStore,
    profile: ProfileName,
    from: Option<String>,
) -> Result<CreateProfileResult> {
    match from {
        Some(id) => {
            let integration = registry
                .get(env, &id)?
                .ok_or_else(|| anyhow::anyhow!("unsupported harness {id}"))?;
            match integration.detect(env)? {
                HarnessDetection::Detected { .. } => {}
                HarnessDetection::NotDetected => anyhow::bail!("{id} was not detected on PATH"),
            }
            let paths = integration.paths(env)?;
            let path = store.create_skeleton(&profile)?;
            if let Err(error) = (|| {
                let mut imported = integration.import_from_harness(&paths)?;
                let shared_skills = merge_shared_agent_skills(&mut imported.skills, env)?;
                store.apply_import(&profile, integration.instance_id(), imported)?;
                remove_imported_shared_skills(&shared_skills)?;
                Ok::<(), anyhow::Error>(())
            })() {
                let _ = std::fs::remove_dir_all(&path);
                return Err(error.context(format!("failed to import from {id}")));
            }
            Ok(CreateProfileResult::Imported {
                profile,
                harness: integration.instance_id().to_string(),
                path,
            })
        }
        None => {
            let path = store.create_skeleton(&profile)?;
            Ok(CreateProfileResult::Created { profile, path })
        }
    }
}

pub(crate) fn merge_shared_agent_skills(
    harness_skills: &mut Vec<ImportedDirectory>,
    env: &AppEnvironment,
) -> Result<Vec<PathBuf>> {
    let shared_skills_dir = env.user_home.join(".agents").join("skills");
    let shared_skills = import_skills(&shared_skills_dir)?;
    let mut native_names = harness_skills
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<BTreeSet<_>>();
    let mut imported_shared_paths = Vec::new();

    for skill in shared_skills {
        if native_names.insert(skill.name.clone()) {
            imported_shared_paths.push(shared_skills_dir.join(&skill.name));
            harness_skills.push(skill);
        }
    }

    harness_skills.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(imported_shared_paths)
}

pub(crate) fn remove_imported_shared_skills(paths: &[PathBuf]) -> Result<()> {
    for path in paths {
        remove_shared_skill(path)
            .with_context(|| format!("failed to remove shared skill at {}", path.display()))?;
    }
    Ok(())
}

fn remove_shared_skill(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || metadata.is_file() {
        std::fs::remove_file(path)
            .with_context(|| format!("failed to remove file {}", path.display()))
    } else if metadata.is_dir() {
        std::fs::remove_dir_all(path)
            .with_context(|| format!("failed to remove directory {}", path.display()))
    } else {
        Ok(())
    }
}
