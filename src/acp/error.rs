use std::fmt;

use super::Client;

#[derive(Debug)]
pub struct PromptTimeout {
    pub(super) connection_ready: bool,
}

impl PromptTimeout {
    pub fn connection_ready(&self) -> bool {
        self.connection_ready
    }
}

impl fmt::Display for PromptTimeout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.connection_ready {
            formatter.write_str("agent exceeded the configured inactivity limit")
        } else {
            formatter.write_str("agent did not stop after cancellation; restart chat")
        }
    }
}

impl std::error::Error for PromptTimeout {}

#[derive(Debug)]
pub struct ProtocolError {
    pub(super) code: i64,
    pub(super) message: String,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ACP error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for ProtocolError {}

impl Client {
    pub fn friendly_error(&self, error: &anyhow::Error, system_error: &str) -> String {
        friendly_error(self.runtime_name, error, system_error, &self.diagnostics())
    }
}

pub fn friendly_error(
    runtime_name: &str,
    error: &anyhow::Error,
    system_error: &str,
    diagnostics: &[String],
) -> String {
    let details = format!("{system_error}\n{}\n{error:#}", diagnostics.join("\n"));
    let lowercase = details.to_ascii_lowercase();
    if lowercase.contains("usage limit") {
        if let Some(start) = lowercase.find("try again at") {
            let retry = details[start..]
                .split(['.', '\n'])
                .next()
                .unwrap_or("try again later")
                .trim();
            return format!("Usage limit reached. {}.", capitalize(retry));
        }
        return "Usage limit reached. Try again later.".to_owned();
    }
    if lowercase.contains("authentication")
        || lowercase.contains("not logged in")
        || lowercase.contains("no api key")
        || lowercase.contains("missing credential")
    {
        return format!("{runtime_name} authentication failed. Sign in and try again.");
    }
    format!("{runtime_name} could not complete the request. Try again.")
}

fn capitalize(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_provider_api_key_errors_as_authentication_errors() {
        let error = anyhow::anyhow!("No API key for openrouter");
        assert_eq!(
            friendly_error("Pi", &error, "", &[]),
            "Pi authentication failed. Sign in and try again."
        );
    }
}
