use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use inquire::{Confirm, InquireError, Select, Text};
use tokio::process::Command;

use crate::acp::{Client, ConfigChoice, ConfigKind, StreamEvent};
use crate::config::{paths, AgentConfig};
use crate::harness::Harness;
use crate::{harness, mcp, slug};

const SOUL_WRITER: &str = "You write precise system prompts for small AI agents. Return only the finished Markdown system prompt. Do not use a code fence. Define the agent's identity, purpose, operating principles, boundaries, and response style. Keep it concise and useful. Do not invent tools, skills, files, or facts that were not supplied.";

pub async fn run(root: PathBuf) -> Result<()> {
    match run_setup(root).await {
        Err(error) if error.downcast_ref::<SetupCancelled>().is_some() => {
            println!("Setup cancelled.");
            Ok(())
        }
        result => result,
    }
}

async fn run_setup(root: PathBuf) -> Result<()> {
    let available = harness::installed();
    if available.is_empty() {
        bail!("no supported harness is installed; install a supported runtime first");
    }

    println!("Create an Agent in {}\n", display_path(&root));
    let selected = select_harness(&available)?;
    replace_answer("Harness", selected.display_name())?;
    let name = text("What is your Agent name:")?;
    let slug = slug::make(&name)?;
    replace_answer("Agent name", &name)?;
    let description = text("Describe what your Agent does:")?;
    if description.trim().is_empty() {
        bail!("the agent description cannot be empty");
    }
    replace_answer("Agent description", &description)?;

    let agent_paths = paths(&root);
    let launcher = root.join(&slug);
    if agent_paths.state.exists() || agent_paths.soul.exists() || launcher.exists() {
        bail!(
            "agent files already exist in {}; remove or move them before initialization",
            root.display()
        );
    }
    ensure_authentication(selected).await?;

    let workspace = root.join("workspace");
    let workspace_existed = workspace.exists();
    create_structure(&root)?;
    let result = create_agent(&root, selected, &name, &slug, &description).await;
    match result {
        Ok(()) => {}
        Err(error) => {
            let _ = fs::remove_dir_all(&agent_paths.state);
            let _ = fs::remove_file(&launcher);
            if !workspace_existed {
                let _ = fs::remove_dir_all(&workspace);
            }
            if error.downcast_ref::<crate::acp::ProtocolError>().is_some() {
                return Err(anyhow!(crate::acp::friendly_error(
                    selected.display_name(),
                    &error,
                    "",
                    &[]
                )));
            }
            return Err(error);
        }
    }
    println!("Agent Created ./{}", slug);
    let install_globally = optional_confirm("Install a global launcher in ~/.local/bin", false)?;
    replace_answer(
        "Install a global launcher in ~/.local/bin",
        if install_globally { "Yes" } else { "No" },
    )?;
    if install_globally {
        install_global_link(&launcher, &slug)?;
    }

    println!("\nReady. Run ./{}", slug);
    Ok(())
}

async fn create_agent(
    root: &Path,
    selected: &dyn Harness,
    name: &str,
    slug: &str,
    description: &str,
) -> Result<()> {
    let mut config = AgentConfig {
        name: name.trim().to_owned(),
        slug: slug.to_owned(),
        description: description.trim().to_owned(),
        harness: selected.id().to_owned(),
        model: None,
        thinking: None,
    };
    harness::install_adapter(root, selected).await?;

    let launch = harness::launch(root, &config.harness, SOUL_WRITER)?;
    let mut client = Client::start(launch, root, Vec::new(), None).await?;
    let model = choose_option(
        &mut client,
        ConfigKind::Model,
        "Select model for your Agent:",
    )
    .await?;
    config.model.clone_from(&model.value);
    show_selection("Model", &model)?;

    let thinking = choose_option(
        &mut client,
        ConfigKind::Thinking,
        "Select thinking level for your Agent:",
    )
    .await?;
    config.thinking.clone_from(&thinking.value);
    show_selection("Thinking level", &thinking)?;

    println!();
    let spinner = start_spinner("Creating SOUL of your Agent...");
    let soul = generate_soul(&mut client, &config).await;
    client.close().await;
    if let Some(spinner) = spinner {
        spinner.finish_and_clear();
    }
    let soul = soul?;
    let agent_paths = paths(root);
    fs::write(&agent_paths.soul, format!("{}\n", soul.trim()))
        .with_context(|| format!("could not write {}", agent_paths.soul.display()))?;
    config.save(root)?;
    write_launcher(&root.join(&config.slug))?;
    Ok(())
}

