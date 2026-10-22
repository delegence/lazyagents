mod commands;
mod editor;
mod input;
mod layout;
mod output;

use self::commands::Command;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{Datelike, Local};
use inquire::{InquireError, Select};
use tokio::signal;

use self::input::{ChatHeader, Input};
use crate::acp::{Client, ConfigKind, PermissionOption, PromptTimeout, StreamEvent};
use crate::config::{paths, AgentConfig};
use crate::harness;
use crate::mcp;
use crate::session::{Role, SessionEntry, SessionLog, SessionSummary};

use self::output::{
    compact_line, detail_lines, display_width, interactive, print_colored, render_history,
    render_user_prompt, TurnDisplay, LIGHT_GRAY,
};

pub async fn run(root: PathBuf, resume: Option<Option<String>>) -> Result<()> {
    let mut config = AgentConfig::load(&root)?;
    let agent_paths = paths(&root);
    let session_id = if let Some(requested) = resume {
        let Some(selected) = select_session(
            SessionLog::list(&agent_paths.sessions)?,
            requested.as_deref(),
        )?
        else {
            return Ok(());
        };
        if !selected.harness.is_empty() && selected.harness != config.harness {
            config.harness = selected.harness;
            config.model = None;
            config.thinking = None;
        }
        Some(selected.id)
    } else {
        None
    };
    harness::ensure_adapter(&root, harness::get(&config.harness)?).await?;
    let history = match &session_id {
        Some(session_id) => SessionLog::messages(&agent_paths.sessions, session_id)?,
        None => Vec::new(),
    };
    let mut client = start_client(&root, &config, session_id.as_deref()).await?;
    // The harness restores settings for an existing session.
    if session_id.is_none() {
        apply_selection(&mut client, ConfigKind::Model, config.model.as_deref()).await?;
        apply_selection(
            &mut client,
            ConfigKind::Thinking,
            config.thinking.as_deref(),
        )
        .await?;
    }
    let header = chat_header(&root, &config, &client)?;
    let header = if interactive() && history.is_empty() {
        Some(header)
    } else {
        eprint!("{}", header.render(interactive()));
        None
    };
    render_history(history)?;
    let prompt_history = if interactive() {
        SessionLog::prompt_history(&agent_paths.sessions)?
    } else {
        Vec::new()
    };
    let input = self::input::ChatInput::new(prompt_history, root.clone(), header);
    let mut log = None;

    loop {
        let next = if interactive() {
            // The editor restores terminal mode before returning an interrupt.
            input.read_line().await?
        } else {
            tokio::select! {
                result = input.read_line() => result?,
                _ = signal::ctrl_c() => Input::Eof,
            }
        };
        let prompt = match next {
            Input::Line(prompt) => prompt,
            Input::Interrupted => continue,
            Input::Eof => {
                client.close().await;
                print_colored(interactive(), LIGHT_GRAY, "\n");
                return Ok(());
            }
        };
        let prompt = prompt.trim_end_matches(['\r', '\n']);
        if prompt.trim().is_empty() {
            input.accepted()?;
            continue;
        }
        if let Some(command) = Command::parse(prompt.trim()) {
            input.accepted()?;
            if command == Command::Quit {
                client.close().await;
                return Ok(());
            }
            if command == Command::New {
                log = None;
            }
            run_command(command, &mut client, &root, &config).await?;
            continue;
        }

        render_user_prompt(prompt);
        let log = match &mut log {
            Some(log) => log,
            None => log.insert(SessionLog::create(
                &agent_paths.sessions,
                &config,
                client.session_id(),
            )?),
        };
        log.message(Role::User, prompt)?;
        input.accepted()?;
        run_turn(&mut client, log, prompt, &config).await?;
    }
}

async fn start_client(
    root: &Path,
    config: &AgentConfig,
    session_id: Option<&str>,
) -> Result<Client> {
    let agent_paths = paths(root);
    let soul = fs::read_to_string(&agent_paths.soul)
        .with_context(|| format!("could not read {}", agent_paths.soul.display()))?;
    let launch = harness::launch(root, &config.harness, &runtime_instruction(&soul))?;
    let servers = mcp::load(&agent_paths.mcp)?;
    let spinner = crate::progress::start(format!(
        "Starting {}...",
        crate::markdown::sanitize(&config.name)
    ));
    let client = Client::start(launch, root, servers, session_id).await;
    spinner.finish_and_clear();
    let mut client = client?;
    client.remember_permissions(root, harness::get(&config.harness)?);
    Ok(client)
}

