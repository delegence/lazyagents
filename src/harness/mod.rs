use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::process::Command;

use crate::config::paths;

mod claude;
mod codex;
mod opencode;
mod pi;

#[derive(Clone, Copy)]
pub struct AdapterSpec {
    pub package: &'static str,
    pub version: &'static str,
    pub binary: &'static str,
}

#[derive(Clone, Copy)]
pub struct Authentication {
    pub status_args: &'static [&'static str],
    pub login_args: &'static [&'static str],
    pub api_key_variables: &'static [&'static str],
}

pub struct LaunchContext<'a> {
    pub root: &'a Path,
    pub runtime_path: &'a Path,
    pub instruction: &'a str,
}

#[derive(Default)]
pub struct LaunchOptions {
    pub args: Vec<OsString>,
    pub env: BTreeMap<String, String>,
    pub session_meta: Option<Value>,
}

#[derive(Debug)]
pub struct Launch {
    pub runtime_name: &'static str,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: BTreeMap<String, String>,
    pub session_meta: Option<Value>,
}

/// All provider-specific behavior needed to run one ACP harness.
pub trait Harness: Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn runtime_command(&self) -> &'static str;
    fn adapter(&self) -> Option<AdapterSpec>;
    fn authentication(&self) -> Option<Authentication>;
    fn launch_options(&self, context: LaunchContext<'_>) -> Result<LaunchOptions>;
}

const HARNESSES: &[&dyn Harness] = &[&codex::CODEX, &claude::CLAUDE, &opencode::OPENCODE, &pi::PI];

pub fn get(id: &str) -> Result<&'static dyn Harness> {
    HARNESSES
        .iter()
        .copied()
        .find(|harness| harness.id() == id)
        .with_context(|| format!("unsupported harness {id:?}"))
}

pub fn installed() -> Vec<&'static dyn Harness> {
    HARNESSES
        .iter()
        .copied()
        .filter(|harness| find_command(harness.runtime_command()).is_some())
        .collect()
}

pub fn find_command(name: &str) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 && candidate.is_file() {
        return Some(candidate.to_path_buf());
    }
    env::var_os("PATH").and_then(|path| {
        env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

pub(super) fn local_skills(directory: &Path) -> Result<Vec<String>> {
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut skills = fs::read_dir(directory)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().join("SKILL.md").is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    skills.sort();
    Ok(skills)
}

pub(super) fn link_authentication(source: Option<PathBuf>, destination: &Path) -> Result<()> {
    if destination.exists() {
        return Ok(());
    }
    let Some(source) = source.filter(|source| source.is_file()) else {
        return Ok(());
    };

    #[cfg(unix)]
    std::os::unix::fs::symlink(&source, destination)
        .with_context(|| format!("could not link authentication from {}", source.display()))?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&source, destination)
        .with_context(|| format!("could not link authentication from {}", source.display()))?;
    Ok(())
}

pub async fn install_adapter(root: &Path, harness: &dyn Harness) -> Result<()> {
    let Some(adapter) = harness.adapter() else {
        return Ok(());
    };
    let npm = find_command("npm").context("npm is required to install the ACP adapter")?;
    let runtime = paths(root).runtime.join(format!("acp-{}", harness.id()));
    fs::create_dir_all(&runtime)?;
    let package = format!("{}@{}", adapter.package, adapter.version);

    let input = crate::prompts::CancellationInput::new()?;
    let spinner = crate::progress::start("Preparing harness...".to_owned());
    let output = async {
        let mut command = Command::new(npm);
        command
            .args([
                "install",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--package-lock=false",
                "--no-save",
                "--prefix",
            ])
            .arg(&runtime)
            .arg(&package)
            .env("NPM_CONFIG_MIN_RELEASE_AGE", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let child = command.spawn().context("could not run npm")?;
        #[cfg(unix)]
        let pid = child.id();
        let output = child.wait_with_output();
        tokio::pin!(output);
        tokio::select! {
            biased;
            result = &mut output => result.context("could not run npm"),
            result = input.cancelled(true) => {
                #[cfg(unix)]
                {
                    // Stop npm and its children before setup removes their files.
                    if let Some(pid) = pid {
                        unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL); }
                    }
                    let _ = output.await;
                }
                result?;
                Err(crate::prompts::Cancelled.into())
            }
        }
    }
    .await;
    spinner.finish_and_clear();
    drop(input);
    let output = output?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        if detail.is_empty() {
            bail!("npm could not install {package}");
        }
        bail!("npm could not install {package}: {detail}");
    }
    Ok(())
}

fn adapter_path(root: &Path, harness: &dyn Harness) -> Option<PathBuf> {
    harness.adapter().map(|adapter| {
        paths(root)
            .runtime
            .join(format!("acp-{}", harness.id()))
            .join("node_modules/.bin")
            .join(adapter.binary)
    })
}

/// Installs a missing adapter only after an interactive first-use choice.
pub async fn ensure_adapter(root: &Path, harness: &dyn Harness) -> Result<()> {
    find_command(harness.runtime_command()).with_context(|| {
        format!(
            "{} is not installed or is not on PATH",
            harness.display_name()
        )
    })?;
    if adapter_path(root, harness).is_none_or(|path| path.is_file()) {
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("the ACP adapter is missing; run `lazyagents repair` before using chat in a pipe");
    }
    let install = inquire::Confirm::new("Install the missing ACP adapter now")
        .with_default(true)
        .prompt()?;
    if !install {
        bail!("chat needs the ACP adapter; run `lazyagents repair` when ready");
    }
    install_adapter(root, harness).await
}

pub fn launch(root: &Path, harness_id: &str, instruction: &str) -> Result<Launch> {
    let harness = get(harness_id)?;
    let runtime_path = find_command(harness.runtime_command()).with_context(|| {
        format!(
            "{} is no longer installed or is not on PATH",
            harness.display_name()
        )
    })?;
    let mut options = harness.launch_options(LaunchContext {
        root,
        runtime_path: &runtime_path,
        instruction,
    })?;
    let program = if let Some(adapter_path) = adapter_path(root, harness) {
        if !adapter_path.is_file() {
            bail!(
                "the ACP adapter is missing; run `lazyagents repair` in {}",
                root.display()
            );
        }
        options.args.insert(0, adapter_path.into_os_string());
        find_command("node").context("node is required to run the ACP adapter")?
    } else {
        runtime_path
    };
    Ok(Launch {
        runtime_name: harness.display_name(),
        program,
        args: options.args,
        env: options.env,
        session_meta: options.session_meta,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn registry_ids_are_unique_and_resolvable() {
        let ids = HARNESSES
            .iter()
            .map(|harness| harness.id())
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), HARNESSES.len());
        for id in ids {
            assert_eq!(get(id).unwrap().id(), id);
        }
    }

    #[tokio::test]
    async fn native_acp_harnesses_do_not_install_an_adapter() {
        let root = tempfile::tempdir().unwrap();
        install_adapter(root.path(), &opencode::OPENCODE)
            .await
            .unwrap();
        assert!(!paths(root.path()).runtime.join("acp-opencode").exists());
    }
}
