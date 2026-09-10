#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Chat {
    child: Child,
    output: mpsc::Receiver<u8>,
    status: mpsc::Receiver<u8>,
    text: String,
    status_text: String,
}

impl Chat {
    fn start(root: &std::path::Path, resume: bool) -> Self {
        Self::start_with_args(root, resume, &[])
    }

    fn start_with_args(root: &std::path::Path, resume: bool, args: &[&str]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lazyagents"));
        command.arg("chat").args(args);
        if resume {
            command.args(["--resume", "test"]);
        }
        let mut child = command
            .current_dir(root)
            .env("HOME", root)
            .env("XDG_DATA_HOME", root.join("data"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    root.join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let output = read_output(child.stdout.take().unwrap());
        let status = read_output(child.stderr.take().unwrap());
        Self {
            child,
            output,
            status,
            text: String::new(),
            status_text: String::new(),
        }
    }

    fn wait_for(&mut self, text: &str) {
        wait_for(&self.output, &mut self.text, text);
    }

    fn wait_status(&mut self, text: &str) {
        wait_for(&self.status, &mut self.status_text, text);
    }

    fn send(&mut self, text: &str) {
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }

    fn wait_for_exit(&mut self) {
        self.wait_for_status(true);
    }

    fn wait_for_status(&mut self, success: bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(status.success(), success, "{status}");
                return;
            }
            assert!(
                Instant::now() < deadline,
                "chat did not exit with stdin still open"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Chat {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_output(mut stream: impl Read + Send + 'static) -> mpsc::Receiver<u8> {
    let (sender, output) = mpsc::channel();
    std::thread::spawn(move || {
        let mut byte = [0];
        while stream.read(&mut byte).unwrap_or(0) != 0 {
            if sender.send(byte[0]).is_err() {
                break;
            }
        }
    });
    output
}

fn wait_for(output: &mpsc::Receiver<u8>, captured: &mut String, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !captured.ends_with(text) {
        let byte = output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|error| panic!("waiting for {text:?}: {error}; output: {captured}"));
        captured.push(char::from(byte));
    }
}

fn agent() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join(".agents")).unwrap();
    fs::create_dir(root.path().join("bin")).unwrap();
    fs::write(root.path().join(".agents/agent.json"), r#"{"name":"Test","slug":"test","description":"Test","harness":"opencode","model":null,"thinking":null}"#).unwrap();
    fs::write(root.path().join(".agents/SOUL.md"), "Test").unwrap();
    let adapter = root.path().join("bin/opencode");
    fs::write(&adapter, r#"#!/bin/sh
prompts=0
while IFS= read -r line; do
  printf '%s\n' "$line" >> requests.jsonl
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"agentCapabilities":{"sessionCapabilities":{"resume":{}}}}}'
      ;;
    *'"method":"session/new"'*|*'"method":"session/resume"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"test","configOptions":[{"id":"model","type":"select","category":"model","currentValue":"fast","options":[{"value":"fast","name":"Fast"},{"value":"deep","name":"Deep"}]},{"id":"effort","type":"select","category":"thought_level","currentValue":"low","options":[{"value":"low","name":"Low"},{"value":"high","name":"High"}]}]}}'
      ;;
    *'"method":"session/set_config_option"'*)
      request_id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$request_id"
      ;;
    *'"method":"session/prompt"'*)
      prompts=$((prompts + 1))
      response_id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
      case "$line" in
        *'"text":"Permission"'*)
          printf '%s\n' '{"jsonrpc":"2.0","id":78,"method":"session/request_permission","params":{"toolCall":{"title":"bash","kind":"execute","rawInput":{"command":"pwd"}},"options":[{"optionId":"allow","name":"Allow once","kind":"allow_once"},{"optionId":"always","name":"Always allow bash","kind":"allow_always"}]}}'
          IFS= read -r permission
          printf '%s\n' "$permission" >> requests.jsonl
          ;;
        *'"text":"Wait"'*)
          printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"text":"Waiting\n\n"}}}}'
          continue
          ;;
        *'"text":"Fail"'*)
          printf '%s\n' '{"jsonrpc":"2.0","id":3,"error":{"code":-32603,"message":"test configuration failure"}}'
          continue
          ;;
      esac
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"text":"Before\n\n"}}}}'
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"1","title":"Read file","status":"pending"}}}'
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"1","status":"completed","rawOutput":"File contents"}}}'
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"text":"After\n\n"}}}}'
      printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$response_id"
      ;;
    *'"method":"session/cancel"'*)
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"late","title":"Late result","status":"completed","rawOutput":"Saved before stopping"}}}'
      printf '%s\n' '{"jsonrpc":"2.0","id":77,"method":"session/request_permission","params":{"toolCall":{"title":"Late permission"},"options":[{"optionId":"allow","name":"Allow once","kind":"allow_once"}]}}'
      IFS= read -r permission
      case "$permission" in
        *'"outcome":"cancelled"'*) ;;
        *) exit 2 ;;
      esac
      printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"cancelled"}}\n' "$response_id"
      ;;
    *'"method":"session/close"'*)
      exit 0
      ;;
  esac
done
"#).unwrap();
    fs::set_permissions(adapter, fs::Permissions::from_mode(0o755)).unwrap();
    root
}

#[test]
fn ctrl_c_exits_with_stdin_open() {
    let root = agent();
    let mut chat = Chat::start(root.path(), false);
    chat.wait_status("> ");
    std::thread::sleep(Duration::from_millis(50));
    // The signal handler must release the process without another stdin read.
    unsafe {
        libc::kill(chat.child.id() as libc::pid_t, libc::SIGINT);
    }
    chat.wait_for_exit();
}

#[test]
fn replays_assistant_and_tool_events_in_original_order() {
    let root = agent();
    let mut chat = Chat::start(root.path(), false);
    chat.wait_status("> ");
    chat.send("Hello\n");
    chat.wait_for("After\n\n");
    chat.wait_status("> ");
    assert_eq!(chat.text, "Before\n\nAfter\n\n");
    chat.send("/quit\n");
    chat.wait_for_exit();

    let mut resumed = Chat::start(root.path(), true);
    resumed.wait_status("> ");
    let before = resumed.status_text.find("Before").unwrap();
    let tool = resumed.status_text.find("read file:").unwrap();
    let after = resumed.status_text.find("After").unwrap();
    assert!(before < tool && tool < after, "{}", resumed.status_text);
    assert_eq!(resumed.status_text.matches("Before").count(), 1);
    resumed.send("/quit\n");
    resumed.wait_for_exit();
    assert!(resumed.output.recv_timeout(Duration::from_secs(1)).is_err());
}

