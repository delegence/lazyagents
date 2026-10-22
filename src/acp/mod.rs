mod config;
mod error;
mod events;
mod requests;
#[cfg(test)]
mod tests;
mod transport;

use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;
use tokio::time::{timeout, timeout_at, Duration, Instant};

use crate::harness::Launch;
pub use config::{ConfigKind, SelectConfigOption};
pub use error::{friendly_error, PromptTimeout, ProtocolError};
use events::Updates;
pub use events::{StreamEvent, ToolStatus};
pub use requests::PermissionOption;
use transport::capture_stderr;

const CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CANCEL_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Client {
    pub usage: Option<Value>,
    pub context_usage: Option<Value>,
    permissions: Option<crate::permissions::Store>,
    child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    pending_prompt: Option<u64>,
    session_id: String,
    config_options: Vec<SelectConfigOption>,
    runtime_name: &'static str,
    closed: bool,
    diagnostics: Arc<Mutex<VecDeque<String>>>,
    stderr_task: JoinHandle<()>,
}

impl Client {
    pub async fn start(
        launch: Launch,
        cwd: &Path,
        mcp_servers: Vec<Value>,
        resume_session_id: Option<&str>,
    ) -> Result<Self> {
        let mut command = Command::new(&launch.program);
        command
            .args(&launch.args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command.process_group(0);
        for (name, value) in launch.env {
            command.env(name, value);
        }
        let mut child = command.spawn().with_context(|| {
            format!(
                "could not start ACP runtime at {}",
                launch.program.display()
            )
        })?;
        let input = child.stdin.take().context("ACP adapter has no stdin")?;
        let output =
            BufReader::new(child.stdout.take().context("ACP adapter has no stdout")?).lines();
        let stderr = child.stderr.take().context("ACP adapter has no stderr")?;
        let diagnostics = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_task = capture_stderr(stderr, Arc::clone(&diagnostics));
        let mut client = Self {
            usage: None,
            context_usage: None,
            permissions: None,
            child,
            input,
            output,
            next_id: 1,
            pending_prompt: None,
            session_id: String::new(),
            config_options: Vec::new(),
            runtime_name: launch.runtime_name,
            closed: false,
            diagnostics,
            stderr_task,
        };

        let initialize = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientInfo": {"name": "lazyagents", "version": env!("CARGO_PKG_VERSION")},
                    "clientCapabilities": {}
                }),
            )
            .await
            .context("ACP initialization failed")?;

        let mut session_params = json!({"cwd": cwd, "mcpServers": mcp_servers});
        if let Some(meta) = launch.session_meta {
            session_params["_meta"] = meta;
        }
        let response = if let Some(session_id) = resume_session_id {
            session_params["sessionId"] = Value::String(session_id.to_owned());
            let method = continuation_method(&initialize).context(
                "the selected harness does not support resuming persistent ACP sessions",
            )?;
            let response = match client.request(method, session_params.clone()).await
            {
                Ok(response) => response,
                Err(resume_error)
                    if method == "session/resume" && supports_load_session(&initialize) =>
                {
                    client
                        .request("session/load", session_params)
                        .await
                        .with_context(|| {
                            format!(
                                "could not resume ACP session {session_id:?}; session/resume failed: {resume_error:#}"
                            )
                        })?
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("could not resume ACP session {session_id:?}"));
                }
            };
            client.session_id = session_id.to_owned();
            response
        } else {
            let response = client
                .request("session/new", session_params)
                .await
                .context(
                    "could not create an ACP session; check that the selected harness is signed in",
                )?;
            client.session_id = response
                .get("sessionId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .context("ACP session response has no sessionId")?
                .to_owned();
            response
        };
        client.update_config_options(&response);
        Ok(client)
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Stops an interrupted prompt and drains its response before another turn can start.
    pub async fn cancel_prompt<F>(&mut self, on_event: F) -> Result<()>
    where
        F: FnMut(StreamEvent) -> Result<Option<String>>,
    {
        self.cancel_pending_prompt(&mut Updates::new(on_event))
            .await
    }

    async fn cancel_pending_prompt<F>(&mut self, updates: &mut Updates<F>) -> Result<()>
    where
        F: FnMut(StreamEvent) -> Result<Option<String>>,
    {
        let id = self
            .pending_prompt
            .context("the prompt was interrupted before it was sent; restart chat")?;
        let deadline = Instant::now() + CANCEL_DRAIN_TIMEOUT;
        timeout_at(deadline, async {
            self.write(&json!({
                "jsonrpc": "2.0", "method": "session/cancel",
                "params": {"sessionId": self.session_id}
            }))
            .await?;
            loop {
                let message = self.read().await?;
                if message.get("method").is_some() && message.get("id").is_some() {
                    self.handle_request(&message, updates, true).await?;
                } else if message.get("method").and_then(Value::as_str) == Some("session/update") {
                    if let Some(update) = message.pointer("/params/update") {
                        updates.update(update)?;
                    }
                } else if message.get("id").and_then(Value::as_u64) == Some(id) {
                    self.pending_prompt = None;
                    return Ok(());
                }
            }
        })
        .await
        .context("the agent did not stop within five seconds; restart chat")?
    }

    pub async fn close(&mut self) {
        if self.closed {
            return;
        }
        let session_id = self.session_id.clone();
        if !session_id.is_empty() {
            let _ = timeout(
                Duration::from_secs(2),
                self.request("session/close", json!({"sessionId": session_id})),
            )
            .await;
        }
        let _ = self.input.shutdown().await;
        self.terminate_process_group(libc::SIGTERM);
        if timeout(Duration::from_secs(2), self.child.wait())
            .await
            .is_err()
        {
            self.terminate_process_group(libc::SIGKILL);
            let _ = self.child.wait().await;
        }
        self.closed = true;
        self.stderr_task.abort();
    }

    pub fn diagnostics(&self) -> Vec<String> {
        self.diagnostics
            .lock()
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub async fn prompt<F>(&mut self, text: &str, mut on_event: F) -> Result<String>
    where
        F: FnMut(StreamEvent) -> Result<Option<String>>,
    {
        let mut answer = String::new();
        self.prompt_with_timeout(
            text,
            |event| {
                if let StreamEvent::Assistant(text) = &event {
                    answer.push_str(text);
                }
                on_event(event)
            },
            None,
        )
        .await?;
        Ok(answer)
    }

    pub async fn prompt_with_timeout<F>(
        &mut self,
        text: &str,
        on_event: F,
        inactivity_timeout: Option<Duration>,
    ) -> Result<()>
    where
        F: FnMut(StreamEvent) -> Result<Option<String>>,
    {
        let mut updates = Updates::new(on_event);
        let session_id = self.session_id.clone();
        self.request_with_timeout(
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": text}]
            }),
            &mut updates,
            inactivity_timeout,
        )
        .await?;
        updates.finish()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stderr_task.abort();
        if !self.closed {
            self.terminate_process_group(libc::SIGKILL);
            let _ = self.child.start_kill();
        }
    }
}

fn continuation_method(initialize: &Value) -> Option<&'static str> {
    if initialize
        .pointer("/agentCapabilities/sessionCapabilities/resume")
        .is_some_and(Value::is_object)
    {
        Some("session/resume")
    } else if supports_load_session(initialize) {
        Some("session/load")
    } else {
        None
    }
}

fn supports_load_session(initialize: &Value) -> bool {
    initialize
        .pointer("/agentCapabilities/loadSession")
        .and_then(Value::as_bool)
        == Some(true)
}
