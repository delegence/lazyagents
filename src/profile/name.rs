use anyhow::Result;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProfileName(String);

impl ProfileName {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_profile_name(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProfileName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn validate_profile_name(value: &str) -> Result<()> {
    if value.is_empty() {
        anyhow::bail!("profile name cannot be empty");
    }

    if value.len() > 64 {
        anyhow::bail!("profile name cannot be longer than 64 characters");
    }

    if value.starts_with('-') {
        anyhow::bail!("profile name cannot start with dash");
    }

    if value.ends_with('-') {
        anyhow::bail!("profile name cannot end with dash");
    }

    if value.contains("--") {
        anyhow::bail!("profile name cannot contain consecutive dashes");
    }

    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        anyhow::bail!("profile name may contain only ASCII letters, numbers, and dash");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_name_accepts_valid_names() {
        for name in ["work", "work-1", "a", "A1-b2"] {
            assert!(ProfileName::parse(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn profile_name_rejects_spec_invalid_names() {
        for name in [
            "", "work 1", "work.1", "work_1", "-work", "work-", "work--1", "é",
        ] {
            assert!(ProfileName::parse(name).is_err(), "{name}");
        }
    }

    #[test]
    fn profile_name_rejects_names_longer_than_64_characters() {
        assert!(ProfileName::parse("a".repeat(64)).is_ok());
        assert!(ProfileName::parse("a".repeat(65)).is_err());
    }
}
