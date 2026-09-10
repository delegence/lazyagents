use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::paths;

use super::{
    link_authentication, AdapterSpec, Authentication, Harness, LaunchContext, LaunchOptions,
};

pub static PI: Pi = Pi;

pub struct Pi;

impl Harness for Pi {
    fn id(&self) -> &'static str {
        "pi"
    }

    fn display_name(&self) -> &'static str {
        "Pi"
    }

    fn runtime_command(&self) -> &'static str {
        "pi"
    }

    fn adapter(&self) -> Option<AdapterSpec> {
        Some(AdapterSpec {
            package: "@automatalabs/pi-acp",
            version: "0.8.0",
            binary: "pi-acp",
        })
    }

    fn authentication(&self) -> Option<Authentication> {
        // Pi has provider-specific authentication and no general login command.
        None
    }

    fn launch_options(&self, context: LaunchContext<'_>) -> Result<LaunchOptions> {
        let agent_paths = paths(context.root);
        let runtime = agent_paths.runtime.join(self.id());
        let agent_dir = runtime.join("agent");
        let private_home = runtime.join("home");
        let session_dir = runtime.join("sessions");
        fs::create_dir_all(&agent_dir)?;
        fs::create_dir_all(&private_home)?;
        fs::create_dir_all(&session_dir)?;
        let source_agent_dir = env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".pi/agent")));
        let authentication = source_agent_dir
            .as_ref()
            .map(|directory| directory.join("auth.json"));
        link_authentication(authentication, &agent_dir.join("auth.json"))?;
        if let Some(source) = source_agent_dir {
            copy_model_catalog(
                &source.join("models-store.json"),
                &agent_dir.join("models-store.json"),
            )?;
        }
        fs::write(
            agent_dir.join("APPEND_SYSTEM.md"),
            format!("{}\n", context.instruction.trim_end()),
        )
        .context("could not prepare the Pi system prompt")?;

        let mut options = LaunchOptions::default();
        options
            .env
            .insert("HOME".into(), private_home.display().to_string());
        options.env.insert(
            "PI_CODING_AGENT_DIR".into(),
            agent_dir.display().to_string(),
        );
        options.env.insert(
            "PI_CODING_AGENT_SESSION_DIR".into(),
            session_dir.display().to_string(),
        );
        Ok(options)
    }
}

/// Refreshes the private catalogue without replacing it with a missing or partial source file.
fn copy_model_catalog(source: &Path, destination: &Path) -> Result<()> {
    let Ok(contents) = fs::read(source) else {
        return Ok(());
    };
    if serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&contents).is_err() {
        return Ok(());
    }
    let temporary = destination.with_extension(format!("{}.tmp", uuid::Uuid::now_v7()));
    let result = fs::write(&temporary, contents).and_then(|()| fs::rename(&temporary, destination));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.context("could not copy the Pi model catalogue")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn refreshes_model_catalog_and_keeps_the_last_valid_copy() {
        let root = tempdir().unwrap();
        let source = root.path().join("source.json");
        let destination = root.path().join("models-store.json");
        let original = br#"{"openai-codex":{"models":[]}}"#;
        let updated = br#"{"openai-codex":{"models":[{"id":"gpt-6-astra"}]}}"#;
        fs::write(&source, original).unwrap();
        copy_model_catalog(&source, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), original);
        fs::write(&source, updated).unwrap();
        copy_model_catalog(&source, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), updated);
        assert_eq!(fs::read(&source).unwrap(), updated);
        assert!(!fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink());
        for invalid in [b"{".as_slice(), b"null", b"[]"] {
            fs::write(&source, invalid).unwrap();
            copy_model_catalog(&source, &destination).unwrap();
            assert_eq!(fs::read(&destination).unwrap(), updated);
        }
        fs::remove_file(&source).unwrap();
        copy_model_catalog(&source, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), updated);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn launch_uses_an_isolated_runtime_and_appends_the_system_prompt() {
        let root = tempdir().unwrap();
        let options = PI
            .launch_options(LaunchContext {
                root: root.path(),
                runtime_path: std::path::Path::new("/runtime/pi"),
                instruction: "You are a researcher.\n",
            })
            .unwrap();
        let runtime = paths(root.path()).runtime.join("pi");

        assert_eq!(
            options.env["PI_CODING_AGENT_DIR"],
            runtime.join("agent").display().to_string()
        );
        assert_eq!(
            options.env["PI_CODING_AGENT_SESSION_DIR"],
            runtime.join("sessions").display().to_string()
        );
        assert_eq!(
            options.env["HOME"],
            runtime.join("home").display().to_string()
        );
        assert_eq!(
            fs::read_to_string(runtime.join("agent/APPEND_SYSTEM.md")).unwrap(),
            "You are a researcher.\n"
        );
        assert!(options.session_meta.is_none());
    }
}
