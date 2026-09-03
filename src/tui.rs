use std::collections::HashMap;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{Datelike, Local};
use inquire::Select;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::signal;

use crate::acp::{Client, ConfigKind, PermissionOption, PromptTimeout, StreamEvent};
use crate::config::{paths, AgentConfig};
use crate::harness;
use crate::mcp;
use crate::session::{SessionEntry, SessionLog};

const HEADER_RULE: &str = "------------------------------------";
const DARK_GRAY: &str = "38;5;242";
const LIGHT_GRAY: &str = "38;5;250";

pub async fn run(root: PathBuf, resume: bool) -> Result<()> {
    let config = AgentConfig::load(&root)?;
    let agent_paths = paths(&root);
    let soul = fs::read_to_string(&agent_paths.soul)
        .with_context(|| format!("could not read {}", agent_paths.soul.display()))?;
    let instruction = runtime_instruction(&soul);
    let servers = mcp::load(&agent_paths.mcp)?;
    let launch = harness::launch(&root, &config.harness, &instruction)?;
    let session_id = if resume {
        Some(SessionLog::latest_acp_session_id(&agent_paths.sessions)?)
    } else {
        None
    };
    let history = match &session_id {
        Some(session_id) => SessionLog::messages(&agent_paths.sessions, session_id)?,
        None => Vec::new(),
    };
    let mut client = Client::start(launch, &root, servers, session_id.as_deref()).await?;
    apply_selection(&mut client, ConfigKind::Model, config.model.as_deref()).await?;
    apply_selection(
        &mut client,
        ConfigKind::Thinking,
        config.thinking.as_deref(),
    )
    .await?;
    let mut log = None;
    let model = client
        .current_config_label(ConfigKind::Model)
        .unwrap_or_else(|| "default".to_owned());
    let thinking = client
        .current_config_label(ConfigKind::Thinking)
        .unwrap_or_else(|| "default".to_owned());
    println!(
        "{}",
        crate::markdown::sanitize(&header(&config.name, &config.harness, &model, &thinking))
    );
    render_history(&history);
    let mut input = BufReader::new(tokio::io::stdin());

    loop {
        print!("> ");
        io::stdout().flush()?;
        let mut prompt = String::new();
        let bytes_read = tokio::select! {
            result = input.read_line(&mut prompt) => result?,
            _ = signal::ctrl_c() => {
                client.close().await;
                println!();
                return Ok(());
            }
        };
        if bytes_read == 0 {
            client.close().await;
            println!();
            return Ok(());
        }
        let prompt = prompt.trim_end_matches(['\r', '\n']);
        if prompt.trim().is_empty() {
            continue;
        }
        if matches!(prompt.trim(), "/exit" | "/quit") {
            client.close().await;
            return Ok(());
        }

        render_user_prompt(prompt);
        if log.is_none() {
            log = Some(SessionLog::create(
                &agent_paths.sessions,
                &config,
                client.session_id(),
            )?);
        }
        let log = log.as_mut().expect("the session log was created above");
        log.message("user", prompt)?;
        let mut streamed_answer = String::new();
        let mut system_error = String::new();
        let mut display = TurnDisplay::new();
        let answer = tokio::select! {
            biased;
            result = client.prompt(prompt, |event| {
                if let StreamEvent::Assistant(text) = &event {
                    streamed_answer.push_str(text);
                }
                if let StreamEvent::SystemError(text) = &event {
                    system_error.push_str(text);
                    return Ok(None);
                }
                display.handle(event)
            }) => Some(result),
            _ = signal::ctrl_c() => None,
        };
        if answer.is_none() {
            display.finish(log)?;
            if !streamed_answer.is_empty() {
                log.message("assistant", &streamed_answer)?;
            }
            client.close().await;
            return Ok(());
        }
        let answer = match answer.expect("the interrupted prompt returned above") {
            Ok(answer) => answer,
            Err(error) => {
                display.finish(log)?;
                if !streamed_answer.is_empty() {
                    log.message("assistant", &streamed_answer)?;
                }
                if let Some(timeout) = error.downcast_ref::<PromptTimeout>() {
                    print_colored(io::stdout().is_terminal(), "31", &format!("✗ {timeout}.\n"));
                    if timeout.connection_ready() {
                        continue;
                    }
                    client.close().await;
                    return Ok(());
                }
                if error.downcast_ref::<crate::acp::ProtocolError>().is_some() {
                    let friendly = client.friendly_error(&error, &system_error);
                    print_colored(io::stdout().is_terminal(), "31", &format!("✗ {friendly}\n"));
                    log.event("error", serde_json::json!({"message": friendly}))?;
                    continue;
                }
                client.close().await;
                return Err(error);
            }
        };
        display.finish(log)?;
        log.message("assistant", &answer)?;
    }
}

