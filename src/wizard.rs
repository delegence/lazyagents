use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{anyhow, bail, Context, Result};
use inquire::validator::{StringValidator, Validation};
use inquire::{Confirm, InquireError, Select, Text};
use tokio::process::Command;

use crate::acp::{Client, ConfigKind, StreamEvent};
use crate::config::{paths, AgentConfig};
use crate::harness;
use crate::harness::Harness;
use crate::prompts::{CancellationInput, Cancelled};

const SOUL_WRITER: &str = "You write precise system prompts for small AI agents. Return only the finished Markdown system prompt. Do not use a code fence. Define the agent's identity, purpose, operating principles, boundaries, and response style. Keep it concise and useful. Do not invent tools, skills, files, or facts that were not supplied.";

pub async fn run(root: PathBuf) -> Result<()> {
    match run_setup(root).await {
        Err(error) if error.downcast_ref::<Cancelled>().is_some() => {
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
    print_answer("Harness", selected.display_name())?;
    let name = text("What is your Agent name:", |value: &str| {
        Ok(match make_slug(value) {
            Ok(_) => Validation::Valid,
            Err(error) => Validation::Invalid(error.to_string().into()),
        })
    })?;
    let slug = make_slug(&name)?;
    print_answer("Agent name", &name)?;
    let description = text("Describe what your Agent does:", |value: &str| {
        Ok(if value.trim().is_empty() {
            Validation::Invalid("the agent description cannot be empty".into())
        } else {
            Validation::Valid
        })
    })?;
    print_answer("Agent description", &description)?;

    let files = AgentFiles::read(&root, &slug)?;
    let existing = files.existing_paths();
    if !existing.is_empty() {
        println!("\nSetup will replace:");
        for path in existing {
            println!("  {}", display_path(path));
        }
        if !confirm("Replace these files", false)? {
            return Err(Cancelled.into());
        }
    }
    ensure_authentication(selected).await?;

    let state = paths(&root).state;
    let state_existed = state.exists();
    let workspace = root.join("workspace");
    let workspace_existed = workspace.exists();
    create_structure(&root)?;
    let result = create_agent(&root, selected, &name, &slug, &description).await;
    if let Err(error) = result {
        let cleanup = files.restore().and_then(|()| {
            if !state_existed {
                fs::remove_dir_all(&state)?;
            }
            if !workspace_existed {
                fs::remove_dir_all(&workspace)?;
            }
            Ok(())
        });
        // Report the setup failure, not a later cleanup failure.
        if let Err(cleanup_error) = cleanup {
            eprintln!("warning: could not undo setup changes: {cleanup_error:#}");
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
    println!("Agent Created ./{}", slug);
    let install_globally = optional_confirm("Install a global launcher in ~/.local/bin", false)?;
    print_answer(
        "Install a global launcher in ~/.local/bin",
        if install_globally { "Yes" } else { "No" },
    )?;
    if install_globally {
        install_global_link(&root.join(&slug), &slug)?;
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
    let mut config = AgentConfig::new(name.trim(), slug, description.trim(), selected.id());
    config.color = Some(crate::config::AgentColor::random());
    harness::install_adapter(root, selected).await?;

    let launch = harness::launch(root, &config.harness, SOUL_WRITER)?;
    let input = CancellationInput::new()?;
    let spinner = crate::progress::start("Preparing harness...".to_owned());
    let client = tokio::select! {
        biased;
        result = Client::start(launch, root, Vec::new(), None) => result,
        result = input.cancelled(true) => {
            result.and(Err(Cancelled.into()))
        }
    };
    spinner.finish_and_clear();
    drop(input);
    let mut client = client?;
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
        "Select reasoning level for your Agent:",
    )
    .await?;
    config.thinking.clone_from(&thinking.value);
    show_selection("Reasoning level", &thinking)?;
    let input = CancellationInput::new()?;
    let spinner = crate::progress::start("Creating SOUL.md...".to_owned());
    let soul = tokio::select! {
        biased;
        result = generate_soul(&mut client, &config) => result,
        result = input.cancelled(true) => {
            match result {
                Ok(()) => {
                    // Close the adapter below even if it ignores cancellation.
                    let _ = client.cancel_prompt(|_| Ok(None)).await;
                    Err(Cancelled.into())
                }
                Err(error) => Err(error),
            }
        }
    };
    client.close().await;
    spinner.finish_and_clear();
    drop(input);
    let soul = soul?;
    let agent_paths = paths(root);
    fs::write(&agent_paths.soul, format!("{}\n", soul.trim()))
        .with_context(|| format!("could not write {}", agent_paths.soul.display()))?;
    config.save(root)?;
    write_launcher(&root.join(&config.slug))?;
    Ok(())
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
    write_if_missing(&agent_paths.mcp, b"[]\n")?;
    write_if_missing(
        &agent_paths.state.join(".gitignore"),
        b"agent.json\nSOUL.md\nmcps.json\nruntime/\nsessions/\n",
    )?;
    Ok(())
}

fn write_if_missing(path: &Path, contents: &[u8]) -> Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file
            .write_all(contents)
            .with_context(|| format!("could not write {}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error).with_context(|| format!("could not create {}", path.display())),
    }
}

struct AgentFiles {
    files: Vec<AgentFile>,
}

struct AgentFile {
    path: PathBuf,
    original: Option<(Vec<u8>, fs::Permissions)>,
}

impl AgentFiles {
    fn read(root: &Path, slug: &str) -> Result<Self> {
        let agent_paths = paths(root);
        let mut files = Vec::new();
        for path in [agent_paths.config, agent_paths.soul, root.join(slug)] {
            let original = match fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    if !metadata.is_file() {
                        bail!(
                            "{} is not a regular file; move it before initialization",
                            path.display()
                        );
                    }
                    Some((fs::read(&path)?, metadata.permissions()))
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("could not inspect {}", path.display()))
                }
            };
            files.push(AgentFile { path, original });
        }
        Ok(Self { files })
    }

    fn existing_paths(&self) -> Vec<&Path> {
        self.files
            .iter()
            .filter_map(|file| file.original.as_ref().map(|_| file.path.as_path()))
            .collect()
    }

    fn restore(&self) -> Result<()> {
        for file in &self.files {
            let path = &file.path;
            match &file.original {
                Some((contents, permissions)) => {
                    fs::write(path, contents)?;
                    fs::set_permissions(path, permissions.clone())?;
                }
                None => match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                },
            }
        }
        Ok(())
    }
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
        Err(error) => bail!(
            "{}\n{error:#}\n{}\n{system_error}",
            client.friendly_error(&error, &system_error),
            client.diagnostics().join("\n")
        ),
    };
    if soul.trim().is_empty() {
        bail!("the harness returned an empty SOUL.md");
    }
    Ok(soul)
}

