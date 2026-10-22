use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::acp::{StreamEvent, ToolStatus};
use crate::config::AgentConfig;

pub struct SessionLog {
    writer: BufWriter<File>,
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct SessionMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    #[serde(other)]
    Other,
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEntry {
    Message(SessionMessage),
    Stream {
        event: StreamEvent,
    },
    TurnEnd,
    Error {
        message: String,
        #[serde(default)]
        detail: String,
        #[serde(default)]
        diagnostics: Vec<String>,
    },
}

#[derive(Debug)]
pub struct SessionSummary {
    pub id: String,
    pub harness: String,
    pub last_message: String,
    pub last_message_at: Option<DateTime<Utc>>,
}

impl SessionLog {
    pub fn list(directory: &Path) -> Result<Vec<SessionSummary>> {
        let mut sessions = HashMap::<String, SessionSummary>::new();
        for path in log_paths(directory)?.into_iter().rev() {
            let result = (|| -> Result<()> {
                let mut records = Records::open(&path)?;
                let Some(header) = records.next_value()? else {
                    return Ok(());
                };
                let id = header
                    .get("acp_session_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .context("session header has no ACP ID")?;
                if sessions.contains_key(id) {
                    return Ok(());
                }
                let mut assistant_open = false;
                while let Some(value) = records.next_value()? {
                    let kind = value.get("type").and_then(Value::as_str);
                    let assistant = kind == Some("stream")
                        && value.pointer("/event/event").and_then(Value::as_str)
                            == Some("assistant");
                    let content = if assistant {
                        value.pointer("/event/data").and_then(Value::as_str)
                    } else if kind == Some("message") {
                        value.get("content").and_then(Value::as_str)
                    } else {
                        None
                    };
                    if let Some(content) = content.filter(|content| !content.is_empty()) {
                        let session =
                            sessions
                                .entry(id.to_owned())
                                .or_insert_with(|| SessionSummary {
                                    id: id.to_owned(),
                                    harness: header
                                        .get("harness")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_owned(),
                                    last_message: String::new(),
                                    last_message_at: None,
                                });
                        if !assistant || !assistant_open {
                            session.last_message.clear();
                        }
                        // The picker needs only a short preview, even for very large messages.
                        let remaining =
                            240usize.saturating_sub(session.last_message.chars().count());
                        session.last_message.extend(content.chars().take(remaining));
                        session.last_message_at = value
                            .get("created_at")
                            .and_then(Value::as_str)
                            .and_then(|date| date.parse().ok());
                    }
                    assistant_open = assistant;
                }
                Ok(())
            })();
            if let Err(error) = result {
                eprintln!(
                    "warning: could not read session {}: {error:#}",
                    path.display()
                );
            }
        }
        let mut sessions: Vec<_> = sessions.into_values().collect();
        sessions.sort_by(|left, right| {
            right
                .last_message_at
                .cmp(&left.last_message_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(sessions)
    }

    pub fn prompt_history(directory: &Path) -> Result<Vec<String>> {
        let mut history = VecDeque::new();
        // UUID v7 file names sort by creation time. Read recent files first.
        for path in log_paths(directory)?.into_iter().rev() {
            let mut recent = VecDeque::new();
            let Ok(mut records) = Records::open(&path) else {
                continue;
            };
            while let Ok(Some(value)) = records.next_value() {
                if value.get("type").and_then(Value::as_str) != Some("message")
                    || value.get("role").and_then(Value::as_str) != Some("user")
                {
                    continue;
                }
                if let Some(content) = value.get("content").and_then(Value::as_str) {
                    if recent.back().is_some_and(|last| last == content) {
                        continue;
                    }
                    if recent.len() == 1000 {
                        recent.pop_front();
                    }
                    recent.push_back(content.to_owned());
                }
            }
            for prompt in recent.into_iter().rev() {
                if history.front() != Some(&prompt) {
                    history.push_front(prompt);
                }
                if history.len() == 1000 {
                    return Ok(history.into_iter().collect());
                }
            }
        }
        Ok(history.into_iter().collect())
    }

    pub fn messages(directory: &Path, acp_session_id: &str) -> Result<Vec<SessionEntry>> {
        let mut entries = Vec::new();
        for path in log_paths(directory)? {
            let mut records = Records::open(&path)?;
            let Some(header) = records.next_value()? else {
                continue;
            };
            if header.get("acp_session_id").and_then(Value::as_str) != Some(acp_session_id) {
                continue;
            }
            while let Some(value) = records.next_value()? {
                if let Some(entry) = decode_entry(value).with_context(|| {
                    format!(
                        "could not read {} at line {}",
                        path.display(),
                        records.line_number
                    )
                })? {
                    entries.push(entry);
                }
            }
        }
        Ok(entries)
    }

    pub fn create(directory: &Path, config: &AgentConfig, acp_session_id: &str) -> Result<Self> {
        fs::create_dir_all(directory)?;
        let id = Uuid::now_v7();
        let path = directory.join(format!("{id}.jsonl"));
        let mut log = Self {
            writer: BufWriter::new(
                File::create_new(&path)
                    .with_context(|| format!("could not create {}", path.display()))?,
            ),
        };
        log.write(json!({
            "type": "session", "id": id.to_string(), "acp_session_id": acp_session_id,
            "created_at": Utc::now(), "agent": config.name, "harness": config.harness
        }))?;
        Ok(log)
    }

    pub fn append(&mut self, entry: &SessionEntry) -> Result<()> {
        #[derive(Serialize)]
        struct Record<'a> {
            #[serde(flatten)]
            entry: &'a SessionEntry,
            created_at: chrono::DateTime<Utc>,
        }
        self.write(Record {
            entry,
            created_at: Utc::now(),
        })
    }

    pub fn message(&mut self, role: Role, content: &str) -> Result<()> {
        self.append(&SessionEntry::Message(SessionMessage {
            role,
            content: content.to_owned(),
        }))
    }

    pub fn stream(&mut self, event: &StreamEvent) -> Result<()> {
        #[derive(Serialize)]
        struct Record<'a> {
            r#type: &'static str,
            event: &'a StreamEvent,
            created_at: chrono::DateTime<Utc>,
        }
        self.write(Record {
            r#type: "stream",
            event,
            created_at: Utc::now(),
        })
    }

    fn write(&mut self, value: impl Serialize) -> Result<()> {
        serde_json::to_writer(&mut self.writer, &value)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}

fn log_paths(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut logs = match fs::read_dir(directory) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error.into()),
    };
    logs.retain(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"));
    logs.sort();
    Ok(logs)
}

/// Reads one record at a time and tolerates an interrupted final write.
struct Records {
    reader: BufReader<File>,
    path: PathBuf,
    line_number: usize,
    line: String,
}

impl Records {
    fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            reader: BufReader::new(
                File::open(path)
                    .with_context(|| format!("could not read session log {}", path.display()))?,
            ),
            path: path.to_owned(),
            line_number: 0,
            line: String::new(),
        })
    }

