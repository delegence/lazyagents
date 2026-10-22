use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};

use super::{Client, PromptTimeout, ProtocolError, StreamEvent, Updates, CONTROL_REQUEST_TIMEOUT};

impl Client {
    /// Sends a control request. Permission requests during it are cancelled.
    pub(super) async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let mut updates = Updates::new(|_| Ok(None));
        match timeout(
            CONTROL_REQUEST_TIMEOUT,
            self.request_with_timeout(method, params, &mut updates, None),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => bail!("ACP {method} did not respond after 30 seconds"),
        }
    }

    pub(super) async fn request_with_timeout<F>(
        &mut self,
        method: &str,
        params: Value,
        updates: &mut Updates<F>,
        inactivity_timeout: Option<Duration>,
    ) -> Result<Value>
    where
        F: FnMut(StreamEvent) -> Result<Option<String>>,
    {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .await?;

        if method == "session/prompt" {
            self.pending_prompt = Some(id);
        }
        loop {
            let message = if let Some(duration) = inactivity_timeout {
                match timeout(duration, self.read()).await {
                    Ok(message) => message?,
                    Err(_) => {
                        self.cancel_pending_prompt(updates).await.map_err(|error| {
                            error.context(PromptTimeout {
                                connection_ready: false,
                            })
                        })?;
                        return Err(PromptTimeout {
                            connection_ready: true,
                        }
                        .into());
                    }
                }
            } else {
                self.read().await?
            };
            if message.get("method").is_some() && message.get("id").is_some() {
                self.handle_request(&message, updates, false).await?;
                continue;
            }
            if message.get("method").and_then(Value::as_str) == Some("session/update") {
                if let Some(update) = message.pointer("/params/update") {
                    updates.update(update)?;
                }
                continue;
            }
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if method == "session/prompt" {
                self.pending_prompt = None;
            }
            if let Some(error) = message.get("error") {
                let code = error
                    .get("code")
                    .and_then(Value::as_i64)
                    .unwrap_or_default();
                let text = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown ACP error");
                return Err(ProtocolError {
                    code,
                    message: text.to_owned(),
                }
                .into());
            }
            return message
                .get("result")
                .cloned()
                .context("ACP response has no result");
        }
    }

    pub(super) async fn read(&mut self) -> Result<Value> {
        let Some(line) = self.output.next_line().await? else {
            let status = self.child.wait().await?;
            bail!("ACP adapter stopped unexpectedly with {status}");
        };
        let message: Value =
            serde_json::from_str(&line).context("ACP adapter returned invalid JSON")?;
        if let Some(usage) = message
            .pointer("/result/usage")
            .filter(|value| value.is_object())
        {
            self.usage = Some(usage.clone());
        }
        if let Some(update) = message.pointer("/params/update") {
            if update["sessionUpdate"] == "usage_update" {
                self.context_usage = Some(update.clone());
            }
        }
        Ok(message)
    }

    pub(super) async fn write(&mut self, value: &Value) -> Result<()> {
        self.input
            .write_all(serde_json::to_string(value)?.as_bytes())
            .await?;
        self.input.write_all(b"\n").await?;
        self.input.flush().await?;
        Ok(())
    }

    pub(super) fn terminate_process_group(&self, signal: libc::c_int) {
        if let Some(pid) = self.child.id() {
            // The adapter is the leader of its own process group.
            unsafe {
                libc::kill(-(pid as libc::pid_t), signal);
            }
        }
    }
}

pub(super) fn capture_stderr(
    stderr: tokio::process::ChildStderr,
    diagnostics: Arc<Mutex<VecDeque<String>>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(mut buffer) = diagnostics.lock() {
                if buffer.len() == 50 {
                    buffer.pop_front();
                }
                buffer.push_back(line);
            }
        }
    })
}
