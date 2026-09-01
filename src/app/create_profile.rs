use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

use crate::app::harness_registry::HarnessRegistry;
use crate::harness::integration::{AppEnvironment, HarnessDetection, ImportedDirectory};
use crate::harness::skills::import_shared_skills;
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
        cleanup_warning: Option<String>,
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
            let shared_transaction = match (|| {
                let mut imported = integration.import_from_harness(&paths)?;
                if matches!(imported.instruction.as_deref(), Some(instruction) if instruction.trim().is_empty())
                {
                    imported.instruction = None;
                }
                let shared_skills = SharedSkillTransaction::prepare(&mut imported.skills, env)?;
                if let Err(error) =
                    store.apply_import(&profile, integration.instance_id(), imported)
                {
                    return Err(with_shared_skill_rollback(error, shared_skills));
                }
                Ok::<_, anyhow::Error>(shared_skills)
            })() {
                Ok(shared_skills) => shared_skills,
                Err(error) => {
                    let _ = std::fs::remove_dir_all(&path);
                    return Err(error.context(format!("failed to import from {id}")));
                }
            };

            // The imported profile is now the durable copy. Never delete it if
            // cleaning up the shared source fails, or successfully removed skills
            // could be lost along with the profile.
            let cleanup_warning = shared_transaction.commit()
                .err()
                .map(|error| {
                    format!(
                        "profile {profile} was created, but imported shared-skill cleanup failed: {error}; do not retry the import"
                    )
                });
            Ok(CreateProfileResult::Imported {
                profile,
                harness: integration.instance_id().to_string(),
                path,
                cleanup_warning,
            })
        }
        None => {
            let path = store.create_skeleton(&profile)?;
            Ok(CreateProfileResult::Created { profile, path })
        }
    }
}

pub(crate) struct SharedSkillTransaction {
    shared_skills_dir: PathBuf,
    imported: Vec<ImportedDirectory>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SharedSkillCleanupMarker {
    imported: Vec<ImportedDirectory>,
}

impl SharedSkillTransaction {
    pub(crate) fn prepare(
        harness_skills: &mut Vec<ImportedDirectory>,
        env: &AppEnvironment,
    ) -> Result<Self> {
        let shared_skills_dir = env.user_home.join(".agents").join("skills");
        if !shared_skills_dir.is_dir() {
            return Ok(Self {
                shared_skills_dir,
                imported: Vec::new(),
            });
        }
        let mut native_names = harness_skills
            .iter()
            .map(|skill| skill.name.clone())
            .collect::<BTreeSet<_>>();
        let imported = import_shared_skills(&shared_skills_dir)?
            .into_iter()
            .filter(|skill| !native_names.contains(&skill.name))
            .collect::<Vec<_>>();
        for skill in &imported {
            native_names.insert(skill.name.clone());
            harness_skills.push(skill.clone());
        }
        harness_skills.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(Self {
            shared_skills_dir,
            imported,
        })
    }

