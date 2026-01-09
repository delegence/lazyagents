use std::env;
use std::path::PathBuf;

use crate::error::{Error, Result};

const APP_DIR: &str = "mews";
const CONFIG_FILE: &str = "config.jsonc";
const SKILLS_DIR: &str = "skills";
const COMMANDS_DIR: &str = "commands";
const RULES_DIR: &str = "rules";
const SETTINGS_DIR: &str = "settings";
const BACKUPS_DIR: &str = "backups";

pub fn base_config_dir() -> Result<PathBuf> {
    if let Some(dir) = env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(dir).join(APP_DIR));
    }
    let home = dirs::home_dir().ok_or(Error::MissingHomeDir)?;
    Ok(home.join(".config").join(APP_DIR))
}

pub fn config_file_path() -> Result<PathBuf> {
    Ok(base_config_dir()?.join(CONFIG_FILE))
}

pub fn skills_dir() -> Result<PathBuf> {
    Ok(base_config_dir()?.join(SKILLS_DIR))
}

pub fn commands_dir() -> Result<PathBuf> {
    Ok(base_config_dir()?.join(COMMANDS_DIR))
}

pub fn rules_dir() -> Result<PathBuf> {
    Ok(base_config_dir()?.join(RULES_DIR))
}

pub fn settings_dir() -> Result<PathBuf> {
    Ok(base_config_dir()?.join(SETTINGS_DIR))
}

pub fn backups_dir() -> Result<PathBuf> {
    Ok(base_config_dir()?.join(BACKUPS_DIR))
}
