use std::collections::VecDeque;
use std::fmt;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};

use crate::harness::Launch;

const CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CANCEL_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
pub enum ConfigKind {
    Model,
    Thinking,
}

#[derive(Clone, Debug)]
pub struct ConfigChoice {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SelectConfigOption {
    pub id: String,
    pub current_value: String,
    pub choices: Vec<ConfigChoice>,
}

#[derive(Clone, Debug)]
pub enum StreamEvent {
    Assistant(String),
    Reasoning(String),
    Tool {
        call_id: String,
        title: String,
        kind: Option<String>,
        status: Option<String>,
        input: Option<Value>,
        output: Option<Value>,
    },
    PermissionRequest {
        title: String,
        options: Vec<PermissionOption>,
    },
    PermissionDecision {
        title: String,
        allowed: bool,
    },
    SystemError(String),
}

#[derive(Clone, Debug)]
pub struct PermissionOption {
    pub id: String,
    pub kind: String,
    pub label: String,
}

#[derive(Debug)]
pub struct PromptTimeout {
    connection_ready: bool,
}

impl PromptTimeout {
    pub fn connection_ready(&self) -> bool {
        self.connection_ready
    }
}

impl fmt::Display for PromptTimeout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.connection_ready {
            formatter.write_str("agent stopped responding after 60 seconds")
        } else {
            formatter.write_str("agent did not stop after cancellation; restart chat")
        }
    }
}

impl std::error::Error for PromptTimeout {}

