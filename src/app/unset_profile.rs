use std::collections::BTreeSet;

use anyhow::Result;

use crate::app::harness_registry::HarnessRegistry;
use crate::app::state::LazyagentsState;
use crate::harness::integration::{AppEnvironment, HarnessIntegration};
use crate::profile::ProfileName;

pub enum UnsetProfileTarget {
    Harness(String),
    All,
}

pub struct UnsetProfileResult {
    pub harness: String,
    pub display_name: String,
    pub profile: ProfileName,
    pub alias_updates: Vec<String>,
}

pub fn unset_profile_workflow(
    registry: &dyn HarnessRegistry,
    env: &AppEnvironment,
    target: UnsetProfileTarget,
) -> Result<Vec<UnsetProfileResult>> {
    let state_path = env.lazyagents_home.join("state.json");
    let mut state = LazyagentsState::load(&state_path)?;
    let mut results = match target {
        UnsetProfileTarget::Harness(id) => {
            let integration = registry
                .get(env, &id)?
                .ok_or_else(|| anyhow::anyhow!("unsupported harness {id}"))?;
            unset_group(registry, env, integration.as_ref(), &mut state)?
                .into_iter()
                .collect()
        }
        UnsetProfileTarget::All => unset_all(registry, env, &mut state)?,
    };

    if !results.is_empty() {
        state.save(&state_path)?;
    }
    results.sort_by(|left, right| left.harness.cmp(&right.harness));
    Ok(results)
}

fn unset_all(
    registry: &dyn HarnessRegistry,
    env: &AppEnvironment,
    state: &mut LazyagentsState,
) -> Result<Vec<UnsetProfileResult>> {
    let integrations = registry.all(env)?;
    let mut handled = BTreeSet::new();
    let mut results = Vec::new();

    for integration in &integrations {
        if handled.contains(integration.instance_id()) {
            continue;
        }
        let aliases = registry.aliases_for(env, integration.as_ref())?;
        handled.extend(aliases);
        if let Some(result) = unset_group(registry, env, integration.as_ref(), state)? {
            results.push(result);
        }
    }

    // `--all` also removes stale state entries for harnesses no longer in settings.json.
    for harness in state.active_profiles.keys().cloned().collect::<Vec<_>>() {
        let profile = state.active_profiles.remove(&harness).unwrap();
        results.push(UnsetProfileResult {
            display_name: harness.clone(),
            harness,
            profile,
            alias_updates: Vec::new(),
        });
    }

    Ok(results)
}

fn unset_group(
    registry: &dyn HarnessRegistry,
    env: &AppEnvironment,
    integration: &dyn HarnessIntegration,
    state: &mut LazyagentsState,
) -> Result<Option<UnsetProfileResult>> {
    let aliases = registry.aliases_for(env, integration)?;
    let profile = aliases
        .iter()
        .find_map(|alias| state.active_profiles.get(alias))
        .cloned();
    let Some(profile) = profile else {
        return Ok(None);
    };

    let mut alias_updates = Vec::new();
    for alias in aliases {
        if state.active_profiles.remove(&alias).is_some() && alias != integration.instance_id() {
            alias_updates.push(alias);
        }
    }

    Ok(Some(UnsetProfileResult {
        harness: integration.instance_id().to_string(),
        display_name: integration.display_name().to_string(),
        profile,
        alias_updates,
    }))
}
