use crate::profile::name::ProfileName;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProfileConfig {
    pub name: Option<String>,
    pub description: Option<String>,
    pub models: BTreeMap<String, Value>,
    pub permissions: BTreeMap<String, Value>,
}

impl ProfileConfig {
    pub fn default_for(name: &ProfileName) -> Self {
        Self {
            name: Some(name.as_str().to_string()),
            description: Some(String::new()),
            models: BTreeMap::new(),
            permissions: BTreeMap::new(),
        }
    }

    pub fn model_preference(&self, harness_kind: crate::harness::kind::HarnessKind) -> Value {
        self.models
            .get(harness_kind.id())
            .cloned()
            .unwrap_or_else(default_preference_value)
    }

    pub fn permission_preference(&self, harness_kind: crate::harness::kind::HarnessKind) -> Value {
        self.permissions
            .get(harness_kind.id())
            .cloned()
            .unwrap_or_else(default_preference_value)
    }
}

pub fn default_preference_value() -> Value {
    json!("default")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileConfigStatus {
    Valid,
    Missing,
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_config_missing_preferences_resolve_to_default() {
        let config: ProfileConfig = serde_json::from_str(
            r#"{"models":{"codex":"gpt-5"},"permissions":{"codex":"on-request"}}"#,
        )
        .unwrap();

        assert_eq!(
            config.model_preference(crate::harness::kind::HarnessKind::Codex),
            "gpt-5"
        );
        assert_eq!(
            config.model_preference(crate::harness::kind::HarnessKind::OpenCode),
            "default"
        );
        assert_eq!(
            config.permission_preference(crate::harness::kind::HarnessKind::OpenCode),
            "default"
        );
    }

    #[test]
    fn profile_config_preserves_opencode_object_permission_preference() {
        let config: ProfileConfig = serde_json::from_str(
            r#"{"models":{},"permissions":{"opencode":{"*":"ask","bash":"allow"}}}"#,
        )
        .unwrap();

        assert_eq!(
            config.permission_preference(crate::harness::kind::HarnessKind::OpenCode),
            serde_json::json!({"*":"ask","bash":"allow"})
        );
    }
}
