use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::{save_permissions, AgentConfig};
use crate::harness::Harness;

pub const REMEMBER: &str = "lazyagents:remember";

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Ask,
    FullAccess,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Ask => "ask for approval",
            Self::FullAccess => "full access",
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Approval {
    pub harness: String,
    pub label: String,
    pub scope: Scope,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Scope {
    Tool { name: String },
    Request { tool_call: Value },
}

pub struct Store {
    root: PathBuf,
    harness: &'static dyn Harness,
}

impl Store {
    pub fn new(root: &Path, harness: &'static dyn Harness) -> Self {
        Self {
            root: root.to_owned(),
            harness,
        }
    }

    pub fn mode(&self) -> Result<Mode> {
        Ok(AgentConfig::load(&self.root)?.permission_mode)
    }

    pub fn candidate(&self, tool_call: &Value) -> Option<Approval> {
        if self.harness.approves_tool_names() {
            let name = tool_call.pointer("/_meta/toolName")?.as_str()?;
            if name.is_empty() {
                return None;
            }
            return Some(Approval {
                harness: self.harness.id().to_owned(),
                label: name.to_owned(),
                scope: Scope::Tool {
                    name: name.to_owned(),
                },
            });
        }
        // Match complete request data. A display title alone does not identify an action.
        let has_input = tool_call.get("rawInput").is_some_and(|input| {
            !input.is_null()
                && !input.as_object().is_some_and(serde_json::Map::is_empty)
                && !input.as_array().is_some_and(Vec::is_empty)
                && input.as_str() != Some("")
        });
        let has_edit_paths = tool_call["kind"] == "edit"
            && tool_call["locations"].as_array().is_some_and(|locations| {
                !locations.is_empty()
                    && locations.iter().all(|location| {
                        location["path"]
                            .as_str()
                            .is_some_and(|path| !path.is_empty())
                    })
            });
        if !has_input && !has_edit_paths {
            return None;
        }
        let mut request = tool_call.as_object()?.clone();
        request.remove("toolCallId");
        request.remove("status");
        request.remove("rawOutput");
        let label = tool_call
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("tool request");
        Some(Approval {
            harness: self.harness.id().to_owned(),
            label: label.to_owned(),
            scope: Scope::Request {
                tool_call: Value::Object(request),
            },
        })
    }

    pub fn contains(&self, candidate: &Approval) -> Result<bool> {
        Ok(AgentConfig::load(&self.root)?
            .permissions
            .iter()
            .any(|rule| rule.harness == candidate.harness && rule.scope == candidate.scope))
    }

    pub fn remember(&self, candidate: Approval) -> Result<()> {
        let mut config = AgentConfig::load(&self.root)?;
        if !config
            .permissions
            .iter()
            .any(|rule| rule.harness == candidate.harness && rule.scope == candidate.scope)
        {
            config.permissions.push(candidate);
            save_permissions(&self.root, &config.permissions)?;
        }
        Ok(())
    }

    pub fn options(&self, options: &[Value], candidate: &Approval) -> Vec<Value> {
        let mut choices = options
            .iter()
            .filter_map(|option| {
                let mut option = option.clone();
                if option["kind"] == "allow_always" {
                    let session = self.harness.always_allow_is_session_only()
                        || option["optionId"]
                            .as_str()
                            .is_some_and(|id| id.to_lowercase().contains("session"))
                        || option["name"]
                            .as_str()
                            .is_some_and(|name| name.to_lowercase().contains("session"));
                    if !session {
                        return None;
                    }
                    option["name"] = "allow for this session".into();
                }
                Some(option)
            })
            .collect::<Vec<_>>();
        let label = match candidate.scope {
            Scope::Tool { .. } => "always allow",
            Scope::Request { .. } => "always allow this exact request",
        };
        choices.push(json!({"optionId": REMEMBER, "kind": "allow_always", "name": label}));
        choices
    }
}

// Saved approvals use a one-time response, so revoking a saved rule takes effect
// on the next request instead of leaving an extra approval cached in the harness.
pub fn allow_once(options: &[Value]) -> Option<&str> {
    let mut allowed = options
        .iter()
        .filter(|option| option["kind"] == "allow_once");
    let option = allowed.next()?;
    if allowed.next().is_some() {
        return None;
    }
    option["optionId"].as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::get;

    #[test]
    fn saved_rules_survive_reload_and_stay_with_their_agent_and_harness() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".agents")).unwrap();
        std::fs::write(
            root.path().join(".agents/agent.json"),
            json!({
                "name": "test", "slug": "test", "description": "test", "harness": "pi",
                "model": "custom", "thinking": "high", "custom_field": true
            })
            .to_string(),
        )
        .unwrap();
        let store = Store::new(root.path(), get("pi").unwrap());
        let call = json!({"title": "bash", "_meta": {"toolName": "bash"}});
        let rule = store.candidate(&call).unwrap();
        store.remember(rule.clone()).unwrap();
        store.remember(rule.clone()).unwrap();
        let reloaded = Store::new(root.path(), get("pi").unwrap());
        assert!(reloaded.contains(&rule).unwrap());
        let config = AgentConfig::load(root.path()).unwrap();
        assert_eq!(config.permissions.len(), 1);
        assert_eq!(config.model.as_deref(), Some("custom"));
        let raw: Value =
            serde_json::from_slice(&std::fs::read(root.path().join(".agents/agent.json")).unwrap())
                .unwrap();
        assert_eq!(raw["custom_field"], true);
        let mut other = rule.clone();
        other.harness = "codex".into();
        assert!(!reloaded.contains(&other).unwrap());
        save_permissions(root.path(), &[]).unwrap();
        assert!(!reloaded.contains(&rule).unwrap());
    }

    #[test]
    fn exact_requests_do_not_match_changed_commands_paths_or_permission_data() {
        for harness in ["codex", "claude", "opencode"] {
            let store = Store::new(Path::new("unused"), get(harness).unwrap());
            let call = json!({"toolCallId": "1", "title": "bash", "kind": "execute",
                "rawInput": {"command": "git status", "cwd": "/one"},
                "content": [{"type": "text", "text": "network blocked"}]
            });
            let original = store.candidate(&call).unwrap();
            let mut next = call.clone();
            next["toolCallId"] = "2".into();
            next["status"] = "pending".into();
            assert_eq!(original.scope, store.candidate(&next).unwrap().scope);
            for (field, value) in [("command", "git push"), ("cwd", "/two")] {
                let mut changed = next.clone();
                changed["rawInput"][field] = value.into();
                assert_ne!(original.scope, store.candidate(&changed).unwrap().scope);
            }
            next["content"][0]["text"] = "network allowed".into();
            assert_ne!(original.scope, store.candidate(&next).unwrap().scope);
            assert!(store.candidate(&json!({"title": "bash"})).is_none());
        }
    }

    #[test]
    fn session_and_permanent_choices_are_separate() {
        let store = Store::new(Path::new("unused"), get("pi").unwrap());
        let candidate = store
            .candidate(&json!({"_meta": {"toolName": "bash"}}))
            .unwrap();
        let options = vec![
            json!({"optionId": "once", "kind": "allow_once", "name": "Allow once"}),
            json!({"optionId": "allow_always", "kind": "allow_always", "name": "Always allow bash"}),
        ];
        let choices = store.options(&options, &candidate);
        assert_eq!(choices[1]["name"], "allow for this session");
        assert_eq!(choices[2]["optionId"], REMEMBER);
        assert_eq!(allow_once(&options), Some("once"));
        assert_eq!(allow_once(&[options[0].clone(), options[0].clone()]), None);
        assert_eq!(allow_once(&[options[1].clone()]), None);
    }

    #[test]
    fn file_approvals_match_the_affected_paths() {
        let store = Store::new(Path::new("unused"), get("codex").unwrap());
        let mut call = json!({"toolCallId": "1", "title": "Edit file", "kind": "edit",
            "locations": [{"path": "/agent/workspace/notes.md"}]});
        let original = store.candidate(&call).unwrap();
        call["locations"][0]["path"] = "/agent/workspace/other.md".into();
        assert_ne!(original.scope, store.candidate(&call).unwrap().scope);
        call["locations"] = json!([]);
        assert!(store.candidate(&call).is_none());
    }
}
