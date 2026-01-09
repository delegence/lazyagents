use std::fs;
use std::path::PathBuf;

use super::paths;
use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct CacheDirs {
    pub skills_dir: PathBuf,
    pub commands_dir: PathBuf,
    pub rules_dir: PathBuf,
    pub settings_dir: PathBuf,
}

pub fn ensure_cache_dirs() -> Result<CacheDirs> {
    let base_dir = paths::base_config_dir()?;
    fs::create_dir_all(&base_dir).map_err(|err| Error::io(&base_dir, err))?;

    let skills_dir = paths::skills_dir()?;
    fs::create_dir_all(&skills_dir).map_err(|err| Error::io(&skills_dir, err))?;

    let commands_dir = paths::commands_dir()?;
    fs::create_dir_all(&commands_dir).map_err(|err| Error::io(&commands_dir, err))?;

    let rules_dir = paths::rules_dir()?;
    fs::create_dir_all(&rules_dir).map_err(|err| Error::io(&rules_dir, err))?;

    let settings_dir = paths::settings_dir()?;
    fs::create_dir_all(&settings_dir).map_err(|err| Error::io(&settings_dir, err))?;

    Ok(CacheDirs {
        skills_dir,
        commands_dir,
        rules_dir,
        settings_dir,
    })
}

pub fn rules_profile_dir(profile_id: &str) -> Result<PathBuf> {
    Ok(paths::rules_dir()?.join(profile_id))
}

pub fn settings_profile_dir(profile_id: &str) -> Result<PathBuf> {
    Ok(paths::settings_dir()?.join(profile_id))
}
