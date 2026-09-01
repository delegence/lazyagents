use crate::harness::agents::{profile_agents, scan_agents};
use crate::profile::inspect::{scan_commands, scan_skills};
use crate::profile::mcp::collect_mcp_validation_errors;
use crate::profile::{read_profile_document, PROFILE_FILE_NAME};
use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationCategory {
    Config,
    Skills,
    Commands,
    Subagents,
    Mcp,
}

impl fmt::Display for ValidationCategory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Config => "Config",
            Self::Skills => "Skills",
            Self::Commands => "Commands",
            Self::Subagents => "Sub-agents",
            Self::Mcp => "MCP",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    pub severity: Severity,
    pub category: ValidationCategory,
    pub path: Option<String>,
    pub message: String,
}

impl ValidationIssue {
    pub fn error(category: ValidationCategory, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            category,
            path: None,
            message: message.into(),
        }
    }

    pub fn warning(category: ValidationCategory, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            category,
            path: None,
            message: message.into(),
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

pub fn validate_profile(path: &Path) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();

    // PROFILE.md
    let profile_path = path.join(PROFILE_FILE_NAME);
    if profile_path.exists() {
        if let Err(err) = read_profile_document(path) {
            issues.push(
                ValidationIssue::error(
                    ValidationCategory::Config,
                    format!("malformed PROFILE.md: {}", err),
                )
                .with_path(PROFILE_FILE_NAME),
            );
        }
    } else {
        issues.push(
            ValidationIssue::error(ValidationCategory::Config, "missing PROFILE.md")
                .with_path(PROFILE_FILE_NAME),
        );
    }

    // skills
    match scan_skills(&path.join("skills")) {
        Ok((_valid, ignored)) => {
            for ignored_skill in ignored {
                issues.push(
                    ValidationIssue::warning(
                        ValidationCategory::Skills,
                        "ignored skill directory or missing SKILL.md",
                    )
                    .with_path(format!("skills/{}", ignored_skill)),
                );
            }
        }
        Err(error) => issues.push(
            ValidationIssue::error(
                ValidationCategory::Skills,
                format!("failed to scan skills: {error}"),
            )
            .with_path("skills"),
        ),
    }

    // commands
    match scan_commands(&path.join("commands")) {
        Ok((commands, ignored)) => {
            for ignored_cmd in ignored {
                issues.push(
                    ValidationIssue::warning(
                        ValidationCategory::Commands,
                        "ignored non-markdown command file",
                    )
                    .with_path(format!("commands/{}", ignored_cmd)),
                );
            }
            for cmd in commands {
                if cmd.contains('/') {
                    issues.push(ValidationIssue::warning(ValidationCategory::Commands, "nested command may be incompatible with some target harnesses (e.g., Codex)").with_path(format!("commands/{}", cmd)));
                }
            }
        }
        Err(error) => issues.push(
            ValidationIssue::error(
                ValidationCategory::Commands,
                format!("failed to scan commands: {error}"),
            )
            .with_path("commands"),
        ),
    }

    // sub-agents
    match scan_agents(&path.join("agents")) {
        Ok((_agents, ignored)) => {
            for ignored_agent in ignored {
                issues.push(
                    ValidationIssue::warning(
                        ValidationCategory::Subagents,
                        "ignored non-markdown sub-agent file",
                    )
                    .with_path(format!("agents/{}", ignored_agent)),
                );
            }
        }
        Err(error) => issues.push(
            ValidationIssue::error(
                ValidationCategory::Subagents,
                format!("failed to scan sub-agents: {error}"),
            )
            .with_path("agents"),
        ),
    }
    if let Err(error) = profile_agents(path) {
        issues.push(
            ValidationIssue::error(
                ValidationCategory::Subagents,
                format!("malformed sub-agent definition: {error}"),
            )
            .with_path("agents"),
        );
    }

    // mcps.json
    let mcps_path = path.join("mcps.json");
    match std::fs::read_to_string(&mcps_path) {
        Ok(text) => {
            for error in collect_mcp_validation_errors(&text) {
                issues.push(
                    ValidationIssue::error(
                        ValidationCategory::Mcp,
                        format!("malformed mcps.json: {}", error.message),
                    )
                    .with_path(error.path),
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => issues.push(
            ValidationIssue::error(
                ValidationCategory::Mcp,
                format!("failed to read mcps.json: {error}"),
            )
            .with_path("mcps.json"),
        ),
    }

    issues
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_profile() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path();

        // create missing profile file
        std::fs::create_dir_all(path.join("skills")).unwrap();
        std::fs::create_dir_all(path.join("commands")).unwrap();

        let issues = validate_profile(path);
        assert!(
            issues
                .iter()
                .any(|i| i.category == ValidationCategory::Config
                    && i.message == "missing PROFILE.md")
        );

        // malformed profile frontmatter
        std::fs::write(path.join(PROFILE_FILE_NAME), "invalid").unwrap();
        let issues = validate_profile(path);
        assert!(issues
            .iter()
            .any(|i| i.category == ValidationCategory::Config
                && i.message.contains("malformed PROFILE.md")));

        // good profile file
        std::fs::write(
            path.join(PROFILE_FILE_NAME),
            "---\nname: test\nmodels: {}\npermissions: {}\n---\nInstructions\n",
        )
        .unwrap();

        // ignored skills
        std::fs::create_dir(path.join("skills").join("bad_skill")).unwrap();
        std::fs::write(path.join("skills").join("not_dir"), "content").unwrap();

        let issues = validate_profile(path);
        assert!(issues
            .iter()
            .any(|i| i.path.as_deref() == Some("skills/bad_skill")));
        assert!(issues
            .iter()
            .any(|i| i.path.as_deref() == Some("skills/not_dir")));

        // ignored commands and nested commands
        std::fs::write(path.join("commands").join("bad_cmd.txt"), "content").unwrap();
        std::fs::create_dir(path.join("commands").join("nested")).unwrap();
        std::fs::write(
            path.join("commands").join("nested").join("good_cmd.md"),
            "content",
        )
        .unwrap();

        let issues = validate_profile(path);
        assert!(issues
            .iter()
            .any(|i| i.path.as_deref() == Some("commands/bad_cmd.txt")));
        assert!(issues
            .iter()
            .any(|i| i.path.as_deref() == Some("commands/nested/good_cmd.md")
                && i.severity == Severity::Warning));

        // malformed mcps.json
        std::fs::write(path.join("mcps.json"), "invalid").unwrap();
        let issues = validate_profile(path);
        assert!(issues
            .iter()
            .any(|i| i.category == ValidationCategory::Mcp
                && i.message.contains("malformed mcps.json")));

        // invalid mcp url, empty mcp command, invalid transport, duplicate mcp, disabled duplicate mcp
        std::fs::write(
            path.join("mcps.json"),
            r#"[
            {"name": "m1", "transport": "stdio", "command": " "},
            {"name": "m2", "transport": "http", "url": "ftp://bad"},
            {"name": "m3", "transport": "fake"},
            {"name": "m4", "enabled": false, "transport": "fake"},
            {"name": "m1", "transport": "stdio", "command": "cmd"},
            {"name": "m4", "enabled": false, "transport": "fake2"},
            {"name": "m5", "enabled": false, "transport": "stdio", "command": ""}
        ]"#,
        )
        .unwrap();

        let issues = validate_profile(path);
        assert!(issues.iter().any(|i| i.category == ValidationCategory::Mcp
            && i.path.as_deref() == Some("mcps.json[0]")
            && i.message.contains("stdio MCP m1 requires command")));
        assert!(issues.iter().any(|i| i.category == ValidationCategory::Mcp
            && i.path.as_deref() == Some("mcps.json[1]")
            && i.message.contains("http MCP m2 requires url")));
        assert!(issues.iter().any(|i| i.category == ValidationCategory::Mcp
            && i.path.as_deref() == Some("mcps.json[2]")
            && i.message.contains("unsupported MCP transport: fake")));
    }

    #[test]
    fn scan_failures_are_reported() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(PROFILE_FILE_NAME),
            "---\nname: Test\n---\nInstructions\n",
        )
        .unwrap();
        for artifact in ["skills", "commands", "agents"] {
            std::fs::write(temp.path().join(artifact), "not a directory").unwrap();
        }

        let issues = validate_profile(temp.path());

        assert!(issues
            .iter()
            .any(|issue| issue.message.contains("failed to scan skills")));
        assert!(issues
            .iter()
            .any(|issue| issue.message.contains("failed to scan commands")));
        assert!(issues
            .iter()
            .any(|issue| issue.message.contains("failed to scan sub-agents")));
    }
}