struct TurnDisplay {
    terminal: bool,
    reasoning: String,
    reasoning_open: bool,
    assistant: String,
    last_tool_line: Option<String>,
    tools: HashMap<String, ToolView>,
    records: Vec<DisplayRecord>,
}

impl TurnDisplay {
    fn new() -> Self {
        Self {
            terminal: io::stdout().is_terminal(),
            reasoning: String::new(),
            reasoning_open: false,
            assistant: String::new(),
            last_tool_line: None,
            tools: HashMap::new(),
            records: Vec::new(),
        }
    }

    fn handle(&mut self, event: StreamEvent) -> Result<Option<String>> {
        match event {
            StreamEvent::Assistant(text) => {
                self.flush_reasoning();
                self.last_tool_line = None;
                self.assistant.push_str(&text);
                self.flush_complete_assistant_blocks();
            }
            StreamEvent::Reasoning(text) => {
                self.flush_assistant();
                self.reasoning.push_str(&text);
                self.reasoning_open = true;
                self.last_tool_line = None;
            }
            StreamEvent::Tool {
                call_id,
                title,
                kind,
                status,
                input,
                output,
            } => {
                self.flush_reasoning();
                self.flush_assistant();
                let first = !self.tools.contains_key(&call_id);
                let title = if title == "Tool call" {
                    self.tools
                        .get(&call_id)
                        .map(|tool| tool.title.clone())
                        .unwrap_or(title)
                } else {
                    title
                };
                let tool = self
                    .tools
                    .entry(call_id.clone())
                    .or_insert_with(|| ToolView {
                        title: title.clone(),
                        kind: None,
                        status: None,
                        input: None,
                        output: None,
                        completed_displayed: false,
                        output_displayed: false,
                    });
                tool.title.clone_from(&title);
                if kind.is_some() {
                    tool.kind = kind;
                }
                if status.is_some() {
                    tool.status = status;
                }
                merge_optional_value(&mut tool.input, input);
                merge_optional_value(&mut tool.output, output);
                if first {
                    render_tool(self.terminal, &title, None, None);
                    self.records.push(DisplayRecord::ToolCall(call_id.clone()));
                    self.last_tool_line = Some(call_id.clone());
                }
                if is_terminal_tool_status(tool.status.as_deref()) && !tool.completed_displayed {
                    if self.terminal && self.last_tool_line.as_deref() == Some(&call_id) {
                        print!("\x1b[1A\r\x1b[2K");
                    }
                    render_tool(
                        self.terminal,
                        &title,
                        tool.status.as_deref(),
                        tool.output.as_ref(),
                    );
                    tool.output_displayed = tool.output.is_some();
                    tool.completed_displayed = true;
                    self.records
                        .push(DisplayRecord::ToolResult(call_id.clone()));
                    self.last_tool_line = None;
                } else if tool.completed_displayed && !tool.output_displayed {
                    if let Some(output) = &tool.output {
                        print_value(self.terminal, output);
                        tool.output_displayed = true;
                    }
                }
            }
            StreamEvent::PermissionRequest { title, options } => {
                self.flush_reasoning();
                self.flush_assistant();
                render_permission_request(self.terminal, &title);
                self.records.push(DisplayRecord::Event {
                    event_type: "permission_request",
                    data: serde_json::json!({"title": title}),
                });
                self.last_tool_line = None;
                io::stdout().flush()?;
                return permission_selection(&options);
            }
            StreamEvent::PermissionDecision { title, allowed } => {
                self.flush_assistant();
                render_permission_decision(self.terminal, &title, allowed);
                self.records.push(DisplayRecord::Event {
                    event_type: "permission_decision",
                    data: serde_json::json!({"title": title, "allowed": allowed}),
                });
                self.last_tool_line = None;
            }
            StreamEvent::SystemError(_) => {}
        }
        io::stdout().flush()?;
        Ok(None)
    }