fn chat_header(root: &Path, config: &AgentConfig, client: &Client) -> Result<ChatHeader> {
    Ok(ChatHeader::new(
        &config.name,
        config.color.unwrap_or(crate::config::AgentColor::Cyan),
        &config.harness,
        client
            .current_config_label(ConfigKind::Model)
            .unwrap_or("default"),
        client
            .current_config_label(ConfigKind::Thinking)
            .unwrap_or("default"),
        // Permission changes are saved to disk during chat.
        AgentConfig::load(root)?.permission_mode,
    ))
}

/// Runs a chat command other than `/quit`.
async fn run_command(
    command: Command,
    client: &mut Client,
    root: &Path,
    config: &AgentConfig,
) -> Result<()> {
    match command {
        Command::Quit => {}
        Command::Status => print_status(client, root, &config.harness)?,
        Command::Permissions => manage_permissions(root)?,
        Command::New => {
            let model = client
                .select_config_option(ConfigKind::Model)
                .map(|option| option.current_value.clone());
            let thinking = client
                .select_config_option(ConfigKind::Thinking)
                .map(|option| option.current_value.clone());
            let mut fresh = start_client(root, config, None).await?;
            apply_selection(&mut fresh, ConfigKind::Model, model.as_deref()).await?;
            apply_selection(&mut fresh, ConfigKind::Thinking, thinking.as_deref()).await?;
            client.close().await;
            *client = fresh;
            if interactive() {
                let header = chat_header(root, config, client)?;
                io::stdout().flush()?;
                print_colored(true, LIGHT_GRAY, "\n\nNew session started\n");
                eprint!("{}", header.render(true));
                io::stderr().flush()?;
            } else {
                print_colored(false, LIGHT_GRAY, "New session started\n");
            }
        }
        Command::Model => {
            if !interactive() {
                bail!("/model needs an interactive terminal");
            }
            if let Err(error) = change_model(client, root, &config.harness).await {
                if error.downcast_ref::<crate::acp::ProtocolError>().is_none() {
                    return Err(error);
                }
                print_colored(
                    true,
                    "31",
                    &format!("{}\n", client.friendly_error(&error, "")),
                );
            }
        }
    }
    Ok(())
}

/// Sends one prompt. Returns an error only when chat must stop.
async fn run_turn(
    client: &mut Client,
    log: &mut SessionLog,
    prompt: &str,
    config: &AgentConfig,
) -> Result<()> {
    let mut system_error = String::new();
    let mut display = TurnDisplay::new();
    let turn_input = crate::prompts::CancellationInput::new()?;
    display.start_waiting();
    let answer = tokio::select! {
        biased;
        result = client.prompt_with_timeout(prompt, |event| {
            log.stream(&event)?;
            if let StreamEvent::SystemError(text) = &event {
                system_error.push_str(text);
                return Ok(None);
            }
            if matches!(&event, StreamEvent::PermissionRequest { .. }) {
                turn_input.pause()?;
                let result = display.handle(event, permission_selection);
                turn_input.resume()?;
                result
            } else {
                display.handle(event, permission_selection)
            }
        }, Some(Duration::from_secs(config.idle_timeout.get()))) => Some(result),
        result = turn_input.cancelled(false) => {
            result?;
            None
        },
    };
    drop(turn_input);
    let interrupted = answer.as_ref().is_none_or(|result| {
        result
            .as_ref()
            .err()
            .and_then(|error| error.downcast_ref::<InquireError>())
            .is_some_and(|error| matches!(error, InquireError::OperationInterrupted))
    });
    if interrupted {
        let cancelled = client
            .cancel_prompt(|event| {
                log.stream(&event)?;
                display.handle(event, permission_selection)
            })
            .await;
        display.finish();
        log.append(&SessionEntry::TurnEnd)?;
        if let Err(error) = cancelled {
            log.append(&SessionEntry::Error {
                message: "The agent did not stop. Restart chat.".into(),
                detail: format!("{error:#}"),
                diagnostics: client.diagnostics(),
            })?;
            client.close().await;
            return Err(error);
        }
        print_colored(interactive(), LIGHT_GRAY, "Turn cancelled.\n");
        return Ok(());
    }
    display.finish();
    let Some(Err(error)) = answer else {
        return log.append(&SessionEntry::TurnEnd);
    };
    let friendly = client.friendly_error(&error, &system_error);
    log.append(&SessionEntry::Error {
        message: friendly.clone(),
        detail: format!("{error:#}\n{system_error}"),
        diagnostics: client.diagnostics(),
    })?;
    log.append(&SessionEntry::TurnEnd)?;
    if !interactive() {
        client.close().await;
        return Err(error.context(friendly));
    }
    if let Some(timeout) = error.downcast_ref::<PromptTimeout>() {
        print_colored(interactive(), "31", &format!("✗ {timeout}.\n"));
        if timeout.connection_ready() {
            return Ok(());
        }
    } else if error.downcast_ref::<crate::acp::ProtocolError>().is_some() {
        print_colored(interactive(), "31", &format!("✗ {friendly}\n"));
        return Ok(());
    }
    client.close().await;
    Err(error)
}

