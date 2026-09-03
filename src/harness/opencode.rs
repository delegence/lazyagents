use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::{json, Map, Value};

use crate::config::paths;

use super::{
    link_authentication, local_skills, AdapterSpec, Authentication, Harness, LaunchContext,
    LaunchOptions,
};

pub static OPENCODE: OpenCode = OpenCode;

pub struct OpenCode;

impl Harness for OpenCode {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn display_name(&self) -> &'static str {
        "OpenCode"
    }

    fn runtime_command(&self) -> &'static str {
        "opencode"
    }

    fn adapter(&self) -> Option<AdapterSpec> {
        None
    }

    fn authentication(&self) -> Option<Authentication> {
        None
    }

    fn launch_options(&self, context: LaunchContext<'_>) -> Result<LaunchOptions> {
        let agent_paths = paths(context.root);
        let runtime = agent_paths.runtime.join(self.id());
        let config_home = runtime.join("config");
        let data_home = runtime.join("data");
        let state_home = runtime.join("state");
        fs::create_dir_all(&config_home)?;
        fs::create_dir_all(data_home.join("opencode"))?;
        fs::create_dir_all(&state_home)?;
        let authentication = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
            .map(|data| data.join("opencode/auth.json"));
        link_authentication(authentication, &data_home.join("opencode/auth.json"))?;

        let skills = local_skills(&agent_paths.skills)?;
        let config = isolated_config(context.instruction, &skills);
        let mut options = LaunchOptions {
            args: vec![
                OsString::from("acp"),
                OsString::from("--pure"),
                OsString::from("--cwd"),
                context.root.as_os_str().to_owned(),
            ],
            ..LaunchOptions::default()
        };
        options
            .env
            .insert("XDG_CONFIG_HOME".into(), config_home.display().to_string());
        options
            .env
            .insert("XDG_DATA_HOME".into(), data_home.display().to_string());
        options
            .env
            .insert("XDG_STATE_HOME".into(), state_home.display().to_string());
        options.env.insert(
            "OPENCODE_CONFIG_CONTENT".into(),
            serde_json::to_string(&config)?,
        );
        Ok(options)
    }
}

fn isolated_config(instruction: &str, local_skills: &[String]) -> Value {
    let mut skill_permissions = Map::new();
    skill_permissions.insert("*".into(), Value::String("deny".into()));
    for skill in local_skills {
        skill_permissions.insert(skill.clone(), Value::String("allow".into()));
    }
    json!({
        "autoupdate": false,
        "share": "disabled",
        "plugin": [],
        "default_agent": "lazyagents",
        "agent": {
            "lazyagents": {
                "description": "The local lazyagents agent",
                "mode": "primary",
                "prompt": instruction,
                "permission": {
                    "*": "ask",
                    "read": "allow",
                    "glob": "allow",
                    "grep": "allow",
                    "list": "allow",
                    "lsp": "allow",
                    "skill": skill_permissions
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn launch_uses_native_acp_and_an_isolated_runtime() {
        let root = tempdir().unwrap();
        let skills = paths(root.path()).skills;
        fs::create_dir_all(skills.join("writer")).unwrap();
        fs::write(
            skills.join("writer/SKILL.md"),
            "---\nname: writer\ndescription: Write\n---\n",
        )
        .unwrap();

        let options = OPENCODE
            .launch_options(LaunchContext {
                root: root.path(),
                runtime_path: std::path::Path::new("/runtime/opencode"),
                instruction: "You are a writer.",
            })
            .unwrap();

        assert_eq!(options.args[0], "acp");
        assert_eq!(options.args[1], "--pure");
        assert!(options.env["XDG_CONFIG_HOME"].contains(".agents/runtime/opencode/config"));
        let config: Value = serde_json::from_str(&options.env["OPENCODE_CONFIG_CONTENT"]).unwrap();
        assert_eq!(config["agent"]["lazyagents"]["prompt"], "You are a writer.");
        assert_eq!(
            config["agent"]["lazyagents"]["permission"]["skill"]["*"],
            "deny"
        );
        assert_eq!(
            config["agent"]["lazyagents"]["permission"]["skill"]["writer"],
            "allow"
        );
    }
}
