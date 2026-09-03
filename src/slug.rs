use anyhow::{bail, Result};

pub fn make(value: &str) -> Result<String> {
    let mut slug = String::new();
    let mut separator = false;

    for character in value.trim().chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }

    if slug.is_empty() {
        bail!("the agent name must contain at least one letter or number");
    }
    Ok(slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_a_safe_launcher_name() {
        assert_eq!(make(" Research  Agent! ").unwrap(), "research-agent");
        assert_eq!(make("Agent 42").unwrap(), "agent-42");
    }

    #[test]
    fn rejects_an_empty_name() {
        assert!(make(" -- ").is_err());
    }
}