#[derive(Debug)]
pub struct ProtocolError {
    code: i64,
    message: String,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ACP error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for ProtocolError {}

pub struct Client {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
    session_id: String,
    config_options: Vec<Value>,
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
        #[cfg(unix)]
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
        let output = BufReader::new(child.stdout.take().context("ACP adapter has no stdout")?);
        let stderr = child.stderr.take().context("ACP adapter has no stderr")?;
        let diagnostics = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_task = capture_stderr(stderr, Arc::clone(&diagnostics));
        let mut client = Self {
            child,
            input,
            output,
            next_id: 1,
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
                |_| Ok(None),
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
            let response = match client
                .request(method, session_params.clone(), |_| Ok(None))
                .await
            {
                Ok(response) => response,
                Err(resume_error)
                    if method == "session/resume" && supports_load_session(&initialize) =>
                {
                    client
                        .request("session/load", session_params, |_| Ok(None))
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
                .request("session/new", session_params, |_| Ok(None))
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

    pub async fn close(&mut self) {
        if self.closed {
            return;
        }
        let session_id = self.session_id.clone();
        if !session_id.is_empty() {
            let _ = timeout(
                Duration::from_secs(2),
                self.request("session/close", json!({"sessionId": session_id}), |_| {
                    Ok(None)
                }),
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

    #[cfg(unix)]
    fn terminate_process_group(&self, signal: libc::c_int) {
        if let Some(pid) = self.child.id() {
            // The adapter is the leader of its own process group.
            unsafe {
                libc::kill(-(pid as libc::pid_t), signal);
            }
        }
    }

    #[cfg(not(unix))]
    fn terminate_process_group(&mut self, _signal: libc::c_int) {
        let _ = self.child.start_kill();
    }

    pub fn select_config_option(&self, kind: ConfigKind) -> Option<SelectConfigOption> {
        self.config_options.iter().find_map(|option| {
            let id = option.get("id")?.as_str()?;
            let category = option.get("category").and_then(Value::as_str);
            if !matches_kind(kind, id, category) || option.get("type")?.as_str()? != "select" {
                return None;
            }
            let current_value = option.get("currentValue")?.as_str()?.to_owned();
            let choices = option
                .get("options")?
                .as_array()?
                .iter()
                .flat_map(flatten_choices)
                .collect::<Vec<_>>();
            (!choices.is_empty()).then(|| SelectConfigOption {
                id: id.to_owned(),
                current_value,
                choices,
            })
        })
    }

    pub async fn set_config_option(&mut self, id: &str, value: &str) -> Result<()> {
        let session_id = self.session_id.clone();
        let response = self
            .request(
                "session/set_config_option",
                json!({"sessionId": session_id, "configId": id, "value": value}),
                |_| Ok(None),
            )
            .await
            .with_context(|| format!("could not set ACP option {id:?}"))?;
        self.update_config_options(&response);
        Ok(())
    }

    pub async fn set_config(&mut self, kind: ConfigKind, value: &str) -> Result<bool> {
        let Some(option) = self.select_config_option(kind) else {
            return Ok(false);
        };
        if !option.choices.iter().any(|choice| choice.value == value) {
            return Ok(false);
        }
        self.set_config_option(&option.id, value).await?;
        Ok(true)
    }

    pub fn current_config_label(&self, kind: ConfigKind) -> Option<String> {
        let option = self.select_config_option(kind)?;
        option
            .choices
            .iter()
            .find(|choice| choice.value == option.current_value)
            .map(|choice| choice.label.clone())
            .or(Some(option.current_value))
    }

    pub fn diagnostics(&self) -> Vec<String> {
        self.diagnostics
            .lock()
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn friendly_error(&self, error: &anyhow::Error, system_error: &str) -> String {
        friendly_error(self.runtime_name, error, system_error, &self.diagnostics())
    }

    pub async fn prompt<F>(&mut self, text: &str, on_event: F) -> Result<String>
    where
        F: FnMut(StreamEvent) -> Result<Option<String>>,
    {
        self.prompt_with_timeout(text, on_event, Duration::from_secs(60))
            .await
    }

    async fn prompt_with_timeout<F>(
        &mut self,
        text: &str,
        mut on_event: F,
        inactivity_timeout: Duration,
    ) -> Result<String>
    where
        F: FnMut(StreamEvent) -> Result<Option<String>>,
    {
        let mut answer = String::new();
        let mut system_error = false;
        let mut citations = CitationFilter::default();
        let session_id = self.session_id.clone();
        self.request_with_timeout(
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": text}]
            }),
            |update| {
                match update.get("sessionUpdate").and_then(Value::as_str) {
                    Some("agent_message_chunk") => {
                        if let Some(text) = message_text(update) {
                            if system_error || text.trim_start().starts_with("[SYSTEM_ERROR]") {
                                system_error = true;
                                let _ = on_event(StreamEvent::SystemError(text.to_owned()))?;
                            } else {
                                let text = citations.push(text);
                                if !text.is_empty() {
                                    answer.push_str(&text);
                                    let _ = on_event(StreamEvent::Assistant(text))?;
                                }
                            }
                        }
                    }
                    Some("agent_thought_chunk") => {
                        if let Some(text) = message_text(update) {
                            let _ = on_event(StreamEvent::Reasoning(text.to_owned()))?;
                        }
                    }
                    Some("tool_call" | "tool_call_update") => {
                        let Some(call_id) = update
                            .get("toolCallId")
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty())
                        else {
                            return Ok(None);
                        };
                        let _ = on_event(StreamEvent::Tool {
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
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            input: update.get("rawInput").cloned(),
                            output: update
                                .get("rawOutput")
                                .filter(|value| !value.is_null())
                                .cloned()
                                .or_else(|| update.get("content").cloned())
                                .or_else(|| update.get("locations").cloned()),
                        })?;
                    }
                    Some("permission_request") => {
                        let title = update
                            .get("title")
                            .and_then(Value::as_str)
                            .unwrap_or("The agent wants to use a tool")
                            .to_owned();
                        let options = update
                            .get("options")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(permission_option)
                            .collect();
                        return on_event(StreamEvent::PermissionRequest { title, options });
                    }
                    Some("permission_decision") => {
                        let title = update
                            .get("title")
                            .and_then(Value::as_str)
                            .unwrap_or("The agent wants to use a tool")
                            .to_owned();
                        let _ = on_event(StreamEvent::PermissionDecision {
                            title,
                            allowed: update.get("allowed").and_then(Value::as_bool) == Some(true),
                        })?;
                    }
                    _ => {}
                }
                Ok(None)
            },
            Some(inactivity_timeout),
        )
        .await?;
        if !system_error {
            let text = citations.finish();
            if !text.is_empty() {
                answer.push_str(&text);
                let _ = on_event(StreamEvent::Assistant(text))?;
            }
        }
        Ok(answer)
    }

    async fn request<F>(&mut self, method: &str, params: Value, on_update: F) -> Result<Value>
    where
        F: FnMut(&Value) -> Result<Option<String>>,
    {
        match timeout(
            CONTROL_REQUEST_TIMEOUT,
            self.request_with_timeout(method, params, on_update, None),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => bail!("ACP {method} did not respond after 30 seconds"),
        }
    }

    async fn request_with_timeout<F>(
        &mut self,
        method: &str,
        params: Value,
        mut on_update: F,
        inactivity_timeout: Option<Duration>,
    ) -> Result<Value>
    where
        F: FnMut(&Value) -> Result<Option<String>>,
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

        let mut cancelled = false;
        loop {
            let message = if let Some(duration) = inactivity_timeout {
                match timeout(
                    if cancelled {
                        CANCEL_DRAIN_TIMEOUT.min(duration)
                    } else {
                        duration
                    },
                    self.read(),
                )
                .await
                {
                    Ok(message) => message?,
                    Err(_) if !cancelled => {
                        cancelled = true;
                        let session_id = self.session_id.clone();
                        self.write(&json!({
                            "jsonrpc": "2.0",
                            "method": "session/cancel",
                            "params": {"sessionId": session_id}
                        }))
                        .await?;
                        continue;
                    }
                    Err(_) => {
                        return Err(PromptTimeout {
                            connection_ready: false,
                        }
                        .into())
                    }
                }
            } else {
                self.read().await?
            };
            if message.get("method").is_some() && message.get("id").is_some() {
                self.handle_request(&message, &mut on_update).await?;
                continue;
            }
            if message.get("method").and_then(Value::as_str) == Some("session/update") {
                if !cancelled {
                    if let Some(update) = message.pointer("/params/update") {
                        let _ = on_update(update)?;
                    }
                }
                continue;
            }
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if cancelled {
                return Err(PromptTimeout {
                    connection_ready: true,
                }
                .into());
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

    fn update_config_options(&mut self, response: &Value) {
        if let Some(options) = response.get("configOptions").and_then(Value::as_array) {
            self.config_options.clone_from(options);
        }
    }

    async fn handle_request<F>(&mut self, request: &Value, on_update: &mut F) -> Result<()>
    where
        F: FnMut(&Value) -> Result<Option<String>>,
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
        let selected = on_update(&json!({
            "sessionUpdate": "permission_request",
            "title": title,
            "options": options
        }))?;
        let allowed = selected.as_deref().is_some_and(|selected| {
            options.iter().any(|option| {
                option.get("optionId").and_then(Value::as_str) == Some(selected)
                    && matches!(
                        option.get("kind").and_then(Value::as_str),
                        Some("allow_once" | "allow_always")
                    )
            })
        });
        on_update(&json!({
            "sessionUpdate": "permission_decision",
            "title": title,
            "allowed": allowed
        }))?;
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

    async fn read(&mut self) -> Result<Value> {
        let mut line = String::new();
        let read = self.output.read_line(&mut line).await?;
        if read == 0 {
            let status = self.child.wait().await?;
            bail!("ACP adapter stopped unexpectedly with {status}");
        }
        serde_json::from_str(&line).context("ACP adapter returned invalid JSON")
    }

    async fn write(&mut self, value: &Value) -> Result<()> {
        self.input
            .write_all(serde_json::to_string(value)?.as_bytes())
            .await?;
        self.input.write_all(b"\n").await?;
        self.input.flush().await?;
        Ok(())
    }
}

fn capture_stderr(
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

pub fn friendly_error(
    runtime_name: &str,
    error: &anyhow::Error,
    system_error: &str,
    diagnostics: &[String],
) -> String {
    let details = format!("{system_error}\n{}\n{error:#}", diagnostics.join("\n"));
    let lowercase = details.to_ascii_lowercase();
    if lowercase.contains("usage limit") {
        if let Some(start) = lowercase.find("try again at") {
            let retry = details[start..]
                .split(['.', '\n'])
                .next()
                .unwrap_or("try again later")
                .trim();
            return format!("Usage limit reached. {}.", capitalize(retry));
        }
        return "Usage limit reached. Try again later.".to_owned();
    }
    if lowercase.contains("authentication")
        || lowercase.contains("not logged in")
        || lowercase.contains("no api key")
        || lowercase.contains("missing credential")
    {
        return format!("{runtime_name} authentication failed. Sign in and try again.");
    }
    format!("{runtime_name} could not complete the request. Try again.")
}

fn capitalize(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

impl Drop for Client {
    fn drop(&mut self) {
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

fn matches_kind(kind: ConfigKind, id: &str, category: Option<&str>) -> bool {
    match kind {
        ConfigKind::Model => category == Some("model") || id == "model",
        ConfigKind::Thinking => {
            category == Some("thought_level")
                || ["reasoning", "thinking", "effort"]
                    .iter()
                    .any(|hint| id.contains(hint))
        }
    }
}

fn flatten_choices(value: &Value) -> Vec<ConfigChoice> {
    if let Some(options) = value.get("options").and_then(Value::as_array) {
        return options.iter().flat_map(flatten_choices).collect();
    }
    let Some(choice_value) = value.get("value").and_then(Value::as_str) else {
        return Vec::new();
    };
    vec![ConfigChoice {
        value: choice_value.to_owned(),
        label: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(choice_value)
            .to_owned(),
        description: value
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }]
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
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use tempfile::tempdir;

    #[test]
    fn flattens_grouped_select_options() {
        let options = json!({
            "group": "Models",
            "options": [
                {"value": "fast", "name": "Fast"},
                {"value": "deep", "name": "Deep", "description": "More capable"}
            ]
        });
        let choices = flatten_choices(&options);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[1].value, "deep");
        assert_eq!(choices[1].label, "Deep");
        assert_eq!(choices[1].description.as_deref(), Some("More capable"));
    }

    #[test]
    fn recognizes_standard_model_and_thinking_options() {
        assert!(matches_kind(ConfigKind::Model, "model", Some("model")));
        assert!(matches_kind(
            ConfigKind::Thinking,
            "reasoning_effort",
            Some("thought_level")
        ));
        assert!(matches_kind(ConfigKind::Thinking, "effort", None));
        assert!(!matches_kind(ConfigKind::Model, "mode", None));
    }

    #[test]
    fn identifies_provider_api_key_errors_as_authentication_errors() {
        let error = anyhow::anyhow!("No API key for openrouter");
        assert_eq!(
            friendly_error("Pi", &error, "", &[]),
            "Pi authentication failed. Sign in and try again."
        );
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

    #[test]
    fn selects_the_advertised_continuation_method() {
        assert_eq!(
            continuation_method(&json!({
                "agentCapabilities": {"sessionCapabilities": {"resume": {}}}
            })),
            Some("session/resume")
        );
        assert_eq!(
            continuation_method(&json!({
                "agentCapabilities": {"loadSession": true}
            })),
            Some("session/load")
        );
        assert_eq!(continuation_method(&json!({})), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_a_streaming_session_with_tools_and_permission() {
        let directory = tempdir().unwrap();
        let adapter = directory.path().join("fake-acp.sh");
        fs::write(
            &adapter,
            r##"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"agentCapabilities":{}}}'
      ;;
    *'"method":"session/new"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"fake-session"}}'
      ;;
    *'"method":"session/prompt"'*)
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_thought_chunk","content":{"text":"Thinking"}}}}'
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"Read file","status":"pending"}}}'
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","title":"Read file","status":"completed","rawOutput":"done"}}}'
      printf '%s\n' '{"jsonrpc":"2.0","id":77,"method":"session/request_permission","params":{"toolCall":{"title":"Write file"},"options":[{"optionId":"allow","name":"Allow once","kind":"allow_once"},{"optionId":"reject","name":"Reject once","kind":"reject_once"}]}}'
      IFS= read -r permission
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"text":"Hello"}}}}'
      printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}'
      ;;
    *'"method":"session/close"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{}}'
      exit 0
      ;;
  esac
done
"##,
        )
        .unwrap();
        fs::set_permissions(&adapter, fs::Permissions::from_mode(0o755)).unwrap();
        let launch = Launch {
            runtime_name: "Fake",
            program: "/bin/sh".into(),
            args: vec![adapter.into_os_string()],
            env: Default::default(),
            session_meta: None,
        };
        let mut client = Client::start(launch, directory.path(), Vec::new(), None)
            .await
            .unwrap();
        let mut events = Vec::new();
        let answer = client
            .prompt("Hi", |event| {
                let selection = match &event {
                    StreamEvent::PermissionRequest { options, .. } => {
                        options.first().map(|option| option.id.clone())
                    }
                    _ => None,
                };
                events.push(event);
                Ok(selection)
            })
            .await
            .unwrap();
        assert_eq!(answer, "Hello");
        assert!(events
            .iter()
            .any(|event| matches!(event, StreamEvent::Reasoning(_))));
        assert!(events.iter().any(|event| matches!(event, StreamEvent::Tool { status: Some(status), .. } if status == "completed")));
        assert!(events
            .iter()
            .any(|event| matches!(event, StreamEvent::PermissionDecision { allowed: true, .. })));
        client.close().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn drains_a_cancelled_turn_before_the_next_prompt() {
        let directory = tempdir().unwrap();
        let adapter = directory.path().join("cancel-acp.sh");
        fs::write(
            &adapter,
            r##"#!/bin/sh
prompts=0
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"agentCapabilities":{}}}'
      ;;
    *'"method":"session/new"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"fake-session"}}'
      ;;
    *'"method":"session/prompt"'*)
      prompts=$((prompts + 1))
      if [ "$prompts" -eq 1 ]; then
        IFS= read -r cancel
        printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"cancelled"}}'
      else
        printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"text":"Recovered"}}}}'
        printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{"stopReason":"end_turn"}}'
      fi
      ;;
    *'"method":"session/close"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":5,"result":{}}'
      exit 0
      ;;
  esac
