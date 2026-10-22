use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::time::Instant;

use anyhow::Result;
use unicode_width::UnicodeWidthChar;

use super::input::DARK_GRAY;
use crate::acp::{PermissionOption, StreamEvent, ToolStatus};
use crate::session::{Role, SessionEntry};

pub(crate) const LIGHT_GRAY: &str = "38;5;250";

pub(crate) struct TurnDisplay {
    terminal: bool,
    reasoning_open: bool,
    reasoning: crate::markdown::StreamBuffer,
    waiting: Option<indicatif::ProgressBar>,
    waiting_since: Instant,
    assistant: crate::markdown::StreamBuffer,
    assistant_started: bool,
    tools: HashMap<String, ToolView>,
    replay: bool,
}

impl TurnDisplay {
    pub(crate) fn new() -> Self {
        Self {
            terminal: interactive(),
            reasoning_open: false,
            reasoning: crate::markdown::StreamBuffer::default(),
            waiting: None,
            waiting_since: Instant::now(),
            assistant: crate::markdown::StreamBuffer::default(),
            assistant_started: false,
            tools: HashMap::new(),
            replay: false,
        }
    }

    pub(crate) fn handle(
        &mut self,
        event: StreamEvent,
        select_permission: impl FnOnce(&str, &[PermissionOption], &str) -> Result<Option<String>>,
    ) -> Result<Option<String>> {
        match event {
            StreamEvent::Assistant(text) => {
                self.flush_reasoning();
                if self.terminal {
                    let complete = self.assistant.push(&text, true);
                    if complete.is_empty() {
                        self.start_waiting();
                    }
                    self.print_assistant(&complete);
                } else {
                    self.print_assistant(&crate::markdown::sanitize(&text));
                }
            }
            StreamEvent::Reasoning(text) => {
                self.flush_assistant();
                let complete = self.reasoning.push(&text, false);
                self.print_reasoning(&complete);
            }
            StreamEvent::Tool {
                call_id,
                title,
                status,
                output,
                input,
                kind,
            } => {
                self.stop_waiting();
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
                        status: None,
                        input: None,
                        kind: None,
                        output: None,
                        started: Instant::now(),
                        completed_displayed: false,
                        output_displayed: false,
                    });
                tool.title.clone_from(&title);
                if status == Some(ToolStatus::InProgress) && tool.status != status {
                    tool.started = Instant::now();
                }
                if status.is_some() {
                    tool.status = status;
                }
                merge_optional_value(&mut tool.input, input);
                if kind.is_some() {
                    tool.kind = kind;
                }
                let title = tool_title(&tool.title, tool.kind.as_deref(), tool.input.as_ref());
                merge_optional_value(&mut tool.output, output);
                if first && !self.terminal && !self.replay {
                    render_tool(false, &title, None, None);
                }
                if matches!(
                    tool.status,
                    Some(ToolStatus::Completed | ToolStatus::Failed)
                ) && !tool.completed_displayed
                {
                    render_tool(self.terminal, &title, tool.status, tool.output.as_ref());
                    tool.output_displayed = tool
                        .output
                        .as_ref()
                        .is_some_and(|output| !display_value(output).trim().is_empty());
                    tool.completed_displayed = true;
                    self.waiting_since = Instant::now();
                } else if tool.completed_displayed && !tool.output_displayed {
                    if let Some(output) = &tool.output {
                        let text = display_value(output);
                        print_preview(self.terminal, &text);
                        tool.output_displayed = !text.trim().is_empty();
                    }
                }
                self.start_waiting();
            }
            StreamEvent::PermissionRequest {
                title,
                tool_call,
                options,
                cancelled,
            } => {
                self.stop_waiting();
                self.flush_reasoning();
                self.flush_assistant();
                if self.replay || cancelled || !self.terminal {
                    render_permission_request(self.terminal, &title);
                }
                let details = tool_call
                    .as_ref()
                    .map(permission_details)
                    .unwrap_or_default();
                if !details.is_empty() {
                    print_preview(self.terminal, &details);
                }
                io::stdout().flush()?;
                return if self.replay || cancelled || !self.terminal {
                    Ok(None)
                } else {
                    select_permission(&title, &options, &details)
                };
            }
            StreamEvent::PermissionDecision { title, allowed } => {
                self.stop_waiting();
                self.flush_reasoning();
                self.flush_assistant();
                render_permission_decision(self.terminal, &title, allowed);
                self.start_waiting();
            }
            StreamEvent::SystemError(_) => {}
        }
        io::stdout().flush()?;
        Ok(None)
    }

    pub(crate) fn start_waiting(&mut self) {
        if self.terminal && !self.replay && !self.assistant_started && self.waiting.is_none() {
            let mut active = self
                .tools
                .iter()
                .filter(|(_, tool)| {
                    !matches!(
                        tool.status,
                        Some(ToolStatus::Completed | ToolStatus::Failed)
                    )
                })
                .collect::<Vec<_>>();
            active.sort_by_key(|(id, _)| *id);
            let message = if active.is_empty() {
                "Thinking".to_owned()
            } else {
                compact_line(
                    &active
                        .iter()
                        .map(|(_, tool)| {
                            tool_title(&tool.title, tool.kind.as_deref(), tool.input.as_ref())
                        })
                        .collect::<Vec<_>>()
                        .join(" · "),
                    display_width(),
                )
            };
            let _ = io::stdout().flush();
            let spinner = crate::progress::start(message);
            let template = if active.is_empty() {
                "\x1b[38;5;250m{spinner} {msg} for {elapsed_short} ...\x1b[0m"
            } else {
                "\x1b[38;5;250m○ {msg} · {elapsed_short}\x1b[0m"
            };
            spinner.set_elapsed(
                active
                    .iter()
                    .map(|(_, tool)| tool.started)
                    .min()
                    .unwrap_or(self.waiting_since)
                    .elapsed(),
            );
            spinner.set_style(
                indicatif::ProgressStyle::with_template(template)
                    .expect("the static spinner template must be valid")
                    .with_key(
                        "elapsed_short",
                        |state: &indicatif::ProgressState, out: &mut dyn std::fmt::Write| {
                            let seconds = state.elapsed().as_secs();
                            if seconds >= 3600 {
                                let _ = write!(out, "{}h ", seconds / 3600);
                            }
                            if seconds >= 60 {
                                let _ = write!(out, "{}m ", seconds / 60 % 60);
                            }
                            let _ = write!(out, "{}s", seconds % 60);
                        },
                    ),
            );
            self.waiting = Some(spinner);
        }
    }

    fn stop_waiting(&mut self) {
        if let Some(spinner) = self.waiting.take() {
            spinner.finish_and_clear();
        }
    }

    fn print_reasoning(&mut self, text: &str) {
        let text = crate::markdown::render_plain(text);
        let text = if self.reasoning_open {
            text.as_str()
        } else {
            text.trim_start()
        };
        if text.is_empty() {
            return;
        }
        self.stop_waiting();
        if !self.reasoning_open {
            print_colored(self.terminal, DARK_GRAY, "◇ ");
        }
        print_colored(self.terminal, DARK_GRAY, text);
        self.reasoning_open = !text.ends_with('\n');
    }

    fn flush_reasoning(&mut self) {
        let text = self.reasoning.finish(false);
        self.print_reasoning(&text);
        if self.reasoning_open {
            print_colored(self.terminal, DARK_GRAY, "\n");
            self.reasoning_open = false;
        }
    }

    pub(crate) fn finish(&mut self) {
        self.stop_waiting();
        self.flush_reasoning();
        self.flush_assistant();
    }

    fn flush_assistant(&mut self) {
        let text = self.assistant.finish(self.terminal);
        self.print_assistant(&text);
        self.assistant_started = false;
    }

    fn print_assistant(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.stop_waiting();
        self.assistant_started = true;
        self.waiting_since = Instant::now();
        if self.replay && !self.terminal {
            eprint!("{text}");
        } else {
            print!("{text}");
        }
    }
}

