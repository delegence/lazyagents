use crate::profile::validation::{Severity, ValidationIssue};
use crate::profile::{ArtifactStatus, McpSummary};

pub fn render_artifact_status(status: &ArtifactStatus) -> &'static str {
    match status {
        ArtifactStatus::Present => "present",
        ArtifactStatus::Missing => "missing",
        ArtifactStatus::NotFile => "not a file",
    }
}

pub fn render_string_list(values: &[String]) -> String {
    if values.is_empty() {
        "-".to_string()
    } else {
        values.join(", ")
    }
}

pub fn render_mcp_summary(summary: &McpSummary) -> String {
    match summary {
        McpSummary::Empty => "none".to_string(),
        McpSummary::Servers(names) => render_string_list(names),
        McpSummary::Invalid(error) => format!("invalid: {error}"),
    }
}

pub fn render_json_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

pub fn render_validation_issues(issues: &[ValidationIssue]) -> String {
    if issues.is_empty() {
        return "none".to_string();
    }

    let mut out = String::new();
    out.push_str(&format!("{} issues found:\n", issues.len()));

    for issue in issues {
        let sev = match issue.severity {
            Severity::Error => "ERROR",
            Severity::Warning => "WARN ",
        };
        let path = match &issue.path {
            Some(p) => format!(" [{p}]"),
            None => "".to_string(),
        };
        out.push_str(&format!(
            "  {sev} - {}{}: {}\n",
            issue.category, path, issue.message
        ));
    }
    out
}