fn print_answer(label: &str, value: &str) -> Result<()> {
    if io::stdout().is_terminal() {
        println!("> {label}: \x1b[1m{value}\x1b[0m");
    } else {
        println!("> {label}: {value}");
    }
    io::stdout().flush()?;
    Ok(())
}

fn show_selection(label: &str, selection: &WizardSelection) -> Result<()> {
    print_answer(label, &selection.label)
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
    let label = "Select harness runtime";
    let selected = inquire(label, || {
        Select::new(label, names)
            .with_formatter(&|_| String::new())
            .prompt()
    })?;
    available
        .iter()
        .copied()
        .find(|harness| harness.display_name() == selected)
        .context("selected harness is unavailable")
}

fn text(label: &str, validator: impl StringValidator + 'static) -> Result<String> {
    inquire(label, || {
        Text::new(label)
            .with_validator(validator)
            .with_formatter(&|_| String::new())
            .prompt()
    })
    .map(|value| value.trim().to_owned())
}

fn confirm(label: &str, default: bool) -> Result<bool> {
    inquire(label, || {
        Confirm::new(label)
            .with_default(default)
            .with_formatter(&|_| String::new())
            .prompt()
    })
}

async fn choose_option(
    client: &mut Client,
    kind: ConfigKind,
    label: &str,
) -> Result<WizardSelection> {
    let Some(option) = client.select_config_option(kind) else {
        return Ok(WizardSelection {
            value: None,
            label: "default".to_owned(),
        });
    };
    let id = option.id.clone();
    let cursor = option.cursor();
    let choices = option.choices.clone();
    let selected = inquire(label, || {
        Select::new(label, choices)
            .with_starting_cursor(cursor)
            .with_formatter(&|_| String::new())
            .prompt()
    })?;
    client.set_config_option(&id, &selected.value).await?;
    Ok(WizardSelection {
        value: Some(selected.value),
        label: selected.label,
    })
}

struct WizardSelection {
    value: Option<String>,
    label: String,
}

fn inquire<T>(
    message: &str,
    prompt: impl FnOnce() -> std::result::Result<T, InquireError>,
) -> Result<T> {
    let result = crate::prompts::run(message, prompt);
    match result {
        Ok(value) => Ok(value),
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            Err(Cancelled.into())
        }
        Err(error) => Err(error.into()),
    }
}