    pub(crate) fn commit(self) -> Result<()> {
        if self.imported.is_empty() {
            return Ok(());
        }
        let mut quarantine_holder = Some(
            tempfile::Builder::new()
                .prefix(".lazyagents-import-")
                .tempdir_in(&self.shared_skills_dir)?,
        );
        let mut quarantine = quarantine_holder
            .as_ref()
            .expect("quarantine exists")
            .path()
            .to_path_buf();
        crate::file_system::write_text_atomic(
            &quarantine.join(".committed-cleanup"),
            &format!(
                "{}\n",
                serde_json::to_string(&SharedSkillCleanupMarker {
                    imported: self.imported.clone(),
                })?
            ),
        )?;
        for expected in self.imported {
            let source = self.shared_skills_dir.join(&expected.name);
            let staged = quarantine.join(&expected.name);
            if let Err(error) = std::fs::rename(&source, &staged) {
                if quarantine_holder.is_none() {
                    let _ = std::fs::remove_file(quarantine.join(".committed-cleanup"));
                    let _ = std::fs::remove_dir(&quarantine);
                }
                return Err(error).with_context(|| {
                    format!("failed to stage shared skill {} for cleanup", expected.name)
                });
            }
            if let Some(holder) = quarantine_holder.take() {
                quarantine = holder.keep();
            }
            sync_shared_directory(&self.shared_skills_dir)?;
            if cfg!(debug_assertions)
                && std::env::var_os("LAZYAGENTS_TEST_EXIT_AFTER_SHARED_SKILL_STAGE").is_some()
            {
                std::process::exit(86);
            }
            let actual = import_shared_skills(&quarantine).and_then(|skills| {
                skills
                    .into_iter()
                    .find(|skill| skill.name == expected.name)
                    .ok_or_else(|| anyhow::anyhow!("staged skill is no longer importable"))
            });
            if !matches!(actual, Ok(ref skill) if skill == &expected) {
                std::fs::rename(&staged, &source).with_context(|| {
                    format!(
                        "shared skill {} changed before cleanup; failed to restore it from {}",
                        expected.name,
                        quarantine.display()
                    )
                })?;
                let _ = std::fs::remove_file(quarantine.join(".committed-cleanup"));
                let _ = std::fs::remove_dir(&quarantine);
                anyhow::bail!(
                    "shared skill {} changed before cleanup and was left in place",
                    expected.name
                );
            }
            std::fs::remove_dir_all(&staged).with_context(|| {
                format!(
                    "failed to remove committed shared skill {}; recovery copy remains at {}",
                    expected.name,
                    staged.display()
                )
            })?;
        }
        std::fs::remove_file(quarantine.join(".committed-cleanup"))?;
        std::fs::remove_dir(&quarantine).with_context(|| {
            format!("failed to remove empty quarantine {}", quarantine.display())
        })?;
        Ok(())
    }
}

pub(crate) fn recover_shared_skill_cleanup(env: &AppEnvironment) -> Result<()> {
    let shared = env.user_home.join(".agents/skills");
    if !shared.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&shared)? {
        let entry = entry?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(".lazyagents-import-")
        {
            continue;
        }
        let kind = entry.file_type()?;
        let marker_path = entry.path().join(".committed-cleanup");
        let marker_kind = std::fs::symlink_metadata(&marker_path)
            .ok()
            .map(|metadata| metadata.file_type());
        if !kind.is_dir() || kind.is_symlink() || !marker_kind.is_some_and(|kind| kind.is_file()) {
            anyhow::bail!(
                "unrecognized shared-skill recovery data remains at {}; inspect it manually",
                entry.path().display()
            );
        }
        let marker: SharedSkillCleanupMarker = serde_json::from_str(
            &std::fs::read_to_string(&marker_path)
                .with_context(|| format!("failed to read {}", marker_path.display()))?,
        )
        .with_context(|| {
            format!(
                "invalid shared-skill cleanup marker {}",
                marker_path.display()
            )
        })?;
        validate_shared_skill_cleanup_marker(&marker).with_context(|| {
            format!(
                "invalid shared-skill cleanup marker {}",
                marker_path.display()
            )
        })?;
        for expected in marker.imported {
            let source = shared.join(&expected.name);
            let staged = entry.path().join(&expected.name);
            if source.exists() && staged.exists() {
                anyhow::bail!(
                    "shared skill {} was recreated while recovery data exists at {}; inspect both copies manually",
                    expected.name,
                    staged.display()
                );
            }
            if source.exists() && !staged.exists() {
                std::fs::rename(&source, &staged).with_context(|| {
                    format!(
                        "failed to resume cleanup for shared skill {}",
                        expected.name
                    )
                })?;
            }
            if staged.exists() {
                let actual = import_shared_skills(&entry.path())?
                    .into_iter()
                    .find(|skill| skill.name == expected.name)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "recovery copy for shared skill {} is invalid",
                            expected.name
                        )
                    })?;
                if actual != expected {
                    if !source.exists() {
                        let _ = std::fs::rename(&staged, &source);
                    }
                    anyhow::bail!(
                        "shared skill {} changed during recovery; it was not removed",
                        expected.name
                    );
                }
                std::fs::remove_dir_all(&staged)?;
            }
        }
        std::fs::remove_file(&marker_path)?;
        std::fs::remove_dir(entry.path())
            .context("failed to finish committed shared-skill cleanup")?;
    }
    sync_shared_directory(&shared)
}

fn validate_shared_skill_cleanup_marker(marker: &SharedSkillCleanupMarker) -> Result<()> {
    if marker.imported.is_empty() {
        anyhow::bail!("cleanup marker has no shared skills");
    }

    let mut names = BTreeSet::new();
    for expected in &marker.imported {
        let mut components = Path::new(&expected.name).components();
        let single_name =
            matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
        if !single_name || expected.name.starts_with('.') {
            anyhow::bail!(
                "shared skill name {} must be one visible directory name",
                expected.name
            );
        }
        if !names.insert(&expected.name) {
            anyhow::bail!(
                "cleanup marker contains shared skill {} twice",
                expected.name
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
fn sync_shared_directory(path: &std::path::Path) -> Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_shared_directory(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

pub(crate) fn with_shared_skill_rollback(
    error: anyhow::Error,
    _transaction: SharedSkillTransaction,
) -> anyhow::Error {
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_keeps_the_shared_source_in_place() {
        let temp = tempfile::tempdir().unwrap();
        let env = AppEnvironment {
            lazyagents_home: temp.path().join("lazyagents"),
            user_home: temp.path().join("user"),
            path_entries: Vec::new(),
        };
        let source = env.user_home.join(".agents/skills/shared");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("SKILL.md"), "shared").unwrap();
        let mut skills = Vec::new();
        let _transaction = SharedSkillTransaction::prepare(&mut skills, &env).unwrap();
        assert!(source.is_dir());
        assert_eq!(skills.len(), 1);
        assert!(!std::fs::read_dir(source.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(".lazyagents-import-")));
    }

    #[test]
    fn recovery_rejects_escaping_marker_names_before_moving_data() {
        let temp = tempfile::tempdir().unwrap();
        let env = AppEnvironment {
            lazyagents_home: temp.path().join("lazyagents"),
            user_home: temp.path().join("user"),
            path_entries: Vec::new(),
        };
        let shared = env.user_home.join(".agents/skills");
        let quarantine = shared.join(".lazyagents-import-crafted");
        let outside = env.user_home.join(".agents/victim");
        std::fs::create_dir_all(&quarantine).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("SKILL.md"), "keep me").unwrap();
        let marker = SharedSkillCleanupMarker {
            imported: vec![ImportedDirectory {
                name: "../victim".to_string(),
                unix_mode: None,
                files: Vec::new(),
                directories: Vec::new(),
            }],
        };
        std::fs::write(
            quarantine.join(".committed-cleanup"),
            serde_json::to_string(&marker).unwrap(),
        )
        .unwrap();

        let error = recover_shared_skill_cleanup(&env).unwrap_err();

        assert!(format!("{error:#}").contains("must be one visible directory name"));
        assert_eq!(
            std::fs::read_to_string(outside.join("SKILL.md")).unwrap(),
            "keep me"
        );
        assert!(!shared.join("victim").exists());
    }
}