struct ToolView {
    started: Instant,
    title: String,
    kind: Option<String>,
    input: Option<serde_json::Value>,
    status: Option<ToolStatus>,
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

pub(crate) fn interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

pub(crate) fn print_colored(terminal: bool, color: &str, text: &str) {
    let text = crate::markdown::sanitize(text);
    if terminal {
        print!("\x1b[{color}m{text}\x1b[0m");
    } else {
        eprint!("{text}");
    }
}

pub(crate) fn display_width() -> usize {
    crossterm::terminal::size()
        .map(|(columns, _)| usize::from(columns).saturating_sub(4).clamp(10, 100))
        .unwrap_or(76)
}

pub(crate) fn compact_line(text: &str, width: usize) -> String {
    let text = crate::markdown::sanitize(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    shorten_line(&text, width).0
}

fn shorten_line(text: &str, width: usize) -> (String, bool) {
    let mut line = String::new();
    let mut used = 0;
    for character in text.chars() {
        let count = character.width().unwrap_or(0);
        if used + count > width.saturating_sub(1) {
            line.push('…');
            return (line, true);
        }
        line.push(character);
        used += count;
    }
    (line, false)
}

fn preview(text: &str, width: usize) -> (Vec<String>, bool) {
    let text = crate::markdown::sanitize(text).replace('\t', "    ");
    let mut lines = text.lines();
    let mut truncated = false;
    let preview = lines
        .by_ref()
        .take(5)
        .map(|line| {
            let (line, cut) = shorten_line(line, width);
            truncated |= cut;
            line
        })
        .collect();
    truncated |= lines.next().is_some();
    (preview, truncated)
}

fn print_preview(terminal: bool, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    let (lines, truncated) = preview(text, display_width());
    for line in lines {
        print_colored(terminal, LIGHT_GRAY, &format!("  {line}\n"));
    }
    if truncated {
        print_colored(terminal, DARK_GRAY, "  ...\n");
    }
}

fn permission_details(tool_call: &serde_json::Value) -> String {
    let mut details = Vec::new();
    for (key, label) in [
        ("rawInput", "Input"),
        ("locations", "Files"),
        ("content", "Change"),
    ] {
        if let Some(value) = tool_call.get(key).filter(|value| !value.is_null()) {
            details.push(format!("{label}:\n{}", display_value(value)));
        }
    }
    details.join("\n")
}

pub(crate) fn detail_lines(text: &str, width: usize) -> Vec<String> {
    let text = crate::markdown::sanitize(text).replace('\t', "    ");
    let mut lines = Vec::new();
    for source in text.lines() {
        let mut line = String::new();
        let mut used = 0;
        for character in source.chars() {
            let count = character.width().unwrap_or(0);
            if used + count > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push(character);
            used += count;
        }
        lines.push(line);
    }
    lines
}

fn render_tool(
    terminal: bool,
    title: &str,
    status: Option<ToolStatus>,
    output: Option<&serde_json::Value>,
) {
    let (symbol, color) = match status {
        Some(ToolStatus::Failed) => ("✗ ", "31"),
        Some(ToolStatus::Completed) => ("✓ ", "32"),
        _ => ("○ ", LIGHT_GRAY),
    };
    print_colored(terminal, color, symbol);
    let title = compact_line(title, display_width());
    print_colored(terminal, LIGHT_GRAY, &format!("{title}\n"));
    if let Some(output) = output.filter(|output| !output.is_null()) {
        print_preview(terminal, &display_value(output));
    }
}

fn render_permission_request(terminal: bool, title: &str) {
    let title = compact_line(title, display_width());
    print_colored(terminal, "33", &format!("? {title}\n"));
}

fn render_permission_decision(terminal: bool, title: &str, allowed: bool) {
    let (symbol, color) = if allowed {
        ("✓", "32")
    } else {
        ("⊘", "31")
    };
    print_colored(terminal, color, symbol);
    let title = compact_line(title, display_width());
    print_colored(terminal, LIGHT_GRAY, &format!(" {title}\n"));
}

fn tool_title(title: &str, kind: Option<&str>, input: Option<&serde_json::Value>) -> String {
    let command = input.and_then(|value| {
        value
            .get("command")
            .or_else(|| value.get("cmd"))
            .and_then(serde_json::Value::as_str)
    });
    let name = match kind {
        Some("execute") => "bash".to_owned(),
        Some(name @ ("read" | "edit" | "delete" | "search" | "fetch")) => name.to_owned(),
        _ if command.is_some() && title.contains(char::is_whitespace) => "bash".to_owned(),
        _ => title.to_lowercase(),
    };
    let argument = command
        .or_else(|| {
            input.and_then(|value| {
                ["path", "file_path", "pattern", "query", "url"]
                    .iter()
                    .find_map(|key| value.get(key).and_then(serde_json::Value::as_str))
            })
        })
        .or_else(|| (name != title.to_lowercase()).then_some(title));
    match argument {
        Some(argument) => format!("{name}: {argument}"),
        None => format!("{name}:"),
    }
}

fn display_value(value: &serde_json::Value) -> String {
    use serde_json::Value;

    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(display_value)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(fields) => {
            for key in ["formatted_output", "stdout", "stderr", "text", "content"] {
                if fields.contains_key(key) {
                    let keys = if matches!(key, "stdout" | "stderr") {
                        vec!["stdout", "stderr"]
                    } else {
                        vec![key]
                    };
                    let mut text = keys
                        .iter()
                        .filter_map(|key| fields.get(*key))
                        .map(display_value)
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    if let Some(code) = fields
                        .get("exit_code")
                        .filter(|code| code.as_i64() != Some(0))
                    {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&format!("exit code: {code}"));
                    }
                    return text;
                }
            }
            fields
                .iter()
                .map(|(key, value)| format!("{key}: {}", display_value(value)))
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => value.to_string(),
    }
}

pub(crate) fn render_history(entries: Vec<SessionEntry>) -> Result<()> {
    let mut display = TurnDisplay::new();
    display.replay = true;
    for entry in entries {
        match entry {
            SessionEntry::Message(message) if message.role == Role::User => {
                display.finish();
                display.tools.clear();
                render_historical_user_prompt(&message.content);
            }
            SessionEntry::Message(message) if message.role == Role::Assistant => {
                display.handle(StreamEvent::Assistant(message.content), |_, _, _| Ok(None))?;
                display.flush_assistant();
            }
            SessionEntry::Message(_) => {}
            SessionEntry::Stream { event } => {
                display.handle(event, |_, _, _| Ok(None))?;
            }
            SessionEntry::TurnEnd => display.finish(),
            SessionEntry::Error { message, .. } => {
                display.finish();
                print_colored(display.terminal, "31", &format!("✗ {message}\n"));
            }
        }
    }
    display.finish();
    Ok(())
}

fn render_historical_user_prompt(prompt: &str) {
    let prompt = crate::markdown::sanitize(prompt);
    if interactive() {
        println!("\x1b[0;97m| \x1b[0;1;97m{prompt}\x1b[0m");
    } else {
        eprintln!("| {prompt}");
    }
}

pub(crate) fn render_user_prompt(prompt: &str) {
    if !interactive() {
        eprintln!("| {}", crate::markdown::sanitize(prompt));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_previews_limit_rows_and_display_width() {
        let text = format!("{}\n{}", "界".repeat(1000), "more\n".repeat(100));
        let (lines, truncated) = preview(&text, 40);
        assert_eq!(lines.len(), 5);
        assert!(truncated);
        assert!(lines.iter().all(|line| line
            .chars()
            .map(|ch| ch.width().unwrap_or(0))
            .sum::<usize>()
            <= 40));
        assert_eq!(
            preview("short\nresult", 40),
            (vec!["short".into(), "result".into()], false)
        );
    }

    #[test]
    fn tool_output_unwraps_text_and_preserves_failures() {
        assert_eq!(
            display_value(&serde_json::json!([
                {"content": {"text": "Tashkent: sunny", "type": "text"}}
            ])),
            "Tashkent: sunny"
        );
        assert_eq!(
            display_value(&serde_json::json!({
                "formatted_output": "permission denied", "exit_code": 1
            })),
            "permission denied\nexit code: 1"
        );
        assert_eq!(
            display_value(&serde_json::json!({
                "stdout": "out", "stderr": "error", "exit_code": 0
            })),
            "out\nerror"
        );
        assert_eq!(
            tool_title(
                "bash",
                Some("execute"),
                Some(&serde_json::json!({
                    "command": "uname -a"
                }))
            ),
            "bash: uname -a"
        );
        assert_eq!(
            tool_title(
                "read",
                Some("read"),
                Some(&serde_json::json!({
                    "path": "notes.txt"
                }))
            ),
            "read: notes.txt"
        );
    }

    #[test]
    fn permission_details_keep_commands_paths_and_changes() {
        let call = serde_json::json!({
            "rawInput": {"command": "cat workspace/file"},
            "locations": [{"path": "workspace/file"}],
            "content": [{"type": "diff", "oldText": "before", "newText": "after"}]
        });
        let details = permission_details(&call);
        assert!(details.contains("cat workspace/file"));
        assert!(details.contains("before"));
        assert!(details.contains("after"));
        assert_eq!(detail_lines("abcdefgh界界", 4).concat(), "abcdefgh界界");
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
}
