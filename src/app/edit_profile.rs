use std::path::PathBuf;

use anyhow::Result;

use crate::profile::{ProfileName, ProfileStore};

pub fn edit_profile_path(store: &ProfileStore, name: &str) -> Result<PathBuf> {
    let name = ProfileName::parse(name)?;
    let path = store.profile_dir(&name);
    if !path.is_dir() {
        anyhow::bail!("profile {name} does not exist at {}", path.display());
    }
    Ok(path)
}