    fn next_value(&mut self) -> Result<Option<Value>> {
        self.line.clear();
        if self.reader.read_line(&mut self.line)? == 0 {
            return Ok(None);
        }
        self.line_number += 1;
        match serde_json::from_str(&self.line) {
            Ok(value) => Ok(Some(value)),
            Err(_) if !self.line.ends_with('\n') => Ok(None),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "{} contains invalid JSONL at line {}",
                    self.path.display(),
                    self.line_number
                )
            }),
        }
    }
}

/// Converts the original activity records at the storage boundary.
fn decode_entry(value: Value) -> Result<Option<SessionEntry>> {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text = |key: &str, fallback: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_owned()
    };
    let event = match kind {
        "message" | "stream" | "turn_end" | "error" => {
            return Ok(Some(serde_json::from_value(value)?))
        }
        "reasoning" => StreamEvent::Reasoning(text("content", "")),
        "tool_call" | "tool_result" => StreamEvent::Tool {
            call_id: text("id", ""),
            title: text("title", "Tool call"),
            kind: value.get("kind").and_then(Value::as_str).map(str::to_owned),
            status: value
                .get("status")
                .and_then(|status| ToolStatus::deserialize(status).ok()),
            input: value.get("input").filter(|v| !v.is_null()).cloned(),
            output: value.get("output").filter(|v| !v.is_null()).cloned(),
        },
        "permission_request" => StreamEvent::PermissionRequest {
            title: text("title", "Permission required"),
            tool_call: value.get("toolCall").cloned(),
            options: Vec::new(),
            cancelled: false,
        },
        "permission_decision" => StreamEvent::PermissionDecision {
            title: text("title", "Permission"),
            allowed: value.get("allowed").and_then(Value::as_bool) == Some(true),
        },
        _ => return Ok(None),
    };
    Ok(Some(SessionEntry::Stream { event }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn history_stops_before_opening_older_logs_and_keeps_chronological_order() {
        let directory = tempdir().unwrap();
        // Opening a dangling link fails if the history reader reaches this old log.
        std::os::unix::fs::symlink("missing", directory.path().join("00.jsonl")).unwrap();
        let mut log = String::new();
        for index in 0..1005 {
            log.push_str(&format!(
                "{{\"type\":\"message\",\"role\":\"user\",\"content\":\"Prompt {index}\"}}\n"
            ));
        }
        fs::write(directory.path().join("01.jsonl"), log).unwrap();
        let history = SessionLog::prompt_history(directory.path()).unwrap();
        assert_eq!(history.len(), 1000);
        assert_eq!(history.first().unwrap(), "Prompt 5");
        assert_eq!(history.last().unwrap(), "Prompt 1004");
    }

    #[test]
    fn picker_joins_resumed_logs_and_uses_the_last_message_date() {
        let directory = tempdir().unwrap();
        for (name, content) in [
            ("01.jsonl", concat!(
                "{\"acp_session_id\":\"same\",\"harness\":\"codex\"}\n",
                "{\"type\":\"message\",\"role\":\"user\",\"content\":\"Old prompt\",\"created_at\":\"2026-09-01T12:00:00Z\"}\n")),
            ("02.jsonl", concat!(
                "{\"acp_session_id\":\"other\",\"harness\":\"codex\"}\n",
                "{\"type\":\"message\",\"role\":\"user\",\"content\":\"Other prompt\",\"created_at\":\"2026-09-02T12:00:00Z\"}\n")),
            ("03.jsonl", concat!(
                "{\"acp_session_id\":\"same\",\"harness\":\"codex\"}\n",
                "{\"type\":\"stream\",\"event\":{\"event\":\"assistant\",\"data\":\"Latest \"},\"created_at\":\"2026-09-03T12:00:00Z\"}\n",
                "{\"type\":\"stream\",\"event\":{\"event\":\"assistant\",\"data\":\"answer\"},\"created_at\":\"2026-09-03T12:00:01Z\"}\n",
                "{\"type\":\"turn_end\",\"created_at\":\"2026-09-04T12:00:00Z\"}\n")),
        ] { fs::write(directory.path().join(name), content).unwrap(); }
        let sessions = SessionLog::list(directory.path()).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "same");
        assert_eq!(sessions[0].last_message, "Latest answer");
        assert_eq!(
            sessions[0].last_message_at.unwrap().to_rfc3339(),
            "2026-09-03T12:00:01+00:00"
        );
        assert_eq!(
            SessionLog::prompt_history(directory.path()).unwrap(),
            ["Old prompt", "Other prompt"]
        );
    }

    #[test]
    fn resume_does_not_parse_an_unrelated_session_body() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("01.jsonl"),
            "{\"acp_session_id\":\"other\"}\ninvalid\n",
        )
        .unwrap();
        fs::write(directory.path().join("02.jsonl"), "{\"acp_session_id\":\"wanted\"}\n{\"type\":\"message\",\"role\":\"user\",\"content\":\"Hi\"}\n").unwrap();
        assert_eq!(
            SessionLog::messages(directory.path(), "wanted")
                .unwrap()
                .len(),
            1
        );
        assert!(SessionLog::messages(directory.path(), "other").is_err());
    }

    #[test]
    fn stream_events_are_readable_before_the_turn_finishes() {
        let directory = tempdir().unwrap();
        let config = AgentConfig::new("Test", "test", "Test", "fake");
        let mut log = SessionLog::create(directory.path(), &config, "session").unwrap();
        let events = [
            StreamEvent::Assistant("Before".into()),
            StreamEvent::Tool {
                call_id: "1".into(),
                title: "Read".into(),
                kind: None,
                status: Some(ToolStatus::Pending),
                input: None,
                output: None,
            },
            StreamEvent::Assistant("After".into()),
        ];
        for event in &events {
            log.stream(event).unwrap();
        }
        let entries = SessionLog::messages(directory.path(), "session").unwrap();
        assert_eq!(
            entries,
            events
                .into_iter()
                .map(|event| SessionEntry::Stream { event })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn lists_sessions_by_last_message_date() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("01-old.jsonl"),
            r#"{"acp_session_id":"old"}
{"type":"message","role":"user","content":"old","created_at":"2026-09-01T10:00:00Z"}
"#,
        )
        .unwrap();
        fs::write(
            directory.path().join("02-new.jsonl"),
            r#"{"acp_session_id":"new"}
{"type":"message","role":"user","content":"new","created_at":"2026-09-02T10:00:00Z"}
"#,
        )
        .unwrap();
        assert_eq!(SessionLog::list(directory.path()).unwrap()[0].id, "new");
    }

    #[test]
    fn ignores_a_newer_empty_session() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("01-useful.jsonl"),
            concat!(
                "{\"acp_session_id\":\"useful\"}\n",
                "{\"type\":\"message\",\"role\":\"user\",\"content\":\"Hello\"}\n"
            ),
        )
        .unwrap();
        fs::write(
            directory.path().join("02-empty.jsonl"),
            "{\"acp_session_id\":\"empty\"}\n",
        )
        .unwrap();

        assert_eq!(SessionLog::list(directory.path()).unwrap()[0].id, "useful");
    }

    #[test]
    fn reports_when_no_previous_session_exists() {
        let directory = tempdir().unwrap();
        assert!(SessionLog::list(directory.path()).unwrap().is_empty());
    }

    #[test]
    fn collects_messages_from_every_log_for_the_session() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("01.jsonl"),
            concat!(
                "{\"acp_session_id\":\"same\"}\n",
                "{\"type\":\"message\",\"role\":\"user\",\"content\":\"Hello\"}\n"
            ),
        )
        .unwrap();
        fs::write(
            directory.path().join("02.jsonl"),
            concat!(
                "{\"acp_session_id\":\"same\"}\n",
                "{\"type\":\"message\",\"role\":\"assistant\",\"content\":\"Hi\"}\n"
            ),
        )
        .unwrap();
        fs::write(
            directory.path().join("03.jsonl"),
            "{\"acp_session_id\":\"other\"}\n",
        )
        .unwrap();

        assert_eq!(
            SessionLog::messages(directory.path(), "same").unwrap(),
            [
                SessionEntry::Message(SessionMessage {
                    role: Role::User,
                    content: "Hello".into()
                }),
                SessionEntry::Message(SessionMessage {
                    role: Role::Assistant,
                    content: "Hi".into()
                })
            ]
        );
    }

    #[test]
    fn collects_activity_events() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("01.jsonl"),
            concat!(
                "{\"acp_session_id\":\"same\"}\n",
                "{\"type\":\"reasoning\",\"content\":\"Inspecting\"}\n",
                "{\"type\":\"tool_call\",\"id\":\"call-1\",\"title\":\"Read file\"}\n"
            ),
        )
        .unwrap();

        let entries = SessionLog::messages(directory.path(), "same").unwrap();
        assert_eq!(entries.len(), 2);
        assert!(matches!(
            &entries[0],
            SessionEntry::Stream {
                event: StreamEvent::Reasoning(_)
            }
        ));
    }

    #[test]
    fn prompt_history_skips_an_unreadable_log() {
        let directory = tempdir().unwrap();
        std::os::unix::fs::symlink("missing", directory.path().join("02.jsonl")).unwrap();
        fs::write(
            directory.path().join("01.jsonl"),
            "{\"type\":\"message\",\"role\":\"user\",\"content\":\"Hello\"}\n",
        )
        .unwrap();
        assert_eq!(
            SessionLog::prompt_history(directory.path()).unwrap(),
            ["Hello"]
        );
    }

    #[test]
    fn ignores_a_truncated_final_record() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("01.jsonl"),
            concat!(
                "{\"acp_session_id\":\"same\"}\n",
                "{\"type\":\"message\",\"role\":\"user\",\"content\":\"Hello\"}\n",
                "{\"type\":\"message\""
            ),
        )
        .unwrap();

        let entries = SessionLog::messages(directory.path(), "same").unwrap();
        assert_eq!(entries.len(), 1);
    }
}
