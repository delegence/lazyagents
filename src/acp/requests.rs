use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{Client, StreamEvent, Updates};
use crate::harness::Harness;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PermissionOption {
    pub id: String,
    pub kind: String,
    pub label: String,
}

impl Client {
    pub fn remember_permissions(&mut self, root: &Path, harness: &'static dyn Harness) {
        self.permissions = Some(crate::permissions::Store::new(root, harness));
    }

    pub(super) async fn handle_request<F>(
        &mut self,
        request: &Value,
        updates: &mut Updates<F>,
        cancelled: bool,
    ) -> Result<()>
    where
        F: FnMut(StreamEvent) -> Result<Option<String>>,
    {
        let id = request
            .get("id")
            .cloned()
            .context("ACP request has no id")?;
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        if method != "session/request_permission" {
            return self
                .write(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": format!("Unsupported ACP client method: {method}")}
                }))
                .await;
        }

        let title = request
            .pointer("/params/toolCall/title")
            .and_then(Value::as_str)
            .unwrap_or("The agent wants to use a tool");
        let options = request
            .pointer("/params/options")
            .and_then(Value::as_array)
            .context("ACP permission request has no options")?;
        let once = crate::permissions::allow_once(options);
        let full_access = !cancelled
            && self
                .permissions
                .as_ref()
                .map(|store| store.mode())
                .transpose()?
                == Some(crate::permissions::Mode::FullAccess);
        let candidate = if cancelled || once.is_none() {
            None
        } else {
            self.permissions.as_ref().and_then(|store| {
                request
                    .pointer("/params/toolCall")
                    .and_then(|call| store.candidate(call))
            })
        };
        let remembered = match (&self.permissions, &candidate) {
            (Some(store), Some(candidate)) => store.contains(candidate)?,
            _ => false,
        };
        let choices = match (&self.permissions, &candidate) {
            (Some(store), Some(candidate)) => store.options(options, candidate),
            _ => options.clone(),
        };
        let automatic = if full_access {
            options
                .iter()
                .find(|option| option["kind"] == "allow_once")
                .and_then(|option| option["optionId"].as_str())
                .map(str::to_owned)
        } else if remembered {
            once.map(str::to_owned)
        } else {
            None
        };
        let selected = if automatic.is_some() {
            automatic
        } else {
            updates.emit(StreamEvent::PermissionRequest {
                title: title.to_owned(),
                tool_call: request.pointer("/params/toolCall").cloned(),
                options: choices.iter().filter_map(permission_option).collect(),
                cancelled,
            })?
        };
        let mut selected = selected.filter(|_| !cancelled);
        if selected.as_deref() == Some(crate::permissions::REMEMBER) {
            selected = match (&self.permissions, candidate, once) {
                (Some(store), Some(candidate), Some(once)) => {
                    store.remember(candidate)?;
                    Some(once.to_owned())
                }
                _ => None,
            };
        }
        let allowed = selected.as_deref().is_some_and(|selected| {
            options.iter().any(|option| {
                option.get("optionId").and_then(Value::as_str) == Some(selected)
                    && matches!(
                        option.get("kind").and_then(Value::as_str),
                        Some("allow_once" | "allow_always")
                    )
            })
        });
        updates.emit(StreamEvent::PermissionDecision {
            title: title.to_owned(),
            allowed,
        })?;
        let option_id = selected.filter(|selected| {
            options.iter().any(|option| {
                option.get("optionId").and_then(Value::as_str) == Some(selected.as_str())
            })
        });
        let outcome = option_id.map_or_else(
            || json!({"outcome": "cancelled"}),
            |option_id| json!({"outcome": "selected", "optionId": option_id}),
        );
        self.write(&json!({"jsonrpc": "2.0", "id": id, "result": {"outcome": outcome}}))
            .await
    }
}

fn permission_option(value: &Value) -> Option<PermissionOption> {
    let kind = value.get("kind")?.as_str()?.to_owned();
    Some(PermissionOption {
        id: value.get("optionId")?.as_str()?.to_owned(),
        label: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&kind)
            .to_owned(),
        kind,
    })
}
