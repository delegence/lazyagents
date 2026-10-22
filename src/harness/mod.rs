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
    /// The oldest release known to work. Setup and repair install the latest release.
    pub min_version: &'static str,
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

    /// Saved approvals match the tool name instead of the exact request.
    fn approves_tool_names(&self) -> bool {
        false
    }

    /// The adapter's own "always allow" choice lasts only for the session.
    fn always_allow_is_session_only(&self) -> bool {
        false
    }
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

    std::os::unix::fs::symlink(&source, destination)
        .with_context(|| format!("could not link authentication from {}", source.display()))
}

/// Installs the latest adapter release and returns its version.
pub async fn install_adapter(root: &Path, harness: &dyn Harness) -> Result<Option<String>> {
    let Some(adapter) = harness.adapter() else {
        return Ok(None);
    };
    let npm = find_command("npm").context("npm is required to install the ACP adapter")?;
    let runtime = adapter_runtime(root, harness);
    fs::create_dir_all(paths(root).runtime)?;
    let temporary =
        paths(root)
            .runtime
            .join(format!(".acp-{}-{}", harness.id(), uuid::Uuid::now_v7()));
    fs::create_dir(&temporary)?;
    let temporary = TemporaryAdapter(temporary);
    let package = format!("{}@latest", adapter.package);

    let input = crate::prompts::CancellationInput::new()?;
    let spinner = crate::progress::start("Preparing harness...".to_owned());
    let output = async {
        let child = npm_install(&npm, &temporary.0, &package)
            .spawn()
            .context("could not run npm")?;
        let pid = child.id();
        let output = child.wait_with_output();
        tokio::pin!(output);
        tokio::select! {
            biased;
            result = &mut output => result.context("could not run npm"),
            result = input.cancelled(true) => {
                // Stop npm and its children before setup removes their files.
                if let Some(pid) = pid {
                    unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL); }
                }
                let _ = output.await;
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
    let version = installed_version(&temporary.0, adapter.package)?;
    if !meets_floor(&version, adapter.min_version) {
        bail!(
            "npm installed {} {version}, but lazyagents needs {} or newer",
            adapter.package,
            adapter.min_version
        );
    }
    let binary = temporary.0.join("node_modules/.bin").join(adapter.binary);
    if !binary.is_file() {
        bail!(
            "npm did not install the ACP adapter binary {}",
            binary.display()
        );
    }
    replace_adapter(&temporary.0, &runtime)?;
    Ok(Some(version))
}

struct TemporaryAdapter(PathBuf);

impl Drop for TemporaryAdapter {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn replace_adapter(temporary: &Path, runtime: &Path) -> Result<()> {
    // Keep the old adapter outside the temporary directory so cleanup cannot
    // delete it if restoring it also fails.
    let previous = temporary.with_extension("previous");
    let had_previous = match fs::rename(runtime, &previous) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error).context("could not back up the ACP adapter"),
    };
    if let Err(error) = fs::rename(temporary, runtime) {
        if had_previous {
            fs::rename(&previous, runtime).with_context(|| {
                format!(
                    "could not replace the ACP adapter ({error}) or restore it; the previous adapter is at {}",
                    previous.display()
                )
            })?;
        }
        return Err(error).context("could not replace the ACP adapter");
    }
    if had_previous {
        fs::remove_dir_all(&previous).context("could not remove the previous ACP adapter")?;
    }
    Ok(())
}

fn npm_install(npm: &Path, runtime: &Path, package: &str) -> Command {
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
        .arg(runtime)
        .arg(package)
        .env("NPM_CONFIG_MIN_RELEASE_AGE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    command
}

fn adapter_runtime(root: &Path, harness: &dyn Harness) -> PathBuf {
    paths(root).runtime.join(format!("acp-{}", harness.id()))
}

/// Reads the version npm installed from the package manifest.
fn installed_version(runtime: &Path, package: &str) -> Result<String> {
    let path = runtime
        .join("node_modules")
        .join(package)
        .join("package.json");
    let contents = fs::read(&path).with_context(|| format!("could not read {}", path.display()))?;
    let manifest: Value = serde_json::from_slice(&contents)
        .with_context(|| format!("could not parse {}", path.display()))?;
    manifest["version"]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("{} has no version", path.display()))
}

/// Parses `major.minor.patch`. A prerelease sorts before its release.
fn parse_version(version: &str) -> Option<(u64, u64, u64, bool)> {
    let version = version.trim().trim_start_matches('v');
    let version = version.split_once('+').map_or(version, |(core, _)| core);
    let (core, release) = match version.split_once('-') {
        Some((core, _)) => (core, false),
        None => (version, true),
    };
    let mut parts = core.split('.').map(str::parse::<u64>);
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch)), None) => {
            Some((major, minor, patch, release))
        }
        _ => None,
    }
}

fn meets_floor(version: &str, floor: &str) -> bool {
    matches!(
        (parse_version(version), parse_version(floor)),
        (Some(version), Some(floor)) if version >= floor
    )
}

