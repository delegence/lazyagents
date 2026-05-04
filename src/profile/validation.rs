use crate::harness::agents::{profile_agents, scan_agents};
use crate::profile::inspect::{scan_commands, scan_skills};
use crate::profile::mcp::collect_mcp_validation_errors;
use crate::profile::{read_profile_document, PROFILE_FILE_NAME};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    pub severity: Severity,
    pub category: String,
    pub path: Option<String>,
    pub message: String,
}

impl ValidationIssue {
    pub fn error(category: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            category: category.into(),
            path: None,
            message: message.into(),
        }
    }

    pub fn warning(category: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            category: category.into(),
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
                ValidationIssue::error("Config", format!("malformed PROFILE.md: {}", err))
                    .with_path(PROFILE_FILE_NAME),
            );
        }
    } else {
        issues.push(
            ValidationIssue::error("Config", "missing PROFILE.md").with_path(PROFILE_FILE_NAME),
        );
    }

    // skills
    if let Ok((_valid, ignored)) = scan_skills(&path.join("skills")) {
        for ignored_skill in ignored {
            issues.push(
                ValidationIssue::warning("Skills", "ignored skill directory or missing SKILL.md")
                    .with_path(format!("skills/{}", ignored_skill)),
            );
        }
    }

    // commands
    if let Ok((commands, ignored)) = scan_commands(&path.join("commands")) {
        for ignored_cmd in ignored {
            issues.push(
                ValidationIssue::warning("Commands", "ignored non-markdown command file")
                    .with_path(format!("commands/{}", ignored_cmd)),
            );
        }
        for cmd in commands {
            if cmd.contains('/') {
                issues.push(ValidationIssue::warning("Commands", "nested command may be incompatible with some target harnesses (e.g., Codex)").with_path(format!("commands/{}", cmd)));
            }
        }
    }

    // sub-agents
    if let Ok((_agents, ignored)) = scan_agents(&path.join("agents")) {
        for ignored_agent in ignored {
            issues.push(
                ValidationIssue::warning("Sub-agents", "ignored non-markdown sub-agent file")
                    .with_path(format!("agents/{}", ignored_agent)),
            );
        }
    }
    if let Err(error) = profile_agents(path) {
        issues.push(
            ValidationIssue::error(
                "Sub-agents",
                format!("malformed sub-agent definition: {error}"),
            )
            .with_path("agents"),
        );
    }

    // mcps.json
    let mcps_path = path.join("mcps.json");
    if let Ok(text) = std::fs::read_to_string(&mcps_path) {
        for error in collect_mcp_validation_errors(&text) {
            issues.push(
                ValidationIssue::error("MCP", format!("malformed mcps.json: {}", error.message))
                    .with_path(error.path),
            );
        }
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
        assert!(issues
            .iter()
            .any(|i| i.category == "Config" && i.message == "missing PROFILE.md"));

        // malformed profile frontmatter
        std::fs::write(path.join(PROFILE_FILE_NAME), "invalid").unwrap();
        let issues = validate_profile(path);
        assert!(issues
            .iter()
            .any(|i| i.category == "Config" && i.message.contains("malformed PROFILE.md")));

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
            .any(|i| i.category == "MCP" && i.message.contains("malformed mcps.json")));

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
        assert!(issues.iter().any(|i| i.category == "MCP"
            && i.path.as_deref() == Some("mcps.json[0]")
            && i.message.contains("stdio MCP m1 requires command")));
        assert!(issues.iter().any(|i| i.category == "MCP"
            && i.path.as_deref() == Some("mcps.json[1]")
            && i.message.contains("http MCP m2 requires url")));
        assert!(issues.iter().any(|i| i.category == "MCP"
            && i.path.as_deref() == Some("mcps.json[2]")
            && i.message.contains("unsupported MCP transport: fake")));
    }
}
