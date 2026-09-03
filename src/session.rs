use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::config::AgentConfig;

pub struct SessionLog {
    writer: BufWriter<File>,
}

#[derive(Debug, PartialEq)]
pub struct SessionMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, PartialEq)]
pub enum SessionEntry {
    Message(SessionMessage),
    Event { event_type: String, data: Value },
}

impl SessionLog {
    pub fn latest_acp_session_id(directory: &Path) -> Result<String> {
        for path in log_paths(directory)?.into_iter().rev() {
            if !has_messages(&path)? {
                continue;
            }
            let header = read_header(&path)?;
            return header
                .get("acp_session_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .with_context(|| format!("{} has no ACP session ID", path.display()));
        }
        bail!("no previous session is available")
    }

    pub fn messages(directory: &Path, acp_session_id: &str) -> Result<Vec<SessionEntry>> {
        let mut messages = Vec::new();
        for path in log_paths(directory)? {
            let mut values = read_values(&path)?.into_iter();
            let header = values
                .next()
                .with_context(|| format!("{} has no session header", path.display()))?;
            if header.get("acp_session_id").and_then(Value::as_str) != Some(acp_session_id) {
                continue;
            }
            for value in values {
                let event_type = value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if event_type == "message" {
                    let role = value
                        .get("role")
                        .and_then(Value::as_str)
                        .context("session message has no role")?;
                    let content = value
                        .get("content")
                        .and_then(Value::as_str)
                        .context("session message has no content")?;
                    messages.push(SessionEntry::Message(SessionMessage {
                        role: role.to_owned(),
                        content: content.to_owned(),
                    }));
                } else if matches!(
                    event_type,
                    "reasoning"
                        | "tool_call"
                        | "tool_result"
                        | "permission_request"
                        | "permission_decision"
                ) {
                    messages.push(SessionEntry::Event {
                        event_type: event_type.to_owned(),
                        data: value,
                    });
                }
            }
        }
        Ok(messages)
    }

    pub fn create(directory: &Path, config: &AgentConfig, acp_session_id: &str) -> Result<Self> {
        fs::create_dir_all(directory)?;
        let id = Uuid::now_v7();
        let path = directory.join(format!("{id}.jsonl"));
        let mut log = Self {
            writer: BufWriter::new(
                File::create(&path)
                    .with_context(|| format!("could not create {}", path.display()))?,
            ),
        };
        log.write(json!({
            "type": "session",
            "id": id.to_string(),
            "acp_session_id": acp_session_id,
            "created_at": Utc::now(),
            "agent": config.name,
            "harness": &config.harness
        }))?;
        Ok(log)
    }

    pub fn message(&mut self, role: &str, content: &str) -> Result<()> {
        self.write(json!({
            "type": "message",
            "role": role,
            "content": content,
            "created_at": Utc::now()
        }))
    }

    pub fn event(&mut self, event_type: &str, data: Value) -> Result<()> {
        let mut event = data.as_object().cloned().unwrap_or_default();
        event.insert("type".into(), Value::String(event_type.to_owned()));
        event.insert("created_at".into(), json!(Utc::now()));
        self.write(Value::Object(event))
    }

    fn write(&mut self, value: Value) -> Result<()> {
        serde_json::to_writer(&mut self.writer, &value)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}

fn log_paths(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut logs = match fs::read_dir(directory) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"))
            .collect::<Vec<_>>(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("no previous session is available")
        }
        Err(error) => return Err(error.into()),
    };
    logs.sort();
    Ok(logs)
}

fn read_header(path: &Path) -> Result<Value> {
    read_values(path)?
        .into_iter()
        .next()
        .with_context(|| format!("{} has no session header", path.display()))
}

fn has_messages(path: &Path) -> Result<bool> {
    for value in read_values(path)?.into_iter().skip(1) {
        if value.get("type").and_then(Value::as_str) == Some("message") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_values(path: &Path) -> Result<Vec<Value>> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("could not read session log {}", path.display()))?;
    let line_count = contents.lines().count();
    let has_complete_tail = contents.ends_with('\n');
    let mut values = Vec::with_capacity(line_count);
    for (index, line) in contents.lines().enumerate() {
        match serde_json::from_str(line) {
            Ok(value) => values.push(value),
            Err(_) if index + 1 == line_count && !has_complete_tail => break,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "{} contains invalid JSONL at line {}",
                        path.display(),
                        index + 1
                    )
                })
            }
        }
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn finds_the_latest_acp_session_id() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("01-old.jsonl"),
            r#"{"acp_session_id":"old"}
{"type":"message","role":"user","content":"old"}
"#,
        )
        .unwrap();
        fs::write(
            directory.path().join("02-new.jsonl"),
            r#"{"acp_session_id":"new"}
{"type":"message","role":"user","content":"new"}
"#,
        )
        .unwrap();
        assert_eq!(
            SessionLog::latest_acp_session_id(directory.path()).unwrap(),
            "new"
        );
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

        assert_eq!(
            SessionLog::latest_acp_session_id(directory.path()).unwrap(),
            "useful"
        );
    }

    #[test]
    fn reports_when_no_previous_session_exists() {
        let directory = tempdir().unwrap();
        assert_eq!(
            SessionLog::latest_acp_session_id(directory.path())
                .unwrap_err()
                .to_string(),
            "no previous session is available"
        );
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
                    role: "user".into(),
                    content: "Hello".into()
                }),
                SessionEntry::Message(SessionMessage {
                    role: "assistant".into(),
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
            SessionEntry::Event { event_type, .. } if event_type == "reasoning"
        ));
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
