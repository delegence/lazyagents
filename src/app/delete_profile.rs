use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::app::harness_registry::HarnessRegistry;
use crate::app::state::LazyagentsState;
use crate::harness::integration::AppEnvironment;
use crate::profile::{ProfileName, ProfileStore};

pub fn delete_profile(
    registry: &dyn HarnessRegistry,
    env: &AppEnvironment,
    store: &ProfileStore,
    name: &str,
) -> Result<PathBuf> {
    let path = deletable_profile_path(registry, env, store, name)?;
    remove_stale_rollbacks(&path)?;
    std::fs::remove_dir_all(&path)
        .with_context(|| format!("failed to delete profile at {}", path.display()))?;
    Ok(path)
}

pub fn deletable_profile_path(
    registry: &dyn HarnessRegistry,
    env: &AppEnvironment,
    store: &ProfileStore,
    name: &str,
) -> Result<PathBuf> {
    let name = ProfileName::parse(name)?;
    let path = store.profile_dir(&name);
    if !path.exists() {
        anyhow::bail!("profile {name} does not exist at {}", path.display());
    }

    let active_reasons = profile_active_reasons(registry, env, &name, &path)?;
    if !active_reasons.is_empty() {
        anyhow::bail!(
            "cannot delete active profile {name}: {}",
            active_reasons.join("; ")
        );
    }
    Ok(path)
}

fn profile_active_reasons(
    registry: &dyn HarnessRegistry,
    env: &AppEnvironment,
    name: &ProfileName,
    profile_dir: &Path,
) -> Result<Vec<String>> {
    let mut reasons = Vec::new();
    let canonical_profile = std::fs::canonicalize(profile_dir)
        .with_context(|| format!("failed to resolve profile path {}", profile_dir.display()))?;
    let state = LazyagentsState::load(&env.lazyagents_home.join("state.json"))?;
    for (harness, profile) in state.active_profiles {
        if &profile == name {
            reasons.push(format!("state marks it active for {harness}"));
        }
    }

    for integration in registry.all(env)? {
        let paths = integration.paths(env)?;
        let mut linked = Vec::new();
        for surface in integration.managed_surfaces(&paths) {
            collect_profile_symlinks(&surface.path, &canonical_profile, &mut linked)?;
        }
        linked.sort();
        linked.dedup();
        if !linked.is_empty() {
            reasons.push(format!(
                "{} config links to it at {}",
                integration.display_name(),
                linked.join(", ")
            ));
        }
    }

    Ok(reasons)
}

fn collect_profile_symlinks(
    path: &Path,
    profile_dir: &Path,
    linked: &mut Vec<String>,
) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };

    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(path)
            .with_context(|| format!("failed to read symlink {}", path.display()))?;
        let target = if target.is_absolute() {
            target
        } else {
            path.parent().unwrap_or_else(|| Path::new(".")).join(target)
        };
        if path_resolution_traverses(&target, profile_dir)
            .with_context(|| format!("failed to resolve symlink {}", path.display()))?
        {
            linked.push(path.display().to_string());
        }
        return Ok(());
    }

    if metadata.is_dir() {
        for entry in
            std::fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))?
        {
            collect_profile_symlinks(&entry?.path(), profile_dir, linked)?;
        }
    }

    Ok(())
}

fn path_resolution_traverses(path: &Path, protected: &Path) -> Result<bool> {
    // The final target can be outside the profile but still need a profile
    // directory during path lookup. Check every component and nested link.
    path_resolution_traverses_inner(path, protected, &mut std::collections::BTreeSet::new())
}

