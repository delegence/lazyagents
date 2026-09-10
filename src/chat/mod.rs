mod commands;
mod editor;
mod input;
mod layout;
mod output;

use self::commands::Command;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
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
use crate::session::{SessionEntry, SessionLog, SessionSummary};

use self::output::{
    compact_line, detail_lines, display_width, interactive, print_colored, render_history,
    render_user_prompt, TurnDisplay, LIGHT_GRAY,
};

pub async fn run(root: PathBuf, resume: Option<Option<String>>) -> Result<()> {
    let mut config = AgentConfig::load(&root)?;
    let agent_paths = paths(&root);
    let soul = fs::read_to_string(&agent_paths.soul)
        .with_context(|| format!("could not read {}", agent_paths.soul.display()))?;
    let instruction = runtime_instruction(&soul);
    let servers = mcp::load(&agent_paths.mcp)?;
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
    let launch = harness::launch(&root, &config.harness, &instruction)?;
    let history = match &session_id {
        Some(session_id) => SessionLog::messages(&agent_paths.sessions, session_id)?,
        None => Vec::new(),
    };
    let spinner = crate::progress::start(format!(
        "Starting {}...",
        crate::markdown::sanitize(&config.name)
    ));
    let client = Client::start(launch, &root, servers, session_id.as_deref()).await;
    spinner.finish_and_clear();
    let mut client = client?;
    client.remember_permissions(&root, &config.harness);
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
    let mut log = None;
    let model = client
        .current_config_label(ConfigKind::Model)
        .unwrap_or("default");
    let thinking = client
        .current_config_label(ConfigKind::Thinking)
        .unwrap_or("default");
    let name_color = config.color.unwrap_or(crate::config::AgentColor::Cyan);
    let header = ChatHeader::new(
        &config.name,
        name_color,
        &config.harness,
        model,
        thinking,
        config.permission_mode,
    );
    let defer_header = interactive() && history.is_empty();
    let header = if defer_header {
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
        let command = Command::parse(prompt.trim());
        if command.is_some() {
            input.accepted()?;
        }
        if command == Some(Command::Quit) {
            client.close().await;
            return Ok(());
        }
        if command == Some(Command::Status) {
            print_status(&client, &root, &config.harness)?;
            continue;
        }
        if command == Some(Command::New) {
            let model = client
                .select_config_option(ConfigKind::Model)
                .map(|option| option.current_value.clone());
            let thinking = client
                .select_config_option(ConfigKind::Thinking)
                .map(|option| option.current_value.clone());
            let soul = fs::read_to_string(&agent_paths.soul)?;
            let launch = harness::launch(&root, &config.harness, &runtime_instruction(&soul))?;
            let spinner = crate::progress::start(format!(
                "Starting {}...",
                crate::markdown::sanitize(&config.name)
            ));
            let fresh = Client::start(launch, &root, mcp::load(&agent_paths.mcp)?, None).await;
            spinner.finish_and_clear();
            let mut fresh = fresh?;
            fresh.remember_permissions(&root, &config.harness);
            apply_selection(&mut fresh, ConfigKind::Model, model.as_deref()).await?;
            apply_selection(&mut fresh, ConfigKind::Thinking, thinking.as_deref()).await?;
            client.close().await;
            client = fresh;
            log = None;
            if interactive() {
                let header = ChatHeader::new(
                    &config.name,
                    name_color,
                    &config.harness,
                    client
                        .current_config_label(ConfigKind::Model)
                        .unwrap_or("default"),
                    client
                        .current_config_label(ConfigKind::Thinking)
                        .unwrap_or("default"),
                    AgentConfig::load(&root)?.permission_mode,
                );
                io::stdout().flush()?;
                print_colored(true, LIGHT_GRAY, "\n\nNew session started\n");
                eprint!("{}", header.render(true));
                io::stderr().flush()?;
            } else {
                print_colored(false, LIGHT_GRAY, "New session started\n");
            }
            continue;
        }
        if command == Some(Command::Permissions) {
            manage_permissions(&root)?;
            continue;
        }
        if command == Some(Command::Model) {
            if !interactive() {
                bail!("{} needs an interactive terminal", prompt.trim());
            }
            if let Err(error) = change_model(&mut client, &root, &config.harness).await {
                if error.downcast_ref::<crate::acp::ProtocolError>().is_some() {
                    print_colored(
                        true,
                        "31",
                        &format!("{}\n", client.friendly_error(&error, "")),
                    );
                } else {
                    return Err(error);
                }
            }
            continue;
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
        input.accepted()?;
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
        let interrupted = answer.as_ref().is_some_and(|result| {
            result
                .as_ref()
                .err()
                .and_then(|error| error.downcast_ref::<InquireError>())
                .is_some_and(|error| matches!(error, InquireError::OperationInterrupted))
        });
        if answer.is_none() || interrupted {
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
            continue;
        }
        match answer.expect("the interrupted prompt returned above") {
            Ok(_) => {}
            Err(error) => {
                display.finish();
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
                        continue;
                    }
                    client.close().await;
                    return Err(error);
                }
                if error.downcast_ref::<crate::acp::ProtocolError>().is_some() {
                    print_colored(interactive(), "31", &format!("✗ {friendly}\n"));
                    continue;
                }
                client.close().await;
                return Err(error);
            }
        };
        display.finish();
        log.append(&SessionEntry::TurnEnd)?;
    }
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

fn manage_permissions(root: &std::path::Path) -> Result<()> {
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

fn save_model(client: &Client, root: &std::path::Path, harness: &str) -> Result<()> {
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

fn print_status(client: &Client, root: &std::path::Path, harness: &str) -> Result<()> {
    let config = AgentConfig::load(root)?;
    let mut text = format!(
        "session: {}\nharness: {harness}\nmodel: {}\nreasoning: {}\npermissions: {}\n",
        client.session_id(),
        client
            .current_config_label(ConfigKind::Model)
            .unwrap_or("default")
            .to_lowercase(),
        client
            .current_config_label(ConfigKind::Thinking)
            .unwrap_or("default"),
        config.permission_mode
    );
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

async fn change_model(client: &mut Client, root: &std::path::Path, harness: &str) -> Result<()> {
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
                .unwrap_or("default")
                .to_lowercase(),
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
    let cursor = option
        .choices
        .iter()
        .position(|choice| choice.value == current)
        .unwrap_or_default();
    let choices = option
        .choices
        .iter()
        .cloned()
        .map(|mut choice| {
            if matches!(kind, ConfigKind::Model) {
                choice.label = choice.label.to_lowercase();
            }
            SettingChoice(choice)
        })
        .collect();
    let selected =
        match crate::prompts::select(Select::new(label, choices).with_starting_cursor(cursor)) {
            Ok(selected) => selected.0,
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

struct SettingChoice(crate::acp::ConfigChoice);

impl std::fmt::Display for SettingChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&crate::markdown::sanitize(&self.0.label))
    }
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
