use crate::profile::inspect::{scan_commands, scan_skills};
use serde_json::Value;
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

    // config.json
    let config_path = path.join("config.json");
    if let Ok(text) = std::fs::read_to_string(&config_path) {
        if let Err(err) = serde_json::from_str::<crate::profile::ProfileConfig>(&text) {
            issues.push(
                ValidationIssue::error("Config", format!("malformed config.json: {}", err))
                    .with_path("config.json"),
            );
        }
    } else {
        issues
            .push(ValidationIssue::error("Config", "missing config.json").with_path("config.json"));
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

    // mcps.json
    let mcps_path = path.join("mcps.json");
    if let Ok(text) = std::fs::read_to_string(&mcps_path) {
        if text.trim().is_empty() {
            // Empty is valid
        } else {
            match serde_json::from_str::<Vec<Value>>(&text) {
                Ok(mcps) => {
                    let mut seen_names = std::collections::HashSet::new();
                    for (i, mcp) in mcps.iter().enumerate() {
                        let name = mcp
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("<unnamed>");
                        let path_str = format!("mcps.json[{}]", i);

                        if !seen_names.insert(name.to_string()) {
                            issues.push(
                                ValidationIssue::error(
                                    "MCP",
                                    format!("duplicate MCP name '{}'", name),
                                )
                                .with_path(path_str.clone()),
                            );
                        }

                        let transport = mcp.get("transport").and_then(|v| v.as_str());
                        match transport {
                            Some("stdio") => {
                                if mcp
                                    .get("command")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .trim()
                                    .is_empty()
                                {
                                    issues.push(
                                        ValidationIssue::error(
                                            "MCP",
                                            format!("MCP '{}' has empty command", name),
                                        )
                                        .with_path(path_str),
                                    );
                                }
                            }
                            Some("http") => {
                                if let Some(url) = mcp.get("url").and_then(|v| v.as_str()) {
                                    if !url.starts_with("http://") && !url.starts_with("https://") {
                                        issues.push(
                                            ValidationIssue::error(
                                                "MCP",
                                                format!("MCP '{}' has invalid URL", name),
                                            )
                                            .with_path(path_str),
                                        );
                                    }
                                } else {
                                    issues.push(
                                        ValidationIssue::error(
                                            "MCP",
                                            format!("MCP '{}' is missing url", name),
                                        )
                                        .with_path(path_str),
                                    );
                                }
                            }
                            Some(other) => {
                                issues.push(
                                    ValidationIssue::error(
                                        "MCP",
                                        format!(
                                            "MCP '{}' uses unsupported transport '{}'",
                                            name, other
                                        ),
                                    )
                                    .with_path(path_str),
                                );
                            }
                            None => {
                                issues.push(
                                    ValidationIssue::error(
                                        "MCP",
                                        format!("MCP '{}' is missing transport", name),
                                    )
                                    .with_path(path_str),
                                );
                            }
                        }
                    }
                }
                Err(err) => {
                    issues.push(
                        ValidationIssue::error("MCP", format!("malformed mcps.json: {}", err))
                            .with_path("mcps.json"),
                    );
                }
            }
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

        // create missing config profile
        std::fs::create_dir_all(path.join("skills")).unwrap();
        std::fs::create_dir_all(path.join("commands")).unwrap();

        let issues = validate_profile(path);
        assert!(issues
            .iter()
            .any(|i| i.category == "Config" && i.message == "missing config.json"));

        // malformed config
        std::fs::write(path.join("config.json"), "invalid").unwrap();
        let issues = validate_profile(path);
        assert!(issues
            .iter()
            .any(|i| i.category == "Config" && i.message.contains("malformed config.json")));

        // good config
        std::fs::write(
            path.join("config.json"),
            r#"{"name":"test","models":{},"permissions":{}}"#,
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
        assert!(issues
            .iter()
            .any(|i| i.message.contains("empty command")
                && i.path.as_deref() == Some("mcps.json[0]")));
        assert!(issues.iter().any(
            |i| i.message.contains("invalid URL") && i.path.as_deref() == Some("mcps.json[1]")
        ));
        assert!(issues
            .iter()
            .any(|i| i.message.contains("unsupported transport")
                && i.path.as_deref() == Some("mcps.json[2]")));
        assert!(issues
            .iter()
            .any(|i| i.message.contains("unsupported transport")
                && i.path.as_deref() == Some("mcps.json[3]")));
        assert!(issues
            .iter()
            .any(|i| i.message.contains("duplicate MCP name 'm1'")
                && i.path.as_deref() == Some("mcps.json[4]")));
        assert!(issues
            .iter()
            .any(|i| i.message.contains("duplicate MCP name 'm4'")
                && i.path.as_deref() == Some("mcps.json[5]")));
        assert!(issues
            .iter()
            .any(|i| i.message.contains("empty command")
                && i.path.as_deref() == Some("mcps.json[6]")));
    }
}