    fn flush_reasoning(&mut self) {
        if self.reasoning_open {
            render_reasoning(self.terminal, &self.reasoning);
            self.records.push(DisplayRecord::Event {
                event_type: "reasoning",
                data: serde_json::json!({"content": self.reasoning}),
            });
            self.reasoning.clear();
            self.reasoning_open = false;
        }
    }

    fn finish(&mut self, log: &mut SessionLog) -> Result<()> {
        self.flush_reasoning();
        self.flush_assistant();
        for record in &self.records {
            match record {
                DisplayRecord::Event { event_type, data } => {
                    log.event(event_type, data.clone())?;
                }
                DisplayRecord::ToolCall(call_id) => {
                    let tool = &self.tools[call_id];
                    log.event(
                        "tool_call",
                        serde_json::json!({
                            "id": call_id,
                            "title": tool.title,
                            "kind": tool.kind,
                            "input": tool.input
                        }),
                    )?;
                }
                DisplayRecord::ToolResult(call_id) => {
                    let tool = &self.tools[call_id];
                    log.event(
                        "tool_result",
                        serde_json::json!({
                            "id": call_id,
                            "status": tool.status,
                            "output": tool.output
                        }),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn flush_assistant(&mut self) {
        if self.assistant.is_empty() {
            return;
        }
        print_markdown(self.terminal, &self.assistant);
        self.assistant.clear();
    }

    fn flush_complete_assistant_blocks(&mut self) {
        let end = complete_markdown_prefix(&self.assistant);
        if end == 0 {
            return;
        }
        let complete = self.assistant.drain(..end).collect::<String>();
        print_markdown(self.terminal, &complete);
    }
}

enum DisplayRecord {
    Event {
        event_type: &'static str,
        data: serde_json::Value,
    },
    ToolCall(String),
    ToolResult(String),
}

fn permission_selection(options: &[PermissionOption]) -> Result<Option<String>> {
    let mut choices = options
        .iter()
        .map(|option| PermissionChoice {
            id: Some(option.id.clone()),
            kind: option.kind.clone(),
            label: crate::markdown::sanitize(&option.label),
        })
        .collect::<Vec<_>>();
    choices.sort_by_key(|choice| permission_rank(&choice.kind));
    choices.insert(
        0,
        PermissionChoice {
            id: None,
            kind: "cancel".into(),
            label: "Cancel".into(),
        },
    );
    Ok(Select::new("Permission", choices).prompt()?.id)
}

struct PermissionChoice {
    id: Option<String>,
    kind: String,
    label: String,
}

impl std::fmt::Display for PermissionChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.label)
    }
}

fn permission_rank(kind: &str) -> u8 {
    match kind {
        "reject_once" => 0,
        "reject_always" => 1,
        "allow_once" => 2,
        "allow_always" => 3,
        _ => 4,
    }
}

fn complete_markdown_prefix(source: &str) -> usize {
    let mut fence = None;
    let mut offset = 0;
    let mut complete = 0;

    for line in source.split_inclusive('\n') {
        offset += line.len();
        let trimmed = line.trim_start();
        let marker = trimmed
            .chars()
            .next()
            .filter(|character| matches!(character, '`' | '~'));
        let marker_length = marker.map_or(0, |marker| {
            trimmed
                .chars()
                .take_while(|character| *character == marker)
                .count()
        });
        if marker_length >= 3 {
            match fence {
                Some((open_marker, open_length))
                    if marker == Some(open_marker) && marker_length >= open_length =>
                {
                    fence = None;
                    complete = offset;
                }
                None => fence = marker.map(|marker| (marker, marker_length)),
                _ => {}
            }
        } else if fence.is_none() && line.trim().is_empty() {
            complete = offset;
        }
    }
    complete
}

struct ToolView {
    title: String,
    kind: Option<String>,
    status: Option<String>,
    input: Option<serde_json::Value>,
    output: Option<serde_json::Value>,
    completed_displayed: bool,
    output_displayed: bool,
}

fn merge_optional_value(target: &mut Option<serde_json::Value>, update: Option<serde_json::Value>) {
    let Some(update) = update else {
        return;
    };
    match target {
        Some(target) => merge_value(target, update),
        None => *target = Some(update),
    }
}

fn merge_value(target: &mut serde_json::Value, update: serde_json::Value) {
    match (target, update) {
        (serde_json::Value::Object(target), serde_json::Value::Object(update)) => {
            for (key, value) in update {
                match target.get_mut(&key) {
                    Some(target) => merge_value(target, value),
                    None => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, update) => *target = update,
    }
}

fn is_terminal_tool_status(status: Option<&str>) -> bool {
    matches!(status, Some("completed" | "failed"))
}

fn normalize_reasoning(reasoning: &str) -> String {
    reasoning
        .trim()
        .lines()
        .map(|line| {
            let line = line.trim();
            line.strip_prefix("**")
                .and_then(|line| line.strip_suffix("**"))
                .unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn print_colored(terminal: bool, color: &str, text: &str) {
    let text = crate::markdown::sanitize(text);
    if terminal {
        print!("\x1b[{color}m{text}\x1b[0m");
    } else {
        print!("{text}");
    }
}

fn print_markdown(terminal: bool, markdown: &str) {
    print!("{}", crate::markdown::render(markdown, terminal));
}

fn print_value(terminal: bool, value: &serde_json::Value) {
    let text = display_value(value);
    for line in text.lines() {
        print_colored(terminal, LIGHT_GRAY, &format!("  {line}\n"));
    }
}

fn render_reasoning(terminal: bool, reasoning: &str) {
    let reasoning = normalize_reasoning(reasoning);
    if !reasoning.is_empty() {
        print_colored(terminal, DARK_GRAY, &format!("◇ {reasoning}\n"));
    }
}

fn render_tool(
    terminal: bool,
    title: &str,
    status: Option<&str>,
    output: Option<&serde_json::Value>,
) {
    let (symbol, color) = match status {
        Some("failed") => ("✗", "31"),
        Some("completed") => ("✓", "32"),
        _ => ("○", LIGHT_GRAY),
    };
    print_colored(terminal, color, symbol);
    print_colored(terminal, LIGHT_GRAY, &format!(" {title}\n"));
    if let Some(output) = output.filter(|output| !output.is_null()) {
        print_value(terminal, output);
    }
}

fn render_permission_request(terminal: bool, title: &str) {
    print_colored(terminal, "33", &format!("? {title}\n"));
}

fn render_permission_decision(terminal: bool, title: &str, allowed: bool) {
    let (symbol, color) = if allowed {
        ("✓", "32")
    } else {
        ("⊘", "31")
    };
    print_colored(terminal, color, symbol);
    print_colored(terminal, LIGHT_GRAY, &format!(" {title}\n"));
}

fn display_value(value: &serde_json::Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    if let Some(content) = value.get("content").and_then(serde_json::Value::as_array) {
        let text = content
            .iter()
            .filter_map(|block| {
                block
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| block.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            return text;
        }
    }
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn render_history(entries: &[SessionEntry]) {
    let terminal = io::stdout().is_terminal();
    let tool_results = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::Event { event_type, data } if event_type == "tool_result" => {
                Some((data.get("id")?.as_str()?.to_owned(), data))
            }
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    for entry in entries {
        match entry {
            SessionEntry::Message(message) if message.role == "user" => {
                render_historical_user_prompt(&message.content)
            }
            SessionEntry::Message(message) if message.role == "assistant" => {
                print_markdown(terminal, &message.content);
            }
            SessionEntry::Event { event_type, data } if event_type == "reasoning" => {
                let content = data
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                render_reasoning(terminal, content);
            }
            SessionEntry::Event { event_type, data } if event_type == "tool_call" => {
                let id = data
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let title = data
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Tool call");
                let result = tool_results.get(id);
                let status = result
                    .and_then(|value| value.get("status"))
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| result.map(|_| "completed"));
                let output = result.and_then(|value| value.get("output"));
                render_tool(terminal, title, status, output);
            }
            SessionEntry::Event { event_type, .. } if event_type == "tool_result" => {}
            SessionEntry::Event { event_type, data } if event_type == "permission_request" => {
                let title = data
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Permission required");
                render_permission_request(terminal, title);
            }
            SessionEntry::Event { event_type, data } if event_type == "permission_decision" => {
                let allowed =
                    data.get("allowed").and_then(serde_json::Value::as_bool) == Some(true);
                let title = data
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Permission");
                render_permission_decision(terminal, title, allowed);
            }
            _ => {}
        }
    }
}

fn render_historical_user_prompt(prompt: &str) {
    let prompt = crate::markdown::sanitize(prompt);
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        println!("| \x1b[1m{prompt}\x1b[0m");
    } else {
        println!("| {prompt}");
    }
}

fn header(name: &str, harness: &str, model: &str, thinking: &str) -> String {
    format!(
        "{HEADER_RULE}\n{name}\n{HEADER_RULE}\nharness: {harness}\nmodel: {model} ({thinking})\n\n(/quit or Ctrl-D to exit)\n{HEADER_RULE}"
    )
}

async fn apply_selection(
    client: &mut Client,
    kind: ConfigKind,
    selection: Option<&str>,
) -> Result<()> {
    if let Some(selection) = selection {
        if !client.set_config(kind, selection).await? {
            let name = match kind {
                ConfigKind::Model => "model",
                ConfigKind::Thinking => "thinking level",
            };
            eprintln!("warning: saved {name} {selection:?} is unavailable; using the default");
        }
    }
    Ok(())
}

fn render_user_prompt(prompt: &str) {
    let prompt = crate::markdown::sanitize(prompt);
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        print!("\x1b[1A\r\x1b[2K| \x1b[1m{prompt}\x1b[0m\n");
    } else {
        println!("| {prompt}");
    }
}

fn runtime_instruction(soul: &str) -> String {
    let now = Local::now();
    let current_time = format!(
        "{} {} {} {}. {}",
        now.format("%a"),
        now.format("%b"),
        now.day(),
        now.year(),
        now.format("%H:%M")
    );
    format!(
        "{}\n\nUse `workspace/` as your working directory. Create and modify working files only inside this directory unless the user explicitly asks you to use another location.\n\nCurrent Date and Time: {current_time}",
        soul.trim_end()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_chat_header() {
        assert_eq!(
            header("Research Agent", "codex", "gpt-5.6", "high"),
            "------------------------------------\nResearch Agent\n------------------------------------\nharness: codex\nmodel: gpt-5.6 (high)\n\n(/quit or Ctrl-D to exit)\n------------------------------------"
        );
    }

    #[test]
    fn runtime_instruction_contains_soul_workspace_rule_and_current_time() {
        let instruction = runtime_instruction("You are a researcher.\n");
        assert!(instruction
            .starts_with("You are a researcher.\n\nUse `workspace/` as your working directory."));
        assert!(instruction.contains("\n\nCurrent Date and Time: "));
    }

    #[test]
    fn normalizes_reasoning_for_terminal_output() {
        assert_eq!(
            normalize_reasoning("\n\n**Planning to infer timezone**\n"),
            "Planning to infer timezone"
        );
    }

    #[test]
    fn merges_partial_tool_updates() {
        let mut value = Some(serde_json::json!({"query": "weather"}));
        merge_optional_value(
            &mut value,
            Some(serde_json::json!({"location": "Tashkent"})),
        );
        assert_eq!(
            value,
            Some(serde_json::json!({
                "query": "weather",
                "location": "Tashkent"
            }))
        );
    }

    #[test]
    fn streams_only_complete_markdown_blocks() {
        assert_eq!(complete_markdown_prefix("First paragraph\n\nSecond"), 17);
        assert_eq!(complete_markdown_prefix("Still writing"), 0);
        assert_eq!(
            complete_markdown_prefix("```text\none\n\ntwo\n```\nrest"),
            21
        );
    }

    #[test]
    fn turns_usage_limit_errors_into_a_short_message() {
        let error = anyhow::anyhow!("ACP error -32603: Internal error");
        let system_error = "[SYSTEM_ERROR] Internal error\nYou've hit your usage limit. Upgrade or try again at 7:35 PM.";
        assert_eq!(
            crate::acp::friendly_error("Codex", &error, system_error, &[]),
            "Usage limit reached. Try again at 7:35 PM."
        );
    }

    #[test]
    fn hides_unknown_acp_details() {
        let error = anyhow::anyhow!("ACP error -32603: Internal error");
        assert_eq!(
            crate::acp::friendly_error(
                "Claude Code",
                &error,
                "stack trace",
                &["adapter details".into()]
            ),
            "Claude Code could not complete the request. Try again."
        );
    }
}