fn permission_selection(
    title: &str,
    options: &[PermissionOption],
    details: &str,
) -> Result<Option<String>> {
    let title = compact_line(title, display_width()).to_lowercase();
    let prompt = format!("permission ({title}):");
    let mut choices = options
        .iter()
        .map(|option| PermissionChoice {
            id: Some(option.id.clone()),
            inspect: false,
            kind: option.kind.clone(),
            label: {
                let label = option.label.to_lowercase();
                let suffix = format!(" {title}");
                compact_line(
                    label.strip_suffix(&suffix).unwrap_or(&label),
                    display_width(),
                )
            },
        })
        .collect::<Vec<_>>();
    choices.sort_by_key(|choice| permission_rank(&choice.kind));
    choices.insert(
        0,
        PermissionChoice {
            id: None,
            inspect: false,
            kind: "cancel".into(),
            label: "cancel".into(),
        },
    );
    if !details.is_empty() {
        choices.push(PermissionChoice {
            id: None,
            inspect: true,
            kind: String::new(),
            label: "read details".into(),
        });
    }
    loop {
        let choice =
            match crate::prompts::select(Select::new(&prompt, choices.clone()).with_render_config(
                crate::prompts::theme().with_answered_prompt_prefix(inquire::ui::Styled::new(">")),
            )) {
                Ok(choice) => choice,
                Err(InquireError::OperationCanceled) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
        if choice.inspect {
            inspect_details(details)?;
        } else {
            return Ok(choice.id);
        }
    }
}

#[derive(Clone)]
struct PermissionChoice {
    inspect: bool,
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

fn inspect_details(text: &str) -> Result<()> {
    let lines = detail_lines(text, display_width());
    let height = crossterm::terminal::size()
        .map(|(_, rows)| usize::from(rows).saturating_sub(5).clamp(1, 18))
        .unwrap_or(18);
    let pages = lines.chunks(height).collect::<Vec<_>>();
    for (index, page) in pages.iter().enumerate() {
        for line in *page {
            println!("  {line}");
        }
        let choices = if index + 1 < pages.len() {
            vec!["Next page", "Back to permission"]
        } else {
            vec!["Back to permission"]
        };
        match crate::prompts::select(Select::new("Details", choices)) {
            Ok("Next page") => {}
            Ok(_) | Err(InquireError::OperationCanceled) => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

struct SessionChoice(SessionSummary);

impl std::fmt::Display for SessionChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let date = self
            .0
            .last_message_at
            .map(|date| {
                date.with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| "Unknown date".into());
        write!(
            formatter,
            "{date}  {}",
            compact_line(
                &crate::markdown::preview(&self.0.last_message),
                display_width().saturating_sub(18).max(10)
            )
        )
    }
}

fn select_session(
    sessions: Vec<SessionSummary>,
    requested: Option<&str>,
) -> Result<Option<SessionSummary>> {
    if let Some(id) = requested {
        return sessions
            .into_iter()
            .find(|session| session.id == id)
            .map(Some)
            .with_context(|| format!("session {id:?} was not found"));
    }
    if sessions.is_empty() {
        bail!("no previous session is available");
    }
    if !interactive() {
        for session in sessions {
            let choice = SessionChoice(session);
            eprintln!("{}  {}", choice.0.id, choice);
        }
        bail!("the session picker needs a terminal; use `chat -r <session-id>` for a specific session");
    }
    match crate::prompts::select(
        Select::new(
            "Resume session",
            sessions.into_iter().map(SessionChoice).collect(),
        )
        .with_page_size(8),
    ) {
        Ok(choice) => Ok(Some(choice.0)),
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn manage_permissions(root: &Path) -> Result<()> {
    if !interactive() {
        bail!("/permissions needs an interactive terminal");
    }
    let current = AgentConfig::load(root)?.permission_mode;
    let choices = vec![
        crate::permissions::Mode::Ask,
        crate::permissions::Mode::FullAccess,
    ];
    let cursor = choices
        .iter()
        .position(|mode| *mode == current)
        .unwrap_or_default();
    match crate::prompts::select(Select::new("permissions", choices).with_starting_cursor(cursor)) {
        Ok(selected) if selected != current => crate::config::save_permission_mode(root, selected),
        Ok(_) | Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn save_model(client: &Client, root: &Path, harness: &str) -> Result<()> {
    crate::config::update(root, |config| {
        config["harness"] = harness.into();
        for (key, kind) in [
            ("model", ConfigKind::Model),
            ("thinking", ConfigKind::Thinking),
        ] {
            config[key] = client
                .select_config_option(kind)
                .map(|option| serde_json::Value::String(option.current_value.clone()))
                .unwrap_or_default();
        }
        Ok(())
    })
}

fn print_status(client: &Client, root: &Path, harness: &str) -> Result<()> {
    let config = AgentConfig::load(root)?;
    let mut text = format!(
        "session: {}\nharness: {harness}\nmodel: {}\nreasoning: {}\npermissions: {}\n",
        client.session_id(),
        client
            .current_config_label(ConfigKind::Model)
            .unwrap_or("default"),
        client
            .current_config_label(ConfigKind::Thinking)
            .unwrap_or("default"),
        config.permission_mode
    );
    if let Some((package, version)) = harness::installed_adapter(root, harness::get(harness)?) {
        let version = version.as_deref().unwrap_or("unknown");
        text.push_str(&format!("adapter: {package} {version}\n"));
    }
    if let Some(usage) = &client.context_usage {
        if let (Some(used), Some(size)) = (usage["used"].as_u64(), usage["size"].as_u64()) {
            text.push_str(&format!("context: {used} / {size} tokens\n"));
        }
        if let (Some(cost), Some(currency)) = (
            usage
                .pointer("/cost/amount")
                .and_then(serde_json::Value::as_f64),
            usage
                .pointer("/cost/currency")
                .and_then(serde_json::Value::as_str),
        ) {
            text.push_str(&format!("cost: {cost:.4} {currency}\n"));
        }
    }
    if let Some(usage) = &client.usage {
        for (key, label) in [
            ("inputTokens", "input tokens"),
            ("outputTokens", "output tokens"),
            ("totalTokens", "total tokens"),
        ] {
            if let Some(count) = usage[key].as_u64() {
                text.push_str(&format!("{label}: {count}\n"));
            }
        }
    }
    if client.usage.is_none() && client.context_usage.is_none() {
        text.push_str("usage: not reported by harness\n");
    }
    print_colored(interactive(), LIGHT_GRAY, &text);
    Ok(())
}

async fn change_model(client: &mut Client, root: &Path, harness: &str) -> Result<()> {
    if !change_setting(client, ConfigKind::Model).await? {
        return Ok(());
    }
    save_model(client, root, harness)?;
    change_setting(client, ConfigKind::Thinking).await?;
    save_model(client, root, harness)?;
    print_colored(
        true,
        LIGHT_GRAY,
        &format!(
            "Model: {} · Reasoning: {}\n",
            client
                .current_config_label(ConfigKind::Model)
                .unwrap_or("default"),
            client
                .current_config_label(ConfigKind::Thinking)
                .unwrap_or("default"),
        ),
    );
    Ok(())
}

async fn change_setting(client: &mut Client, kind: ConfigKind) -> Result<bool> {
    let label = match kind {
        ConfigKind::Model => "Select model",
        ConfigKind::Thinking => "Select reasoning level",
    };
    let Some(option) = client.select_config_option(kind) else {
        print_colored(
            true,
            LIGHT_GRAY,
            "The harness does not offer this setting.\n",
        );
        return Ok(true);
    };
    let id = option.id.clone();
    let current = option.current_value.clone();
    let cursor = option.cursor();
    let choices = option.choices.clone();
    let selected =
        match crate::prompts::select(Select::new(label, choices).with_starting_cursor(cursor)) {
            Ok(selected) => selected,
            Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
                return Ok(false)
            }
            Err(error) => return Err(error.into()),
        };
    if selected.value != current {
        client.set_config_option(&id, &selected.value).await?;
    }
    Ok(true)
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
                ConfigKind::Thinking => "reasoning level",
            };
            eprintln!("warning: saved {name} {selection:?} is unavailable; using the default");
        }
    }
    Ok(())
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
    fn runtime_instruction_contains_soul_workspace_rule_and_current_time() {
        let instruction = runtime_instruction("You are a researcher.\n");
        assert!(instruction
            .starts_with("You are a researcher.\n\nUse `workspace/` as your working directory."));
        assert!(instruction.contains("\n\nCurrent Date and Time: "));
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
