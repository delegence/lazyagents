use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use crate::config::paths;

use super::{
    link_authentication, AdapterSpec, Authentication, Harness, LaunchContext, LaunchOptions,
};

pub static CODEX: Codex = Codex;

pub struct Codex;

impl Harness for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn runtime_command(&self) -> &'static str {
        "codex"
    }

    fn adapter(&self) -> Option<AdapterSpec> {
        Some(AdapterSpec {
            package: "@agentclientprotocol/codex-acp",
            version: "1.8.0",
            binary: "codex-acp",
        })
    }

    fn authentication(&self) -> Option<Authentication> {
        Some(Authentication {
            status_args: &["login", "status"],
            login_args: &["login"],
            api_key_variables: &["CODEX_API_KEY", "OPENAI_API_KEY"],
        })
    }

    fn launch_options(&self, context: LaunchContext<'_>) -> Result<LaunchOptions> {
        let agent_paths = paths(context.root);
        let runtime = agent_paths.runtime.join(self.id());
        let private_home = runtime.join("home");
        fs::create_dir_all(&runtime)?;
        fs::create_dir_all(&private_home)?;
        let authentication = env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
            .map(|home| home.join("auth.json"));
        link_authentication(authentication, &runtime.join("auth.json"))?;

        let mut options = LaunchOptions::default();
        options
            .env
            .insert("CODEX_HOME".into(), runtime.display().to_string());
        options
            .env
            .insert("HOME".into(), private_home.display().to_string());
        options.env.insert(
            "CODEX_PATH".into(),
            context.runtime_path.display().to_string(),
        );
        options.env.insert(
            "CODEX_CONFIG".into(),
            serde_json::to_string(&json!({"developer_instructions": context.instruction}))?,
        );
        Ok(options)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn launch_uses_an_isolated_runtime_and_developer_instructions() {
        let root = tempdir().unwrap();
        let runtime = std::path::Path::new("/runtime/codex");
        let options = CODEX
            .launch_options(LaunchContext {
                root: root.path(),
                runtime_path: runtime,
                instruction: "You are a reviewer.",
            })
            .unwrap();

        assert_eq!(options.env["CODEX_PATH"], runtime.display().to_string());
        assert!(options.env["CODEX_HOME"].contains(".agents/runtime/codex"));
        assert!(options.env["HOME"].contains(".agents/runtime/codex/home"));
        let config: serde_json::Value = serde_json::from_str(&options.env["CODEX_CONFIG"]).unwrap();
        assert_eq!(config["developer_instructions"], "You are a reviewer.");
        assert!(options.session_meta.is_none());
    }
}