done
"##,
        )
        .unwrap();
        fs::set_permissions(&adapter, fs::Permissions::from_mode(0o755)).unwrap();
        let launch = Launch {
            runtime_name: "Fake",
            program: "/bin/sh".into(),
            args: vec![adapter.into_os_string()],
            env: Default::default(),
            session_meta: None,
        };
        let mut client = Client::start(launch, directory.path(), Vec::new(), None)
            .await
            .unwrap();
        let error = client
            .prompt_with_timeout("Wait", |_| Ok(None), Duration::from_millis(10))
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<PromptTimeout>().is_some());
        let answer = client.prompt("Again", |_| Ok(None)).await.unwrap();
        assert_eq!(answer, "Recovered");
        client.close().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn marks_the_connection_unusable_when_cancellation_does_not_finish() {
        let directory = tempdir().unwrap();
        let adapter = directory.path().join("stuck-acp.sh");
        fs::write(
            &adapter,
            r##"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"agentCapabilities":{}}}'
      ;;
    *'"method":"session/new"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"fake-session"}}'
      ;;
    *'"method":"session/prompt"'*)
      sleep 1
      ;;
  esac
done
"##,
        )
        .unwrap();
        fs::set_permissions(&adapter, fs::Permissions::from_mode(0o755)).unwrap();
        let launch = Launch {
            runtime_name: "Fake",
            program: "/bin/sh".into(),
            args: vec![adapter.into_os_string()],
            env: Default::default(),
            session_meta: None,
        };
        let mut client = Client::start(launch, directory.path(), Vec::new(), None)
            .await
            .unwrap();

        let error = client
            .prompt_with_timeout("Wait", |_| Ok(None), Duration::from_millis(10))
            .await
            .unwrap_err();
        let timeout = error.downcast_ref::<PromptTimeout>().unwrap();
        assert!(!timeout.connection_ready());
        drop(client);
    }
}