fn path_resolution_traverses_inner(
    path: &Path,
    protected: &Path,
    active_links: &mut std::collections::BTreeSet<PathBuf>,
) -> Result<bool> {
    use std::path::Component;

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory")?
            .join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Prefix(_) | Component::RootDir => {
                resolved.push(component.as_os_str());
            }
            Component::Normal(name) => {
                let candidate = resolved.join(name);
                if candidate.starts_with(protected) {
                    return Ok(true);
                }
                match std::fs::symlink_metadata(&candidate) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        if !active_links.insert(candidate.clone()) {
                            anyhow::bail!("symlink cycle detected at {}", candidate.display());
                        }
                        let target = std::fs::read_link(&candidate).with_context(|| {
                            format!("failed to read symlink {}", candidate.display())
                        })?;
                        let target = if target.is_absolute() {
                            target
                        } else {
                            candidate
                                .parent()
                                .unwrap_or_else(|| Path::new("."))
                                .join(target)
                        };
                        if path_resolution_traverses_inner(&target, protected, active_links)? {
                            return Ok(true);
                        }
                        resolved = std::fs::canonicalize(&candidate).with_context(|| {
                            format!("failed to resolve path {}", candidate.display())
                        })?;
                        active_links.remove(&candidate);
                    }
                    Ok(_) => resolved = candidate,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        resolved = candidate;
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("failed to inspect path {}", candidate.display())
                        });
                    }
                }
                if resolved.starts_with(protected) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn remove_stale_rollbacks(profile: &Path) -> Result<()> {
    let Some(parent) = profile.parent() else {
        return Ok(());
    };
    let Some(name) = profile.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let prefix = format!(".{name}-rollback-");
    let mut rollbacks = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let kind = entry.file_type()?;
        if !kind.is_dir() || kind.is_symlink() {
            anyhow::bail!(
                "cannot delete profile while invalid rollback data exists at {}",
                entry.path().display()
            );
        }
        rollbacks.push(entry.path());
    }
    let marker = parent.join(format!(".{name}-transaction.json"));
    let marker_data = match std::fs::read_to_string(&marker) {
        Ok(text) => {
            let value: serde_json::Value = serde_json::from_str(&text).with_context(|| {
                format!("invalid profile transaction marker {}", marker.display())
            })?;
            let rollback = value.get("rollback").and_then(serde_json::Value::as_str);
            let phase = value.get("phase").and_then(serde_json::Value::as_str);
            if rollback.is_none() || !matches!(phase, Some("prepared" | "committed")) {
                anyhow::bail!("invalid profile transaction marker {}", marker.display());
            }
            rollback.map(str::to_string)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if !rollbacks.is_empty() {
        if rollbacks.len() != 1
            || marker_data.as_deref() != rollbacks[0].file_name().and_then(|value| value.to_str())
        {
            anyhow::bail!(
                "profile {name} has unmatched rollback data; manual recovery is required"
            );
        }
        std::fs::remove_dir_all(&rollbacks[0])
            .with_context(|| format!("failed to remove stale rollback for profile {name}"))?;
    }
    match std::fs::remove_file(&marker) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to remove profile transaction marker {}",
                    marker.display()
                )
            })
        }
    }
    sync_profiles_directory(parent)?;
    Ok(())
}

#[cfg(unix)]
fn sync_profiles_directory(path: &Path) -> Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_profiles_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn relative_parent_link_into_profile_is_detected_without_prefix_false_positive() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("lazyagents/profiles/work");
        let other = temp.path().join("lazyagents/profiles/work-copy");
        let managed = temp.path().join("config/skills");
        std::fs::create_dir_all(profile.join("skill")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::create_dir_all(&managed).unwrap();
        symlink(
            "../../lazyagents/profiles/work/skill",
            managed.join("linked"),
        )
        .unwrap();
        symlink(
            "../../lazyagents/profiles/work-copy",
            managed.join("outside"),
        )
        .unwrap();

        let mut linked = Vec::new();
        collect_profile_symlinks(
            &managed,
            &std::fs::canonicalize(&profile).unwrap(),
            &mut linked,
        )
        .unwrap();

        assert_eq!(linked.len(), 1);
        assert!(linked[0].ends_with("linked"));
    }

    #[test]
    fn dangling_unrelated_link_does_not_block_profile_deletion_scan() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        let managed = temp.path().join("managed");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::create_dir_all(&managed).unwrap();
        symlink("missing", managed.join("dangling")).unwrap();
        let mut linked = Vec::new();
        collect_profile_symlinks(
            &managed,
            &std::fs::canonicalize(&profile).unwrap(),
            &mut linked,
        )
        .unwrap();
        assert!(linked.is_empty());
    }

    #[test]
    fn dangling_link_with_missing_leaf_inside_profile_blocks_deletion() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profiles/work");
        let managed = temp.path().join("managed");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::create_dir_all(&managed).unwrap();
        symlink("../profiles/work/missing", managed.join("linked")).unwrap();

        let mut linked = Vec::new();
        collect_profile_symlinks(
            &managed,
            &std::fs::canonicalize(&profile).unwrap(),
            &mut linked,
        )
        .unwrap();

        assert_eq!(linked.len(), 1);
    }

    #[test]
    fn link_that_enters_and_leaves_profile_blocks_deletion() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profiles/work");
        let managed = temp.path().join("managed");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(profile.join("pass")).unwrap();
        std::fs::create_dir_all(&managed).unwrap();
        std::fs::write(&outside, "outside").unwrap();
        let linked = managed.join("linked");
        symlink("../profiles/work/pass/../../../outside", &linked).unwrap();
        assert_eq!(
            std::fs::canonicalize(&linked).unwrap(),
            std::fs::canonicalize(&outside).unwrap()
        );

        let mut links = Vec::new();
        collect_profile_symlinks(
            &managed,
            &std::fs::canonicalize(&profile).unwrap(),
            &mut links,
        )
        .unwrap();

        assert_eq!(links, vec![linked.display().to_string()]);
    }

    #[test]
    fn stale_rollbacks_are_removed_before_the_live_profile() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profiles/work");
        let rollback = temp.path().join("profiles/.work-rollback-old");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::create_dir_all(&rollback).unwrap();
        std::fs::write(rollback.join("PROFILE.md"), "old").unwrap();
        std::fs::write(
            temp.path().join("profiles/.work-transaction.json"),
            r#"{"rollback":".work-rollback-old","phase":"committed"}"#,
        )
        .unwrap();

        remove_stale_rollbacks(&profile).unwrap();

        assert!(profile.is_dir());
        assert!(!rollback.exists());
    }
}
