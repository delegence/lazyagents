use anyhow::Result;
use serde_json::json;

use crate::config::paths;

use super::{local_skills, AdapterSpec, Authentication, Harness, LaunchContext, LaunchOptions};

pub static CLAUDE: Claude = Claude;

pub struct Claude;

impl Harness for Claude {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn runtime_command(&self) -> &'static str {
        "claude"
    }

    fn adapter(&self) -> Option<AdapterSpec> {
        Some(AdapterSpec {
            package: "@agentclientprotocol/claude-agent-acp",
            version: "0.71.0",
            binary: "claude-agent-acp",
        })
    }

    fn authentication(&self) -> Option<Authentication> {
        Some(Authentication {
            status_args: &["auth", "status"],
            login_args: &["auth", "login"],
            api_key_variables: &["ANTHROPIC_API_KEY"],
        })
    }

    fn launch_options(&self, context: LaunchContext<'_>) -> Result<LaunchOptions> {
        let skills = local_skills(&paths(context.root).skills)?;
        Ok(LaunchOptions {
            session_meta: Some(json!({
                "systemPrompt": {"append": context.instruction},
                "claudeCode": {
                    "options": {
                        "settingSources": ["project"],
                        "skills": skills,
                        "env": {
                            "CLAUDE_CODE_DISABLE_AUTO_MEMORY": "1",
                            "CLAUDE_CODE_DISABLE_BUNDLED_SKILLS": "1"
                        }
                    }
                }
            })),
            ..LaunchOptions::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn lists_only_local_skill_directories() {
        let directory = tempdir().unwrap();
        fs::create_dir_all(directory.path().join("writer")).unwrap();
        fs::write(directory.path().join("writer/SKILL.md"), "# Writer").unwrap();
        fs::create_dir_all(directory.path().join("empty")).unwrap();
        assert_eq!(local_skills(directory.path()).unwrap(), vec!["writer"]);
    }

    #[test]
    fn launch_appends_instructions_and_selects_only_agent_skills() {
        let root = tempdir().unwrap();
        let skills = paths(root.path()).skills;
        fs::create_dir_all(skills.join("writer")).unwrap();
        fs::write(skills.join("writer/SKILL.md"), "# Writer").unwrap();

        let options = CLAUDE
            .launch_options(LaunchContext {
                root: root.path(),
                runtime_path: std::path::Path::new("/runtime/claude"),
                instruction: "You are a writer.",
            })
            .unwrap();
        let meta = options.session_meta.unwrap();

        assert_eq!(
            meta["systemPrompt"]["append"],
            serde_json::Value::String("You are a writer.".into())
        );
        assert_eq!(meta["claudeCode"]["options"]["skills"][0], "writer");
        assert!(options.env.is_empty());
    }
}