fn start_spinner(message: &'static str) -> Option<ProgressBar> {
    if !io::stderr().is_terminal() {
        println!("{message}");
        return None;
    }
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::with_template("{spinner} {msg}")
            .expect("the static spinner template must be valid")
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    spinner.set_message(message);
    spinner.enable_steady_tick(Duration::from_millis(80));
    Some(spinner)
}

async fn ensure_authentication(harness: &dyn Harness) -> Result<()> {
    let Some(authentication) = harness.authentication() else {
        return Ok(());
    };
    if authentication
        .api_key_variables
        .iter()
        .any(|name| env::var_os(name).is_some_and(|value| !value.is_empty()))
    {
        return Ok(());
    }
    let executable = harness::find_command(harness.runtime_command())
        .with_context(|| format!("{} is no longer available", harness.display_name()))?;
    let authenticated = Command::new(&executable)
        .args(authentication.status_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false);
    if authenticated {
        return Ok(());
    }

    if !confirm(
        &format!("{} is not signed in. Sign in now", harness.display_name()),
        true,
    )? {
        bail!("{} authentication is required", harness.display_name());
    }
    let status = Command::new(executable)
        .args(authentication.login_args)
        .status()
        .await
        .with_context(|| format!("could not start {} login", harness.display_name()))?;
    if !status.success() {
        bail!("{} login did not complete", harness.display_name());
    }
    Ok(())
}

fn create_structure(root: &Path) -> Result<()> {
    let agent_paths = paths(root);
    fs::create_dir_all(root.join("workspace"))?;
    fs::create_dir_all(&agent_paths.skills)?;
    fs::create_dir_all(&agent_paths.runtime)?;
    fs::create_dir_all(&agent_paths.sessions)?;
    fs::write(&agent_paths.mcp, mcp::template())?;
    fs::write(
        agent_paths.state.join(".gitignore"),
        b"agent.json\nSOUL.md\nmcps.json\nruntime/\nsessions/\n",
    )?;
    Ok(())
}

async fn generate_soul(client: &mut Client, config: &AgentConfig) -> Result<String> {
    let request = format!(
        "Create the system prompt for this agent.\n\nName: {}\nDescription: {}",
        config.name, config.description
    );
    let mut system_error = String::new();
    let soul = match client
        .prompt(&request, |event| {
            if let StreamEvent::SystemError(text) = event {
                system_error.push_str(&text);
            }
            Ok(None)
        })
        .await
    {
        Ok(soul) => soul,
        Err(error) => bail!(client.friendly_error(&error, &system_error)),
    };
    if soul.trim().is_empty() {
        bail!("the harness returned an empty SOUL.md");
    }
    Ok(soul)
}

fn replace_answer(label: &str, value: &str) -> Result<()> {
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        print!("\x1b[1A\r\x1b[2K> {label}: {value}\n");
    } else {
        println!("> {label}: {value}");
    }
    io::stdout().flush()?;
    Ok(())
}

fn show_selection(label: &str, selection: &WizardSelection) -> Result<()> {
    if selection.prompted {
        replace_answer(label, &selection.label)
    } else {
        println!("> {label}: {}", selection.label);
        Ok(())
    }
}

fn display_path(path: &Path) -> String {
    let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
        return path.display().to_string();
    };
    match path.strip_prefix(&home) {
        Ok(relative) if relative.as_os_str().is_empty() => "~".to_owned(),
        Ok(relative) => format!("~/{}", relative.display()),
        Err(_) => path.display().to_string(),
    }
}