/// Returns the adapter package and its installed version, if the version can be read.
pub fn installed_adapter(
    root: &Path,
    harness: &dyn Harness,
) -> Option<(&'static str, Option<String>)> {
    let adapter = harness.adapter()?;
    let version = installed_version(&adapter_runtime(root, harness), adapter.package).ok();
    Some((adapter.package, version))
}

/// Rejects an installed adapter that is older than the known-good floor.
fn check_installed_floor(root: &Path, harness: &dyn Harness) -> Result<()> {
    let Some(adapter) = harness.adapter() else {
        return Ok(());
    };
    let Ok(version) = installed_version(&adapter_runtime(root, harness), adapter.package) else {
        bail!(
            "could not read the ACP adapter version; run `lazyagents repair` in {}",
            root.display()
        );
    };
    if !meets_floor(&version, adapter.min_version) {
        bail!(
            "the ACP adapter {} {version} is older than {}; run `lazyagents repair` in {}",
            adapter.package,
            adapter.min_version,
            root.display()
        );
    }
    Ok(())
}

fn adapter_path(root: &Path, harness: &dyn Harness) -> Option<PathBuf> {
    harness.adapter().map(|adapter| {
        adapter_runtime(root, harness)
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
    let message = "Install the missing ACP adapter now";
    let install = crate::prompts::run(message, || {
        inquire::Confirm::new(message)
            .with_default(true)
            .with_formatter(&|_| String::new())
            .prompt()
    });
    match install {
        Ok(true) => install_adapter(root, harness).await.map(|_| ()),
        Ok(false)
        | Err(
            inquire::InquireError::OperationCanceled | inquire::InquireError::OperationInterrupted,
        ) => bail!("chat needs the ACP adapter; run `lazyagents repair` when ready"),
        Err(error) => Err(error.into()),
    }
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
        check_installed_floor(root, harness)?;
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

    #[test]
    fn replacement_failure_restores_previous_adapter() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("acp-pi");
        fs::create_dir(&runtime).unwrap();
        fs::write(runtime.join("original"), "previous adapter").unwrap();
        let temporary = root.path().join("missing-install");

        let error = replace_adapter(&temporary, &runtime).unwrap_err();

        assert!(error.to_string().contains("could not replace"), "{error}");
        assert_eq!(
            fs::read_to_string(runtime.join("original")).unwrap(),
            "previous adapter"
        );
        assert!(!temporary.with_extension("previous").exists());
    }

    #[test]
    fn versions_compare_numerically_and_prereleases_sort_first() {
        assert!(meets_floor("0.76.0", "0.76.0"));
        assert!(meets_floor("0.100.0", "0.76.0"));
        assert!(meets_floor("2.0.0", "1.11.0"));
        assert!(meets_floor("v1.11.1+build.5", "1.11.0"));
        assert!(meets_floor("1.12.0-rc.1", "1.11.0"));
        assert!(!meets_floor("0.9.0", "0.76.0"));
        assert!(!meets_floor("1.11.0-beta.2", "1.11.0"));
        assert!(!meets_floor("1.11", "1.11.0"));
        assert!(!meets_floor("latest", "1.11.0"));
    }

    fn write_manifest(root: &Path, contents: &str) {
        let package = adapter_runtime(root, &claude::CLAUDE)
            .join("node_modules/@agentclientprotocol/claude-agent-acp");
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("package.json"), contents).unwrap();
    }

    #[test]
    fn installed_adapter_reads_the_package_manifest() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            installed_adapter(root.path(), &claude::CLAUDE),
            Some(("@agentclientprotocol/claude-agent-acp", None))
        );
        write_manifest(root.path(), r#"{"name":"x","version":"0.77.1"}"#);
        assert_eq!(
            installed_adapter(root.path(), &claude::CLAUDE),
            Some((
                "@agentclientprotocol/claude-agent-acp",
                Some("0.77.1".into())
            ))
        );
        assert_eq!(installed_adapter(root.path(), &opencode::OPENCODE), None);
    }

    #[test]
    fn installed_floor_rejects_old_or_unreadable_adapters() {
        let root = tempfile::tempdir().unwrap();
        let error = check_installed_floor(root.path(), &claude::CLAUDE).unwrap_err();
        assert!(error.to_string().contains("lazyagents repair"), "{error}");

        write_manifest(root.path(), r#"{"version":"0.75.9"}"#);
        let error = check_installed_floor(root.path(), &claude::CLAUDE).unwrap_err();
        assert!(
            error.to_string().contains("0.75.9 is older than 0.76.0"),
            "{error}"
        );
        assert!(error.to_string().contains("lazyagents repair"), "{error}");

        write_manifest(root.path(), r#"{"version":"0.77.1"}"#);
        check_installed_floor(root.path(), &claude::CLAUDE).unwrap();
        check_installed_floor(root.path(), &opencode::OPENCODE).unwrap();
    }
}
