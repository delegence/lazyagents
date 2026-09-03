use std::env;
use std::fs;
use std::path::PathBuf;

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
            version: "0.6.2",
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
        let authentication = env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".pi/agent")))
            .map(|directory| directory.join("auth.json"));
        link_authentication(authentication, &agent_dir.join("auth.json"))?;
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

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

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