fn optional_confirm(label: &str, default: bool) -> Result<bool> {
    match confirm(label, default) {
        Ok(value) => Ok(value),
        Err(error) if error.downcast_ref::<Cancelled>().is_some() => Ok(false),
        Err(error) => Err(error),
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
    fs::set_permissions(destination, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn install_global_link(launcher: &Path, slug: &str) -> Result<()> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")?;
    let bin = home.join(".local/bin");
    fs::create_dir_all(&bin)?;
    let link = bin.join(slug);
    if link.symlink_metadata().is_ok() {
        bail!(
            "{} already exists; the local launcher is still ready",
            link.display()
        );
    }
    let launcher = launcher.canonicalize()?;
    std::os::unix::fs::symlink(&launcher, &link)?;
    println!("Linked {}", link.display());
    Ok(())
}

fn make_slug(value: &str) -> Result<String> {
    let mut slug = String::new();
    let mut separator = false;

    for character in value.trim().chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }

    if slug.is_empty() {
        bail!("the agent name must contain at least one letter or number");
    }
    // The launcher shares the agent folder with the working directory.
    if slug == "workspace" {
        bail!("the agent name \"workspace\" is reserved; choose another name");
    }
    Ok(slug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn initializes_with_existing_skills_and_preserves_support_files() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let agent_paths = paths(root);
        fs::create_dir_all(agent_paths.skills.join("job-search")).unwrap();
        fs::write(agent_paths.skills.join("job-search/SKILL.md"), "Find jobs").unwrap();
        fs::write(&agent_paths.mcp, "[{\"name\":\"jobs\"}]\n").unwrap();
        fs::write(agent_paths.state.join(".gitignore"), "custom\n").unwrap();

        assert!(AgentFiles::read(root, "attache")
            .unwrap()
            .existing_paths()
            .is_empty());
        create_structure(root).unwrap();

        assert_eq!(
            fs::read_to_string(agent_paths.skills.join("job-search/SKILL.md")).unwrap(),
            "Find jobs"
        );
        assert_eq!(
            fs::read_to_string(agent_paths.mcp).unwrap(),
            "[{\"name\":\"jobs\"}]\n"
        );
        assert_eq!(
            fs::read_to_string(agent_paths.state.join(".gitignore")).unwrap(),
            "custom\n"
        );
        assert!(agent_paths.sessions.is_dir());
    }

    #[test]
    fn detects_each_file_that_setup_will_replace() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        create_structure(root).unwrap();
        let agent_paths = paths(root);
        let launcher = root.join("attache");
        for path in [&agent_paths.config, &agent_paths.soul, &launcher] {
            fs::write(path, "existing").unwrap();
            assert!(AgentFiles::read(root, "attache")
                .unwrap()
                .existing_paths()
                .contains(&path.as_path()));
            fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn restores_replaced_files_and_removes_partial_setup_files() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        create_structure(root).unwrap();
        let agent_paths = paths(root);
        fs::write(&agent_paths.config, "old config").unwrap();
        fs::write(&agent_paths.soul, "old soul").unwrap();
        fs::write(agent_paths.sessions.join("history.jsonl"), "history").unwrap();
        let original = AgentFiles::read(root, "attache").unwrap();
        fs::write(&agent_paths.config, "new config").unwrap();
        fs::write(&agent_paths.soul, "new soul").unwrap();
        write_launcher(&root.join("attache")).unwrap();

        original.restore().unwrap();

        assert_eq!(
            fs::read_to_string(agent_paths.config).unwrap(),
            "old config"
        );
        assert_eq!(fs::read_to_string(agent_paths.soul).unwrap(), "old soul");
        assert!(!root.join("attache").exists());
        assert_eq!(
            fs::read_to_string(agent_paths.sessions.join("history.jsonl")).unwrap(),
            "history"
        );
    }

    #[test]
    fn rejects_a_launcher_symlink_without_changing_its_target() {
        let directory = tempdir().unwrap();
        let target = directory.path().join("target");
        fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, directory.path().join("attache")).unwrap();
        assert!(AgentFiles::read(directory.path(), "attache").is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "keep");
    }

    #[test]
    fn writes_a_small_launcher_script() {
        let directory = tempdir().unwrap();
        let launcher = directory.path().join("test-agent");
        write_launcher(&launcher).unwrap();
        let script = fs::read_to_string(&launcher).unwrap();
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("exec lazyagents chat \"$@\""));
        assert_ne!(
            fs::metadata(launcher).unwrap().permissions().mode() & 0o111,
            0
        );
    }
    #[test]
    fn creates_a_safe_launcher_name() {
        assert_eq!(make_slug(" Research  Agent! ").unwrap(), "research-agent");
        assert_eq!(make_slug("Agent 42").unwrap(), "agent-42");
    }

    #[test]
    fn rejects_an_empty_or_reserved_name() {
        assert!(make_slug(" -- ").is_err());
        assert!(make_slug("Workspace").is_err());
    }
}