fn select_harness(available: &[&'static dyn Harness]) -> Result<&'static dyn Harness> {
    let names = available
        .iter()
        .map(|harness| harness.display_name())
        .collect();
    let selected = inquire(Select::new("Select harness runtime", names).prompt())?;
    available
        .iter()
        .copied()
        .find(|harness| harness.display_name() == selected)
        .context("selected harness is unavailable")
}

fn text(label: &str) -> Result<String> {
    inquire(Text::new(label).prompt()).map(|value| value.trim().to_owned())
}

fn confirm(label: &str, default: bool) -> Result<bool> {
    inquire(Confirm::new(label).with_default(default).prompt())
}

async fn choose_option(
    client: &mut Client,
    kind: ConfigKind,
    label: &str,
) -> Result<WizardSelection> {
    let Some(option) = client.select_config_option(kind) else {
        return Ok(WizardSelection {
            value: None,
            label: "Default".to_owned(),
            prompted: false,
        });
    };
    let choices = option.choices.into_iter().map(Choice).collect::<Vec<_>>();
    let cursor = choices
        .iter()
        .position(|choice| choice.0.value == option.current_value)
        .unwrap_or_default();
    let selected = inquire(
        Select::new(label, choices)
            .with_starting_cursor(cursor)
            .prompt(),
    )?
    .0;
    client
        .set_config_option(&option.id, &selected.value)
        .await?;
    Ok(WizardSelection {
        value: Some(selected.value),
        label: selected.label,
        prompted: true,
    })
}

struct WizardSelection {
    value: Option<String>,
    label: String,
    prompted: bool,
}

struct Choice(ConfigChoice);

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = crate::markdown::sanitize(&self.0.label);
        match &self.0.description {
            Some(description) if !description.is_empty() => {
                write!(
                    formatter,
                    "{} - {}",
                    label,
                    crate::markdown::sanitize(description)
                )
            }
            _ => formatter.write_str(&label),
        }
    }
}

fn inquire<T>(result: std::result::Result<T, InquireError>) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            Err(SetupCancelled.into())
        }
        Err(error) => Err(error.into()),
    }
}

#[derive(Debug)]
struct SetupCancelled;

impl fmt::Display for SetupCancelled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("setup cancelled")
    }
}

impl Error for SetupCancelled {}

fn optional_confirm(label: &str, default: bool) -> Result<bool> {
    match Confirm::new(label).with_default(default).prompt() {
        Ok(value) => Ok(value),
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn write_launcher(destination: &Path) -> Result<()> {
    let script = r##"#!/bin/sh
set -eu

launcher=$0
while [ -L "$launcher" ]; do
  launcher_dir=$(CDPATH= cd -- "$(dirname -- "$launcher")" && pwd)
  link=$(readlink "$launcher")
  case "$link" in
    /*) launcher=$link ;;
    *) launcher=$launcher_dir/$link ;;
  esac
done

agent_dir=$(CDPATH= cd -- "$(dirname -- "$launcher")" && pwd)
cd "$agent_dir"
exec lazyagents chat "$@"
"##;
    fs::write(destination, script)
        .with_context(|| format!("could not write launcher {}", destination.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(destination)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(destination, permissions)?;
    }
    Ok(())
}

fn install_global_link(launcher: &Path, slug: &str) -> Result<()> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")?;
    let bin = home.join(".local/bin");
    fs::create_dir_all(&bin)?;
    let link = bin.join(slug);
    if link.exists() || link.symlink_metadata().is_ok() {
        bail!(
            "{} already exists; the local launcher is still ready",
            link.display()
        );
    }
    let launcher = launcher.canonicalize()?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&launcher, &link)?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&launcher, &link)?;
    println!("Linked {}", link.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn writes_a_small_launcher_script() {
        let directory = tempdir().unwrap();
        let launcher = directory.path().join("test-agent");
        write_launcher(&launcher).unwrap();
        let script = fs::read_to_string(&launcher).unwrap();
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("exec lazyagents chat \"$@\""));
        #[cfg(unix)]
        assert_ne!(
            fs::metadata(launcher).unwrap().permissions().mode() & 0o111,
            0
        );
    }
}
