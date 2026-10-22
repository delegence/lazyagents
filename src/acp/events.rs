use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::requests::PermissionOption;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum StreamEvent {
    Assistant(String),
    Reasoning(String),
    Tool {
        call_id: String,
        title: String,
        kind: Option<String>,
        status: Option<ToolStatus>,
        input: Option<Value>,
        output: Option<Value>,
    },
    PermissionRequest {
        title: String,
        #[serde(default)]
        tool_call: Option<Value>,
        options: Vec<PermissionOption>,
        #[serde(default)]
        cancelled: bool,
    },
    PermissionDecision {
        title: String,
        allowed: bool,
    },
    SystemError(String),
}

/// Converts session updates into events for one request.
pub(super) struct Updates<F> {
    on_event: F,
    citations: CitationFilter,
    system_error: bool,
}

impl<F> Updates<F>
where
    F: FnMut(StreamEvent) -> Result<Option<String>>,
{
    pub(super) fn new(on_event: F) -> Self {
        Self {
            on_event,
            citations: CitationFilter::default(),
            system_error: false,
        }
    }

    /// Sends an event. Only a permission request returns a selected option.
    pub(super) fn emit(&mut self, event: StreamEvent) -> Result<Option<String>> {
        (self.on_event)(event)
    }

    pub(super) fn update(&mut self, update: &Value) -> Result<()> {
        let event = match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("agent_message_chunk") => {
                let Some(text) = message_text(update) else {
                    return Ok(());
                };
                if self.system_error || text.trim_start().starts_with("[SYSTEM_ERROR]") {
                    self.system_error = true;
                    StreamEvent::SystemError(text.to_owned())
                } else {
                    let text = self.citations.push(text);
                    if text.is_empty() {
                        return Ok(());
                    }
                    StreamEvent::Assistant(text)
                }
            }
            Some("agent_thought_chunk") => match message_text(update) {
                Some(text) => StreamEvent::Reasoning(text.to_owned()),
                None => return Ok(()),
            },
            Some("tool_call" | "tool_call_update") => {
                let Some(call_id) = update
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                else {
                    return Ok(());
                };
                StreamEvent::Tool {
                    call_id: call_id.to_owned(),
                    title: update
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("Tool call")
                        .to_owned(),
                    kind: update
                        .get("kind")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    status: update
                        .get("status")
                        .and_then(|status| ToolStatus::deserialize(status).ok()),
                    input: update.get("rawInput").cloned(),
                    output: update
                        .get("rawOutput")
                        .filter(|value| {
                            !value.is_null()
                                && !value.as_object().is_some_and(serde_json::Map::is_empty)
                                && !value.as_array().is_some_and(Vec::is_empty)
                                && value.as_str() != Some("")
                        })
                        .cloned()
                        .or_else(|| update.get("content").cloned())
                        .or_else(|| update.get("locations").cloned()),
                }
            }
            _ => return Ok(()),
        };
        self.emit(event).map(drop)
    }

    /// Sends text held back for a possible citation, unless the turn failed.
    pub(super) fn finish(&mut self) -> Result<()> {
        let text = std::mem::take(&mut self.citations).finish();
        if self.system_error {
            bail!("the harness reported a system error");
        }
        if !text.is_empty() {
            self.emit(StreamEvent::Assistant(text))?;
        }
        Ok(())
    }
}

fn message_text(update: &Value) -> Option<&str> {
    update
        .get("content")
        .and_then(|content| {
            content
                .get("text")
                .and_then(Value::as_str)
                .or_else(|| content.as_str())
        })
        .or_else(|| update.get("text").and_then(Value::as_str))
}

const CITATION_START: &str = "\u{e200}cite\u{e202}";
const CITATION_END: char = '\u{e201}';

#[derive(Default)]
struct CitationFilter {
    pending: String,
}

impl CitationFilter {
    fn push(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        let input = std::mem::take(&mut self.pending);
        let mut remaining = input.as_str();
        let mut output = String::new();

        loop {
            if let Some(start) = remaining.find(CITATION_START) {
                output.push_str(&remaining[..start]);
                let marker = &remaining[start + CITATION_START.len()..];
                if let Some(end) = marker.find(CITATION_END) {
                    remaining = &marker[end + CITATION_END.len_utf8()..];
                    continue;
                }
                self.pending.push_str(&remaining[start..]);
                break;
            }

            let partial_start = CITATION_START
                .char_indices()
                .map(|(index, _)| index)
                .filter(|index| *index > 0 && remaining.ends_with(&CITATION_START[..*index]))
                .max()
                .unwrap_or(0);
            let complete = remaining.len() - partial_start;
            output.push_str(&remaining[..complete]);
            self.pending.push_str(&remaining[complete..]);
            break;
        }

        output
    }

    fn finish(self) -> String {
        self.pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_raw_output_does_not_hide_tool_errors() {
        let content = json!([{"type": "content", "content": {
            "type": "text", "text": "mise: configuration is not trusted"
        }}]);
        for raw_output in [json!({}), json!([]), Value::Null, json!("")] {
            let update = json!({
                "sessionUpdate": "tool_call_update", "toolCallId": "bash-1",
                "status": "failed", "rawOutput": raw_output, "content": content
            });
            let mut events = Vec::new();
            Updates::new(|event| {
                events.push(event);
                Ok(None)
            })
            .update(&update)
            .unwrap();
            assert!(
                matches!(&events[0], StreamEvent::Tool { output: Some(output), .. } if output == &content)
            );
        }
    }

    #[test]
    fn removes_embedded_citation_markers() {
        let mut filter = CitationFilter::default();
        assert_eq!(
            filter.push("Weather. \u{e200}cite\u{e202}turn2search1\u{e201}"),
            "Weather. "
        );
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn removes_citation_markers_split_across_chunks() {
        let mut filter = CitationFilter::default();
        assert_eq!(filter.push("Weather. \u{e200}ci"), "Weather. ");
        assert_eq!(filter.push("te\u{e202}turn2"), "");
        assert_eq!(filter.push("search1\u{e201} Next."), " Next.");
        assert_eq!(filter.finish(), "");
    }
}