#[test]
fn preserves_protocol_error_details_in_the_session_log() {
    let root = agent();
    let mut chat = Chat::start(root.path(), false);
    chat.wait_status("> ");
    chat.send("Fail\nHello\n");
    chat.wait_for_status(false);
    let log = fs::read_dir(root.path().join(".agents/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let contents = fs::read_to_string(log).unwrap();
    assert!(!contents.contains("Hello"));
    let error = contents
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|record| record["type"] == "error")
        .unwrap();
    assert!(error["detail"]
        .as_str()
        .unwrap()
        .contains("ACP error -32603: test configuration failure"));
    assert_eq!(
        error["message"],
        "OpenCode could not complete the request. Try again."
    );
}

#[test]
fn ctrl_c_stops_the_turn_and_accepts_another_prompt() {
    let root = agent();
    let mut chat = Chat::start(root.path(), false);
    chat.wait_status("> ");
    chat.send("Wait\n");
    chat.wait_for("Waiting\n\n");
    std::thread::sleep(Duration::from_millis(50));
    unsafe {
        libc::kill(chat.child.id() as libc::pid_t, libc::SIGINT);
    }
    chat.wait_status("Turn cancelled.\n");
    chat.wait_status("> ");
    let log = fs::read_dir(root.path().join(".agents/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let contents = fs::read_to_string(log).unwrap();
    assert!(contents.contains("Saved before stopping"));
    assert!(contents.contains("\"cancelled\":true"));
    chat.send("Continue\n");
    chat.wait_for("After\n\n");
    chat.wait_status("> ");
    chat.send("/quit\n");
    chat.wait_for_exit();
}

#[test]
fn piped_timeout_returns_failure_after_preserving_late_events() {
    let root = agent();
    let path = root.path().join(".agents/agent.json");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["idle_timeout"] = 1.into();
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
    let mut chat = Chat::start(root.path(), false);
    chat.wait_status("> ");
    chat.send("Wait\nHello\n");
    chat.wait_for_status(false);
    let path = fs::read_dir(root.path().join(".agents/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let log = fs::read_to_string(path).unwrap();
    assert!(log.contains("Saved before stopping"));
    assert!(!log.contains("Hello"));
}

#[test]
fn only_new_sessions_apply_agent_model_and_thinking_settings() {
    for (resume, model, thinking) in [
        (false, "deep", "high"),
        (true, "deep", "high"),
        (false, "fast", "low"),
    ] {
        let root = agent();
        let config_path = root.path().join(".agents/agent.json");
        let mut config: serde_json::Value =
            serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        config["model"] = model.into();
        config["thinking"] = thinking.into();
        fs::write(config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        if resume {
            fs::create_dir(root.path().join(".agents/sessions")).unwrap();
            fs::write(root.path().join(".agents/sessions/01.jsonl"),
                "{\"acp_session_id\":\"test\",\"harness\":\"opencode\"}\n{\"type\":\"message\",\"role\":\"user\",\"content\":\"Old prompt\"}\n").unwrap();
        }
        let mut chat = Chat::start(root.path(), resume);
        chat.wait_status("> ");
        chat.send("/quit\n");
        chat.wait_for_exit();
        let calls = fs::read_to_string(root.path().join("requests.jsonl")).unwrap();
        let settings: Vec<serde_json::Value> = calls
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|call| call["method"] == "session/set_config_option")
            .collect();
        if resume || model == "fast" {
            assert!(settings.is_empty());
            assert!(chat.status_text.contains("model: fast (Low)"));
        } else {
            assert_eq!(settings.len(), 2);
            assert_eq!(settings[0]["params"]["value"], "deep");
            assert_eq!(settings[1]["params"]["value"], "high");
        }
    }
}

#[test]
fn missing_harness_is_reported_before_missing_adapter() {
    let root = agent();
    let config_path = root.path().join(".agents/agent.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["harness"] = "claude".into();
    fs::write(config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_lazyagents"))
        .arg("chat")
        .current_dir(root.path())
        .env("PATH", root.path().join("bin"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains("Claude Code is not installed or is not on PATH"),
        "{error}"
    );
    assert!(!error.contains("adapter"));
    assert!(!root.path().join(".agents/runtime").exists());
}

#[test]
fn terminal_formats_submitted_prompts_and_shows_waiting_before_output() {
    let root = agent();
    let config_path = root.path().join(".agents/agent.json");
    let mut initial_config: serde_json::Value =
        serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    initial_config["draft"] = "recovered draft".into();
    fs::write(&config_path, initial_config.to_string()).unwrap();

    let adapter = root.path().join("bin/opencode");
    let source = fs::read_to_string(&adapter).unwrap();
    let marker = "      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"text\":\"Before\\n\\n\"}}}}'";
    assert!(source.contains(marker));
    fs::write(&adapter, source.replace(marker, &format!(r#"      sleep 0.2
      printf '%s\n' '{{"jsonrpc":"2.0","method":"session/update","params":{{"update":{{"sessionUpdate":"agent_thought_chunk","content":{{"text":"\n\n"}}}}}}}}'
      printf '%s\n' '{{"jsonrpc":"2.0","method":"session/update","params":{{"update":{{"sessionUpdate":"agent_thought_chunk","content":{{"text":"**Checking weather**"}}}}}}}}'
{marker}"#))).unwrap();

    let update = |update: serde_json::Value| {
        format!(
            "      printf '%s\\n' '{}'\n",
            serde_json::json!({
                "jsonrpc": "2.0", "method": "session/update", "params": {"update": update}
            })
        )
    };
    let mut events = update(serde_json::json!({
        "sessionUpdate": "agent_message_chunk", "content": {"text": format!("```text\n{}", "x".repeat(600))}
    }));
    events.push_str("      sleep 0.2\n");
    events.push_str(&update(serde_json::json!({
        "sessionUpdate": "agent_message_chunk", "content": {"text": "tail"}
    })));
    events.push_str("      sleep 0.2\n");
    events.push_str(&update(serde_json::json!({
        "sessionUpdate": "agent_message_chunk", "content": {"text": "\n```\n"}
    })));
    for id in ["A", "B"] {
        events.push_str(&update(serde_json::json!({
            "sessionUpdate": "tool_call", "toolCallId": id, "title": format!("Tool {id}"), "status": "in_progress"
        })));
    }
    for id in ["B", "A"] {
        events.push_str("      sleep 0.1\n");
        events.push_str(&update(serde_json::json!({
            "sessionUpdate": "tool_call_update", "toolCallId": id, "status": "completed", "rawOutput": "Result"
        })));
    }
    let source = fs::read_to_string(&adapter).unwrap();
    fs::write(
        &adapter,
        source.replace(marker, &format!("{events}{marker}")),
    )
    .unwrap();

    let (mut chat, mut terminal, _slave) = terminal_chat(root.path(), 40, 24);
    fn wait_terminal(chat: &mut Chat, terminal: &mut fs::File, suffix: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut plain = String::new();
        let mut ansi = 0;
        loop {
            let byte = chat
                .output
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| panic!("waiting for {suffix:?}: {error}; {:?}", chat.text));
            chat.text.push(char::from(byte));
            plain_byte(&mut plain, &mut ansi, byte);
            if chat.text.ends_with("\x1b[6n") {
                terminal.write_all(b"\x1b[1;1R").unwrap();
            }
            if if suffix.contains('\x1b') {
                chat.text.ends_with(suffix)
            } else {
                plain.ends_with(suffix)
            } {
                break;
            }
        }
    }
    wait_terminal(&mut chat, &mut terminal, "recovered draft");
    wait_terminal(&mut chat, &mut terminal, "\r\x1b[17C");
    let rule = "─".repeat(40).bytes().map(char::from).collect::<String>();
    let first_input = chat.text.rsplit(" commands").next().unwrap();
    assert_eq!(
        first_input.matches(&rule).count(),
        0,
        "input must have no borders: {first_input:?}"
    );
    fn resize_input(chat: &mut Chat, terminal: &mut fs::File, columns: u16, cursor: &str) {
        use std::os::fd::AsRawFd;
        let size = libc::winsize {
            ws_row: 24,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let start = chat.text.len();
        assert_eq!(
            unsafe { libc::ioctl(terminal.as_raw_fd(), libc::TIOCSWINSZ as _, &size) },
            0
        );
        assert_eq!(
            unsafe { libc::kill(chat.child.id() as i32, libc::SIGWINCH) },
            0
        );
        wait_terminal(chat, terminal, "\x1b[?2026h");
        wait_terminal(chat, terminal, cursor);
        let rule = "─"
            .repeat(usize::from(columns))
            .bytes()
            .map(char::from)
            .collect::<String>();
        if cursor == "\r\x1b[17C" {
            assert!(chat.text[start..].contains("> \x1b[0mrecovered draft"));
            assert_eq!(chat.text[start..].matches("Test").count(), 0);
            assert!(
                chat.text[start..].contains("\x1b8\r\x1b[J"),
                "resize must clear the reflowed editor from its saved start"
            );
        }
        assert_eq!(
            chat.text[start..].matches(&rule).count(),
            0,
            "resized input must not draw borders at {columns} columns: {:?}",
            &chat.text[start..]
        );
    }
    for columns in [100, 24, 40] {
        resize_input(&mut chat, &mut terminal, columns, "\r\x1b[17C");
    }
    terminal.write_all(b"\x03draft to save").unwrap();
    wait_terminal(&mut chat, &mut terminal, "draft to save");
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let config: serde_json::Value =
            serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        if config["draft"] == "draft to save" {
            break;
        }
        assert!(Instant::now() < deadline, "draft was not saved: {config}");
        std::thread::sleep(Duration::from_millis(20));
    }
    terminal.write_all(b"\x03").unwrap();
    wait_terminal(&mut chat, &mut terminal, "> ");
    let input_start = chat.text.len();
    terminal.write_all(b"\x03\x03discard me").unwrap();
    wait_terminal(&mut chat, &mut terminal, "discard me");
    terminal.write_all(b"\x03escape this").unwrap();
    wait_terminal(&mut chat, &mut terminal, "escape this");
    terminal.write_all(b"\x1b").unwrap();
    wait_terminal(&mut chat, &mut terminal, "> ");
    assert!(
        !chat.text[input_start..].contains("\x1b[?2004l"),
        "input cancellation restarted the editor: {:?}",
        &chat.text[input_start..]
    );
    terminal.write_all(b"Hello\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "After\r\n");
    assert!(
        chat.text
            .contains("\x1b[0;97m| \x1b[0m\x1b[0;1;97mHello\x1b[0m"),
        "{:?}",
        chat.text
    );
    assert!(chat.text.find("Thinking for ").unwrap() < chat.text.find("Checking weather").unwrap());
    assert!(!chat.text.contains("**Checking weather**"));
    assert!(chat.text.contains("\x1b[38;5;242mChecking weather\r\n"));
    let code_start = chat.text.find(&"x".repeat(512)).unwrap();
    let code_end = chat.text[code_start..].find("tail").unwrap() + code_start;
    assert!(!chat.text[code_start..code_end].contains("Thinking for "));
    assert!(!chat.text[code_start..code_end].contains("Running:"));
    let terminal_bytes = chat
        .text
        .chars()
        .map(|character| character as u8)
        .collect::<Vec<_>>();
    let terminal_text = String::from_utf8(terminal_bytes).unwrap();
    assert!(
        !terminal_text.contains("○ tool a:\r\n"),
        "{terminal_text:?}"
    );
    for id in ["A", "B"] {
        assert_eq!(
            terminal_text
                .matches(&format!(
                    "\x1b[32m✓ \x1b[0m\x1b[38;5;250mtool {}:\r\n",
                    id.to_lowercase()
                ))
                .count(),
            1
        );
    }
    let next_input_start = chat.text.len();
    wait_terminal(&mut chat, &mut terminal, "> ");
    wait_terminal(&mut chat, &mut terminal, "\r\x1b[2C");
    assert_eq!(chat.text[next_input_start..].matches(&rule).count(), 0);
    let multiline_start = chat.text.len();
    terminal
        .write_all(
            b"\x1b[200~first line\nsecond line which wraps beyond forty columns here\x1b[201~\r",
        )
        .unwrap();
    wait_terminal(&mut chat, &mut terminal, "After\r\n");
    assert!(
        chat.text.contains(
            "\x1b[0;97m| \x1b[0m\x1b[0;1;97mfirst line\r\nsecond line which wraps beyond forty columns here\x1b[0m"
        ),
        "{:?}",
        chat.text
    );
    // The dedicated editor clears its region and writes the message at its top.
    // It no longer needs Rustyline's delete-line repair after submission.
    assert!(chat.text[multiline_start..].contains("\x1b[?7h\x1b[0;97m| "));
    let final_redraw = chat.text[multiline_start..]
        .split("\x1b[?2004l")
        .next()
        .unwrap()
        .rsplit("\x1b[K")
        .next()
        .unwrap();
    assert!(
        !final_redraw.contains(&rule),
        "submitted message kept an input border: {final_redraw:?}"
    );
    wait_terminal(&mut chat, &mut terminal, "> ");
    terminal.write_all(b"Wait\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "Waiting\r\n");
    terminal.write_all(b"\x1b[A\x03").unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(!fs::read_to_string(root.path().join("requests.jsonl"))
        .unwrap()
        .contains("session/cancel"));
    terminal.write_all(b"\x1b").unwrap();
    wait_terminal(&mut chat, &mut terminal, "Turn cancelled.\r\n");
    wait_terminal(&mut chat, &mut terminal, "> ");
    terminal.write_all(b"Permission\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "always allow");
    terminal.write_all(b"\x1b[B\x1b[B\x1b[B\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "After\r\n");
    wait_terminal(&mut chat, &mut terminal, "> ");
    terminal.write_all(b"/model\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "Select model");
    terminal.write_all(b"\x1b[B\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "Select reasoning level");
    terminal.write_all(b"\x1b[B\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "Reasoning: High");
    wait_terminal(&mut chat, &mut terminal, "> ");
    terminal.write_all(b"/model\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "> deep");
    terminal.write_all(b"\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "> High");
    terminal.write_all(b"\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "Reasoning: High");
    wait_terminal(&mut chat, &mut terminal, "> ");
    terminal.write_all(b"/permissions\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "> ask for approval");
    terminal.write_all(b"\x1b[B\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "permissions full access");
    wait_terminal(&mut chat, &mut terminal, "> ");
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join(".agents/agent.json")).unwrap()).unwrap();
    assert_eq!(config["permission_mode"], "full_access");
    assert_eq!(config["permissions"].as_array().unwrap().len(), 1);
    terminal.write_all(b"/permissions\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "> full access");
    terminal.write_all(b"\x1b[A\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "permissions ask for approval");
    wait_terminal(&mut chat, &mut terminal, "> ");
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join(".agents/agent.json")).unwrap()).unwrap();
    assert_eq!(config["permission_mode"], "ask");
    assert_eq!(config["model"], "deep");
    assert_eq!(config["thinking"], "high");
    assert_eq!(config["draft"], "");
    assert_eq!(config["permissions"].as_array().unwrap().len(), 1);
    terminal.write_all(b"/model\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "Select model");
    terminal.write_all(b"\x1b").unwrap();
    wait_terminal(&mut chat, &mut terminal, "<canceled>");
    wait_terminal(&mut chat, &mut terminal, "> ");
    terminal.write_all(b"Still here\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "After\r\n");
    wait_terminal(&mut chat, &mut terminal, "> ");
    // The menu filters, Escape preserves the prefix, and Right never inserts menu text.
    terminal.write_all(b"/sta").unwrap();
    wait_terminal(&mut chat, &mut terminal, "\r\x1b[6C");
    terminal.write_all(b"\x1b").unwrap();
    wait_terminal(&mut chat, &mut terminal, "\r\x1b[6C");
    terminal.write_all(b"\x1b[Ctus\r").unwrap();
    wait_terminal(
        &mut chat,
        &mut terminal,
        "usage: not reported by harness\r\n",
    );
    wait_terminal(&mut chat, &mut terminal, "> ");
    // Navigate the full list in both directions and run a choice from just '/'.
    let menu_start = chat.text.len();
    terminal.write_all(b"/").unwrap();
    wait_terminal(&mut chat, &mut terminal, "\r\x1b[3C");
    let menu = &chat.text[menu_start..];
    for name in ["/model", "/permissions", "/new", "/status", "/quit"] {
        assert!(
            menu.contains(name),
            "missing {name} in command menu: {menu:?}"
        );
    }
    if std::env::var_os("NO_COLOR").is_none() {
        assert!(menu.contains("\x1b[97m> /model"));
        assert!(menu.contains("\x1b[38;5;250m  /permissions"));
    }
    for columns in [100, 24, 40] {
        resize_input(&mut chat, &mut terminal, columns, "\r\x1b[3C");
    }
    terminal.write_all(b"\x1b[A\x1b[A\x1b[B\x1b[A\r").unwrap();
    wait_terminal(
        &mut chat,
        &mut terminal,
        "usage: not reported by harness\r\n",
    );
    wait_terminal(&mut chat, &mut terminal, "> ");
    assert!(!chat.text[menu_start..].contains("> /status\r"));
    let completion_start = chat.text.len();
    terminal.write_all(b"/s\t").unwrap();
    wait_terminal(&mut chat, &mut terminal, "\r\x1b[9C");
    assert!(chat.text[completion_start..].contains("/status"));
    assert!(!chat.text[completion_start..].contains("usage: not reported"));
    terminal.write_all(b"\r").unwrap();
    wait_terminal(
        &mut chat,
        &mut terminal,
        "usage: not reported by harness\r\n",
    );
    wait_terminal(&mut chat, &mut terminal, "> ");
    assert!(
        chat.text[completion_start..].contains("\x1b[K"),
        "status must reclaim the hidden command and border rows: {:?}",
        &chat.text[completion_start..]
    );
    let submitted_row = chat.text[completion_start..]
        .split("\x1b[?2004l")
        .next()
        .unwrap()
        .rsplit("\x1b[K")
        .next()
        .unwrap();
    assert!(
        !submitted_row.contains("/s") && !submitted_row.contains('>'),
        "command remained after the final redraw: {submitted_row:?}"
    );
    assert!(chat.text.contains("reasoning: High"));
    let new_session_start = chat.text.len();
    terminal.write_all(b"/new\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "\r\n\r\nNew session started\r\n");
    assert!(!chat.text[new_session_start..].contains("\x1b[3J"));
    assert!(!chat.text[new_session_start..].contains("\x1b[2J"));
    let fresh_screen = chat.text.len();
    wait_terminal(&mut chat, &mut terminal, "> ");
    assert!(chat.text[fresh_screen..].contains("Test"));
    assert!(chat.text[fresh_screen..].contains("deep"));
    assert!(!chat.text[fresh_screen..].contains("New conversation."));
    terminal.write_all(b"\x1b[A").unwrap();
    wait_terminal(&mut chat, &mut terminal, "Still here");
    terminal.write_all(b"\x03Fresh start\r").unwrap();
    wait_terminal(&mut chat, &mut terminal, "After\r\n");
    wait_terminal(&mut chat, &mut terminal, "> ");
    terminal.write_all(b"/quit\r").unwrap();
    chat.wait_for_exit();
    let requests = fs::read_to_string(root.path().join("requests.jsonl")).unwrap();
    let requests = requests
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let last_prompt = requests
        .iter()
        .rev()
        .find(|request| request["method"] == "session/prompt")
        .unwrap();
    assert_eq!(last_prompt["params"]["prompt"][0]["text"], "Fresh start");
    let mut plain = String::new();
    let mut ansi = 0;
    for byte in chat.text.chars().map(|c| c as u8) {
        plain_byte(&mut plain, &mut ansi, byte);
    }
    assert!(plain.contains("> permission (bash): always allow"));
    assert!(!plain.contains("? bash"));
    let permission = requests
        .iter()
        .find(|request| request["id"] == 78 && request.get("result").is_some())
        .unwrap();
    assert_eq!(permission["result"]["outcome"]["optionId"], "allow");
    let settings = requests
        .iter()
        .filter(|request| request["method"] == "session/set_config_option")
        .collect::<Vec<_>>();
    assert_eq!(settings.len(), 4);
    assert_eq!(settings[2]["params"]["value"], "deep");
    assert_eq!(settings[3]["params"]["value"], "high");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "session/new")
            .count(),
        2
    );
    assert_eq!(settings[0]["params"]["configId"], "model");
    assert_eq!(settings[0]["params"]["value"], "deep");
    assert_eq!(settings[1]["params"]["configId"], "effort");
    assert_eq!(settings[1]["params"]["value"], "high");
    assert!(requests
        .iter()
        .filter(|request| request["method"] == "session/prompt")
        .all(|request| !matches!(
            request["params"]["prompt"][0]["text"].as_str(),
            Some("/model" | "/thinking" | "/permissions" | "/new" | "/status")
        )));
}

fn terminal_chat(root: &std::path::Path, columns: u16, rows: u16) -> (Chat, fs::File, fs::File) {
    terminal_command(root, columns, rows, "chat")
}

fn terminal_command(
    root: &std::path::Path,
    columns: u16,
    rows: u16,
    subcommand: &str,
) -> (Chat, fs::File, fs::File) {
    use std::os::fd::FromRawFd;
    use std::os::unix::process::CommandExt;
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: rows,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // Give the editor a real terminal so wrapping and final redraws are exercised.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        },
        0
    );
    let terminal = unsafe { fs::File::from_raw_fd(master) };
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    let saved_slave = slave.try_clone().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_lazyagents"));
    command
        .arg(subcommand)
        .current_dir(root)
        .env("TERM", "xterm-256color")
        .env(
            "PATH",
            format!(
                "{}:{}",
                root.join("bin").display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave);
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let (_, status) = mpsc::channel();
    let chat = Chat {
        child: command.spawn().unwrap(),
        output: read_output(terminal.try_clone().unwrap()),
        status,
        text: String::new(),
        status_text: String::new(),
    };
    (chat, terminal, saved_slave)
}

// Feed actual PTY bytes to a screen parser, including cursor-position replies.
fn terminal_byte(chat: &mut Chat, terminal: &mut fs::File, screen: &mut vt100::Parser, byte: u8) {
    chat.text.push(char::from(byte));
    screen.process(&[byte]);
    if chat.text.ends_with("\x1b[6n") {
        let (row, column) = screen.screen().cursor_position();
        write!(terminal, "\x1b[{};{}R", row + 1, column + 1).unwrap();
    }
}

fn terminal_frame(chat: &mut Chat, terminal: &mut fs::File, screen: &mut vt100::Parser) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let byte = chat
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|error| {
                panic!("waiting for frame: {error}; {}", screen.screen().contents())
            });
        terminal_byte(chat, terminal, screen, byte);
        if chat.text.ends_with("\x1b[?2026l") {
            break;
        }
    }
    // Consume the final cursor movement and any other frames from batched keys.
    while let Ok(byte) = chat.output.recv_timeout(Duration::from_millis(30)) {
        terminal_byte(chat, terminal, screen, byte);
    }
}

#[test]
fn terminal_paste_history_resize_and_cleanup() {
    use std::os::fd::AsRawFd;
    let root = agent();
    let (mut chat, mut terminal, _slave) = terminal_chat(root.path(), 40, 24);
    let mut screen = vt100::Parser::new(24, 40, 500);
    terminal_frame(&mut chat, &mut terminal, &mut screen);
    assert!(
        chat.output
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "idle input repainted"
    );
    // Signals can collapse a shrink/grow pair into the same final dimensions.
    unsafe {
        libc::kill(chat.child.id() as i32, libc::SIGWINCH);
    }
    terminal_frame(&mut chat, &mut terminal, &mut screen);
    let text = format!("e\u{301}👩🏽‍💻🇺🇿\n{}tail", "long line 界\n".repeat(40));
    write!(terminal, "\x1b[200~{text}\x1b[201~").unwrap();
    terminal_frame(&mut chat, &mut terminal, &mut screen);
    for (width, height) in [(8, 8), (1, 3), (2, 2), (80, 24), (24, 10), (40, 24)] {
        let size = libc::winsize {
            ws_row: height,
            ws_col: width,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        screen.screen_mut().set_size(height, width);
        assert_eq!(
            unsafe { libc::ioctl(terminal.as_raw_fd(), libc::TIOCSWINSZ as _, &size) },
            0
        );
        unsafe {
            libc::kill(chat.child.id() as i32, libc::SIGWINCH);
        }
        terminal_frame(&mut chat, &mut terminal, &mut screen);
        let (row, column) = screen.screen().cursor_position();
        assert!(row < height && column < width);
        assert!(!screen.screen().alternate_screen());
    }
    terminal.write_all(b"\r").unwrap();
    // Wait for the next editor, after the fake harness returns its answer.
    loop {
        terminal_frame(&mut chat, &mut terminal, &mut screen);
        if chat.text.contains("After\r\n") && screen.screen().contents().contains("> ") {
            break;
        }
    }
    terminal.write_all(b"\x12\x1b[200~tail\x1b[201~").unwrap();
    terminal_frame(&mut chat, &mut terminal, &mut screen);
    assert!(chat.text.contains("reverse-i-search"));
    terminal.write_all(b"\x1b").unwrap();
    terminal_frame(&mut chat, &mut terminal, &mut screen);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let config: serde_json::Value =
            serde_json::from_slice(&fs::read(root.path().join(".agents/agent.json")).unwrap())
                .unwrap();
        if config["draft"] == text {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "history did not restore the pasted input"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    terminal.write_all(b"\x03one\x1b\rsecond\x0athird").unwrap();
    terminal_frame(&mut chat, &mut terminal, &mut screen);
    assert!(
        screen.screen().contents().contains("> one\nsecond\nthird"),
        "{}",
        screen.screen().contents()
    );
    terminal.write_all(b"\x03\x04").unwrap();
    chat.wait_for_exit();
    while let Ok(byte) = chat.output.recv_timeout(Duration::from_millis(50)) {
        terminal_byte(&mut chat, &mut terminal, &mut screen, byte);
    }
    let mut mode = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(terminal.as_raw_fd(), mode.as_mut_ptr()) },
        0
    );
    let mode = unsafe { mode.assume_init() };
    assert_ne!(mode.c_lflag & libc::ICANON, 0);
    assert_ne!(mode.c_lflag & libc::ECHO, 0);
    assert!(chat.text.contains("\x1b[?2004l"));
    assert!(!screen.screen().hide_cursor());
    assert!(!screen.screen().bold());
    let requests = fs::read_to_string(root.path().join("requests.jsonl")).unwrap();
    let request = requests
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|request| request["method"] == "session/prompt")
        .unwrap();
    assert_eq!(request["params"]["prompt"][0]["text"], text);
}

#[test]
fn terminal_restores_modes_when_draft_save_fails() {
    use std::os::fd::AsRawFd;
    let root = agent();
    let (mut chat, mut terminal, _slave) = terminal_chat(root.path(), 40, 24);
    let mut screen = vt100::Parser::new(24, 40, 100);
    terminal_frame(&mut chat, &mut terminal, &mut screen);
    // Make the config unwritable without relying on user privileges.
    let config = root.path().join(".agents/agent.json");
    fs::remove_file(&config).unwrap();
    fs::create_dir(&config).unwrap();
    terminal.write_all(b"Hello\r").unwrap();
    chat.wait_for_status(false);
    while let Ok(byte) = chat.output.recv_timeout(Duration::from_millis(50)) {
        terminal_byte(&mut chat, &mut terminal, &mut screen, byte);
    }
    let mut mode = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(terminal.as_raw_fd(), mode.as_mut_ptr()) },
        0
    );
    let mode = unsafe { mode.assume_init() };
    assert_ne!(mode.c_lflag & libc::ICANON, 0);
    assert_ne!(mode.c_lflag & libc::ECHO, 0);
    assert!(chat.text.contains("\x1b[?2004l"));
    assert!(!screen.screen().hide_cursor());
    assert!(!screen.screen().bold());
}

#[test]
fn terminal_keeps_submitted_draft_when_session_creation_fails() {
    let root = agent();
    let (mut chat, mut terminal, _slave) = terminal_chat(root.path(), 80, 24);
    let mut screen = vt100::Parser::new(24, 80, 100);
    terminal_frame(&mut chat, &mut terminal, &mut screen);
    fs::write(root.path().join(".agents/sessions"), "blocked").unwrap();
    terminal.write_all(b"valuable unsaved prompt\r").unwrap();
    chat.wait_for_status(false);
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join(".agents/agent.json")).unwrap()).unwrap();
    assert_eq!(config["draft"], "valuable unsaved prompt");
}

// Inquire may enable colors independently on macOS and Linux. Text waits ignore
// SGR styling; tests for editor control sequences still inspect the raw bytes.
fn plain_byte(text: &mut String, ansi: &mut u8, byte: u8) {
    match (*ansi, byte) {
        (0, 0x1b) => *ansi = 1,
        (0, _) => text.push(char::from(byte)),
        (1, b'[') => *ansi = 2,
        (1, b']') => *ansi = 3,
        (3, 7) | (4, b'\\') => *ansi = 0,
        (3, 0x1b) => *ansi = 4,
        (4, _) => *ansi = 3,
        (1, _) => *ansi = 0,
        (2, b'@'..=b'~') => *ansi = 0,
        _ => {}
    }
}

fn terminal_prompt(
    chat: &mut Chat,
    terminal: &mut fs::File,
    screen: &mut vt100::Parser,
    label: &str,
    input: &[u8],
) {
    let mut plain = String::new();
    let mut ansi = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let byte = chat
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|error| panic!("waiting for {label}: {error}; {plain}"));
        terminal_byte(chat, terminal, screen, byte);
        plain_byte(&mut plain, &mut ansi, byte);
        if plain.contains(label) && chat.text.ends_with("\x1b[?25h") {
            break;
        }
    }
    terminal.write_all(input).unwrap();
}

fn preparing_agent() -> tempfile::TempDir {
    let root = agent();
    fs::remove_dir_all(root.path().join(".agents")).unwrap();
    for (name, script) in [
        ("pi", "#!/bin/sh\nexit 0\n"),
        ("node", "#!/bin/sh\nexec \"$@\"\n"),
        (
            "npm",
            r#"#!/bin/sh
set -eu
printf '%s\n' "$$" > npm.pid
sleep 60 &
sleeper=$!
printf '%s\n' "$sleeper" > npm-child.pid
trap 'kill "$sleeper" 2>/dev/null || true' EXIT
attempts=0
while [ ! -f finish-install ]; do
  attempts=$((attempts + 1))
  [ "$attempts" -lt 500 ] || exit 1
  sleep 0.02
done
kill "$sleeper"
wait "$sleeper" 2>/dev/null || true
mkdir -p .agents/runtime/acp-pi/node_modules/.bin
cp bin/opencode .agents/runtime/acp-pi/node_modules/.bin/pi-acp
"#,
        ),
    ] {
        let path = root.path().join("bin").join(name);
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    root
}

fn terminal_progress(
    chat: &mut Chat,
    terminal: &mut fs::File,
    screen: &mut vt100::Parser,
    message: &str,
    marker: &std::path::Path,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !screen.screen().contents().contains(message) || !marker.exists() {
        if let Ok(byte) = chat.output.recv_timeout(Duration::from_millis(30)) {
            terminal_byte(chat, terminal, screen, byte);
        }
        assert!(
            Instant::now() < deadline,
            "waiting for {message}: {}",
            screen.screen().contents()
        );
    }
}

#[test]
fn setup_retries_invalid_text_and_cleans_up_scrolled_menus() {
    for (width, submit_model) in [(80, false), (40, false), (80, true)] {
        let root = agent();
        fs::remove_dir_all(root.path().join(".agents")).unwrap();
        let adapter = root.path().join("bin/opencode");
        let script = fs::read_to_string(&adapter).unwrap();
        let models = (0..12)
            .map(|index| serde_json::json!({"value": format!("model-{index}"), "name": format!("Model {index}")}))
            .collect::<Vec<_>>();
        fs::write(
            adapter,
            script.replace(
                r#"[{"value":"fast","name":"Fast"},{"value":"deep","name":"Deep"}]"#,
                &serde_json::to_string(&models).unwrap(),
            ),
        )
        .unwrap();
        let (mut chat, mut terminal, _slave) = terminal_command(root.path(), width, 12, "init");
        let mut screen = vt100::Parser::new(12, width, 100);
        let mut prompt = |label: &str, input: &[u8]| {
            terminal_prompt(&mut chat, &mut terminal, &mut screen, label, input);
        };
        prompt("Select harness runtime", b"OpenCode\r");
        prompt("What is your Agent name:", b"\r");
        prompt("the agent name must contain", b"   \r");
        prompt("What is your Agent name:", b" -- \r");
        prompt(
            "What is your Agent name:",
            b"\x7f\x7f\x7f\x7f\x7f\x7f\x7fBuddy\r",
        );
        prompt("Describe what your Agent does:", b"\r");
        prompt("the agent description cannot be empty", b"   \r");
        prompt("Describe what your Agent does:", b"friend\r");
        prompt(
            "Select model for your Agent:",
            if submit_model { b"\r" } else { b"\x1b" },
        );
        if submit_model {
            prompt("Select reasoning level for your Agent:", b"\x1b");
        }
        loop {
            let byte = chat.output.recv_timeout(Duration::from_secs(5)).unwrap();
            terminal_byte(&mut chat, &mut terminal, &mut screen, byte);
            if chat.text.ends_with("Setup cancelled.\r\n") {
                break;
            }
        }
        chat.wait_for_exit();
        assert!(chat.text.contains("> Agent name: \x1b[1mBuddy\x1b[0m\r\n"));
        let contents = screen.screen().contents();
        let answer = if submit_model {
            "> Model: model 0"
        } else {
            "> Agent description: friend"
        };
        assert!(
            contents.ends_with(&format!("{answer}\nSetup cancelled.")),
            "width={width}, submitted={submit_model}: {contents}"
        );
        assert!(!contents.contains("<canceled>"), "{contents}");
        assert!(!contents.contains("Select model"), "{contents}");
        assert!(!root.path().join(".agents").exists());
    }
}

#[test]
fn setup_soul_cancellation_stops_adapter_and_restores_terminal() {
    use std::os::fd::AsRawFd;
    for (key, ignore_cancel, keep_workspace) in [
        (Some("\x1b"), false, false),
        (Some("\x03"), false, true),
        (None, false, false),
        (Some("\x1b"), true, true),
    ] {
        let root = agent();
        fs::remove_dir_all(root.path().join(".agents")).unwrap();
        if keep_workspace {
            fs::create_dir(root.path().join("workspace")).unwrap();
            fs::write(root.path().join("workspace/keep.txt"), "existing work").unwrap();
        }
        let adapter = root.path().join("bin/opencode");
        let mut script = fs::read_to_string(&adapter)
            .unwrap()
            .replace("prompts=0", "prompts=0\nprintf '%s\\n' \"$$\" > adapter.pid")
            .replace(
                "      case \"$line\" in\n",
                "      case \"$line\" in\n        *'Create the system prompt for this agent.'*) continue ;;\n",
            );
        if ignore_cancel {
            script = script.replace(
                "    *'\"method\":\"session/cancel\"'*)",
                "    *'\"method\":\"session/cancel\"'*) continue ;;\n    unused)",
            );
        }
        fs::write(adapter, script).unwrap();
        let (mut chat, mut terminal, _slave) = terminal_command(root.path(), 80, 24, "init");
        let mut screen = vt100::Parser::new(24, 80, 100);
        for (label, input) in [
            ("Select harness runtime", "OpenCode\r"),
            ("What is your Agent name:", "Buddy\r"),
            ("Describe what your Agent does:", "friend\r"),
            ("Select model for your Agent:", "\r"),
            ("Select reasoning level for your Agent:", "\r"),
        ] {
            terminal_prompt(
                &mut chat,
                &mut terminal,
                &mut screen,
                label,
                input.as_bytes(),
            );
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(byte) = chat.output.recv_timeout(Duration::from_millis(30)) {
                terminal_byte(&mut chat, &mut terminal, &mut screen, byte);
            }
            if screen.screen().contents().contains("Creating SOUL.md...")
                && fs::read_to_string(root.path().join("requests.jsonl"))
                    .unwrap()
                    .contains("\"method\":\"session/prompt\"")
            {
                break;
            }
            assert!(Instant::now() < deadline, "SOUL generation did not start");
        }
        if let Some(key) = key {
            terminal.write_all(key.as_bytes()).unwrap();
        } else {
            assert_eq!(
                unsafe { libc::kill(chat.child.id() as i32, libc::SIGINT) },
                0
            );
        }
        let deadline = Instant::now() + Duration::from_secs(12);
        while !chat.text.ends_with("Setup cancelled.\r\n") {
            let byte = chat
                .output
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| panic!("cancelling SOUL: {error}; {:?}", chat.text));
            terminal_byte(&mut chat, &mut terminal, &mut screen, byte);
        }
        chat.wait_for_exit();
        let contents = screen.screen().contents();
        assert!(
            contents.ends_with("> Reasoning level: Low\nSetup cancelled."),
            "{contents}"
        );
        assert!(!contents.contains("Creating SOUL.md"), "{contents}");
        assert!(!contents.contains("\n\n\n"), "{contents}");
        assert!(!screen.screen().hide_cursor());
        assert!(!root.path().join(".agents").exists());
        assert!(!root.path().join("buddy").exists());
        if keep_workspace {
            assert_eq!(
                fs::read_to_string(root.path().join("workspace/keep.txt")).unwrap(),
                "existing work"
            );
        } else {
            assert!(!root.path().join("workspace").exists());
        }
        let requests = fs::read_to_string(root.path().join("requests.jsonl")).unwrap();
        assert!(requests.contains("\"method\":\"session/cancel\""));
        assert!(requests.contains("\"method\":\"session/close\""));
        let pid: i32 = fs::read_to_string(root.path().join("adapter.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "adapter is still running"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        let mut mode = std::mem::MaybeUninit::<libc::termios>::uninit();
        assert_eq!(
            unsafe { libc::tcgetattr(terminal.as_raw_fd(), mode.as_mut_ptr()) },
            0
        );
        let mode = unsafe { mode.assume_init() };
        assert_ne!(mode.c_lflag & libc::ICANON, 0);
        assert_ne!(mode.c_lflag & libc::ECHO, 0);
    }
}

#[test]
fn setup_preparation_cancellation_stops_npm_and_its_children() {
    for key in ["\x1b", "\x03"] {
        let root = preparing_agent();
        let (mut chat, mut terminal, _slave) = terminal_command(root.path(), 80, 24, "init");
        let mut screen = vt100::Parser::new(24, 80, 100);
        for (label, input) in [
            ("Select harness runtime", "Pi\r"),
            ("What is your Agent name:", "Buddy\r"),
            ("Describe what your Agent does:", "friend\r"),
        ] {
            terminal_prompt(
                &mut chat,
                &mut terminal,
                &mut screen,
                label,
                input.as_bytes(),
            );
        }
        terminal_progress(
            &mut chat,
            &mut terminal,
            &mut screen,
            "Preparing harness...",
            &root.path().join("npm-child.pid"),
        );
        terminal.write_all(key.as_bytes()).unwrap();
        while !chat.text.ends_with("Setup cancelled.\r\n") {
            let byte = chat.output.recv_timeout(Duration::from_secs(5)).unwrap();
            terminal_byte(&mut chat, &mut terminal, &mut screen, byte);
        }
        chat.wait_for_exit();
        let contents = screen.screen().contents();
        assert!(
            contents.ends_with("> Agent description: friend\nSetup cancelled."),
            "{contents}"
        );
        assert!(!contents.contains("Preparing harness"), "{contents}");
        assert!(!chat.text.contains("Installing "));
        assert!(!root.path().join(".agents").exists());
        assert!(!root.path().join("workspace").exists());
        for file in ["npm.pid", "npm-child.pid"] {
            let pid: i32 = fs::read_to_string(root.path().join(file))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while unsafe { libc::kill(pid, 0) } == 0 {
                assert!(Instant::now() < deadline, "{file} is still running");
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
    }
}

#[test]
fn setup_soul_completion_continues_to_launcher_prompt() {
    let root = preparing_agent();
    let adapter = root.path().join("bin/opencode");
    let script = fs::read_to_string(&adapter).unwrap().replace(
        "      prompts=$((prompts + 1))",
        "      touch soul-started\n      while [ ! -f finish-soul ]; do sleep 0.02; done\n      prompts=$((prompts + 1))",
    );
    fs::write(adapter, script).unwrap();
    let (mut chat, mut terminal, _slave) = terminal_command(root.path(), 80, 24, "init");
    let mut screen = vt100::Parser::new(24, 80, 100);
    for (label, input) in [
        ("Select harness runtime", "Pi\r"),
        ("What is your Agent name:", "Buddy\r"),
        ("Describe what your Agent does:", "friend\r"),
    ] {
        terminal_prompt(
            &mut chat,
            &mut terminal,
            &mut screen,
            label,
            input.as_bytes(),
        );
    }
    terminal_progress(
        &mut chat,
        &mut terminal,
        &mut screen,
        "Preparing harness...",
        &root.path().join("npm-child.pid"),
    );
    terminal.write_all(b"\r\r\r").unwrap();
    fs::write(root.path().join("finish-install"), "").unwrap();
    terminal_prompt(
        &mut chat,
        &mut terminal,
        &mut screen,
        "Select model for your Agent:",
        b"",
    );
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !fs::read_to_string(root.path().join("requests.jsonl"))
            .unwrap()
            .contains("session/set_config_option"),
        "Enter during preparation selected a model"
    );
    terminal.write_all(b"\r").unwrap();
    terminal_prompt(
        &mut chat,
        &mut terminal,
        &mut screen,
        "Select reasoning level for your Agent:",
        b"\r",
    );
    terminal_progress(
        &mut chat,
        &mut terminal,
        &mut screen,
        "Creating SOUL.md...",
        &root.path().join("soul-started"),
    );
    terminal.write_all(b"\r\r\r").unwrap();
    fs::write(root.path().join("finish-soul"), "").unwrap();
    terminal_prompt(
        &mut chat,
        &mut terminal,
        &mut screen,
        "Install a global launcher",
        b"",
    );
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        chat.child.try_wait().unwrap().is_none(),
        "Enter during SOUL generation answered the launcher prompt"
    );
    terminal.write_all(b"n\r").unwrap();
    while !chat.text.ends_with("Ready. Run ./buddy\r\n") {
        let byte = chat.output.recv_timeout(Duration::from_secs(5)).unwrap();
        terminal_byte(&mut chat, &mut terminal, &mut screen, byte);
    }
    chat.wait_for_exit();
    assert_eq!(
        fs::read_to_string(root.path().join(".agents/SOUL.md")).unwrap(),
        "Before\n\nAfter\n"
    );
    assert!(root.path().join(".agents/agent.json").is_file());
    assert!(root.path().join("buddy").is_file());
    assert!(!screen.screen().contents().contains("Creating SOUL.md"));
    let requests = fs::read_to_string(root.path().join("requests.jsonl")).unwrap();
    assert!(!requests.contains("\"method\":\"session/cancel\""));
    assert!(requests.contains("\"method\":\"session/close\""));
}

#[test]
fn setup_cancel_after_resize_removes_menu_and_blank_rows() {
    use std::os::fd::AsRawFd;
    for (cancel, submit_first) in [("\x03", false), ("\x1b", false), ("\x1b", true)] {
        let root = agent();
        let (mut chat, mut terminal, _slave) = terminal_command(root.path(), 80, 30, "init");
        let mut screen = vt100::Parser::new(30, 80, 100);
        wait_for(&chat.output, &mut chat.text, "\x1b[?25h");
        // read_output stores each byte as a char, so recover the original bytes.
        screen.process(&chat.text.chars().map(|c| c as u8).collect::<Vec<_>>());
        for width in [35, 100, 45, 80] {
            screen.screen_mut().set_size(30, width);
            let size = libc::winsize {
                ws_row: 30,
                ws_col: width,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            assert_eq!(
                unsafe { libc::ioctl(terminal.as_raw_fd(), libc::TIOCSWINSZ as _, &size) },
                0
            );
            unsafe {
                libc::kill(chat.child.id() as i32, libc::SIGWINCH);
            }
            // Let Inquire consume resize signals before sending the next key.
            // Burst resize behavior is covered by the editor test above.
            std::thread::sleep(Duration::from_millis(30));
        }
        if submit_first {
            terminal.write_all(b"\r").unwrap();
            wait_for(&chat.output, &mut chat.text, "What is your Agent name:");
            wait_for(&chat.output, &mut chat.text, "\x1b[?25h");
            terminal.write_all(b"Buddy\r").unwrap();
            wait_for(
                &chat.output,
                &mut chat.text,
                "Describe what your Agent does:",
            );
            wait_for(&chat.output, &mut chat.text, "\x1b[?25h");
            // Rebuild the screen from the full capture at the final width.
            screen = vt100::Parser::new(30, 80, 100);
            screen.process(&chat.text.chars().map(|c| c as u8).collect::<Vec<_>>());
        }
        terminal.write_all(cancel.as_bytes()).unwrap();
        loop {
            let byte = chat
                .output
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|error| {
                    panic!(
                        "setup cancellation {cancel:?}, submitted={submit_first}: {error}; {:?}",
                        chat.text
                    )
                });
            terminal_byte(&mut chat, &mut terminal, &mut screen, byte);
            if chat.text.ends_with("Setup cancelled.\r\n") {
                break;
            }
        }
        chat.wait_for_exit();
        let contents = screen.screen().contents();
        assert!(contents.starts_with("Create an Agent in "), "{contents}");
        if submit_first {
            assert!(contents.contains("> Harness: "), "{contents}");
            assert!(
                contents.ends_with("> Agent name: Buddy\nSetup cancelled."),
                "{contents}"
            );
        } else {
            assert!(contents.ends_with("\n\nSetup cancelled."), "{contents}");
        }
        assert!(!contents.contains("\n\n\n"), "{contents}");
        assert!(!contents.contains("Select harness runtime"), "{contents}");
        let mut mode = std::mem::MaybeUninit::<libc::termios>::uninit();
        assert_eq!(
            unsafe { libc::tcgetattr(terminal.as_raw_fd(), mode.as_mut_ptr()) },
            0
        );
        let mode = unsafe { mode.assume_init() };
        assert_ne!(mode.c_lflag & libc::ICANON, 0);
        assert_ne!(mode.c_lflag & libc::ECHO, 0);
    }
}
