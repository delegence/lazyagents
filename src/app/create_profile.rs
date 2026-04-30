use std::path::PathBuf;

use anyhow::Result;

use crate::app::harness_registry::HarnessRegistry;
use crate::harness::integration::{AppEnvironment, HarnessDetection};
use crate::harness::kind::HarnessKind;
use crate::profile::{ProfileName, ProfileStore};

pub enum CreateProfileResult {
    Created {
        profile: ProfileName,
        path: PathBuf,
    },
    Imported {
        profile: ProfileName,
        harness: HarnessKind,
        path: PathBuf,
    },
}

pub fn create_profile(
    registry: &dyn HarnessRegistry,
    env: &AppEnvironment,
    store: &ProfileStore,
    profile: ProfileName,
    from: Option<HarnessKind>,
) -> Result<CreateProfileResult> {
    match from {
        Some(kind) => {
            let integration = registry
                .get(kind)
                .ok_or_else(|| anyhow::anyhow!("unsupported harness {kind}"))?;
            match integration.detect(env)? {
                HarnessDetection::Detected { .. } => {}
                HarnessDetection::NotDetected => anyhow::bail!("{kind} was not detected on PATH"),
            }
            let paths = integration.paths(env)?;
            let path = store.create_skeleton(&profile)?;
            if let Err(error) = integration
                .import_from_harness(&paths)
                .and_then(|imported| store.apply_import(&profile, kind, imported))
            {
                let _ = std::fs::remove_dir_all(&path);
                return Err(error.context(format!("failed to import from {kind}")));
            }
            Ok(CreateProfileResult::Imported {
                profile,
                harness: kind,
                path,
            })
        }
        None => {
            let path = store.create_skeleton(&profile)?;
            Ok(CreateProfileResult::Created { profile, path })
        }
    }
}
