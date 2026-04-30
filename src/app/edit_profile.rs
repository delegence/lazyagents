use std::path::PathBuf;

use anyhow::Result;

use crate::profile::ProfileStore;

pub fn edit_profile_path(store: &ProfileStore, name: &str) -> Result<PathBuf> {
    store.get_path(name)
}
