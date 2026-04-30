use anyhow::{Context, Result};

use crate::profile::inspect::ProfileSummary;
use crate::profile::{ProfileName, ProfileStore};

pub fn inspect_profile(store: &ProfileStore, profile: &ProfileName) -> Result<ProfileSummary> {
    store
        .summarize(profile)
        .with_context(|| format!("failed to inspect profile {profile}"))
}
