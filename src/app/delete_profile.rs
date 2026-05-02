use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::app::harness_registry::HarnessRegistry;
use crate::app::state::LazyagentsState;
use crate::harness::integration::AppEnvironment;
use crate::profile::ProfileStore;

pub fn delete_profile(
    registry: &dyn HarnessRegistry,
    env: &AppEnvironment,
    store: &ProfileStore,
    name: &str,
) -> Result<PathBuf> {
    let path = store.profile_dir_for_raw_name(name)?;
    if !path.exists() {
        anyhow::bail!("profile {name} does not exist at {}", path.display());
    }

    let active_reasons = profile_active_reasons(registry, env, store, name, &path)?;
    if !active_reasons.is_empty() {
        anyhow::bail!(
            "cannot delete active profile {name}: {}",
            active_reasons.join("; ")
        );
    }

    std::fs::remove_dir_all(&path)
        .with_context(|| format!("failed to delete profile at {}", path.display()))?;
    Ok(path)
}

fn profile_active_reasons(
    registry: &dyn HarnessRegistry,
    env: &AppEnvironment,
    store: &ProfileStore,
    name: &str,
    profile_dir: &Path,
) -> Result<Vec<String>> {
    let mut reasons = Vec::new();
    let state = LazyagentsState::load(&env.lazyagents_home.join("state.json"))?;
    for (harness, profile) in state.active_profiles {
        if profile.as_str() == name {
            reasons.push(format!("state marks it active for {harness}"));
        }
    }

    for integration in registry.all(env)? {
        let paths = integration.paths(env)?;
        let mut linked = Vec::new();
        for surface in integration.managed_surfaces(&paths) {
            collect_profile_symlinks(&surface.path, profile_dir, &mut linked)?;
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

    let _ = store;
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
        let absolute_target = if target.is_absolute() {
            target
        } else {
            path.parent().unwrap_or_else(|| Path::new("/")).join(target)
        };
        if absolute_target.starts_with(profile_dir) {
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
