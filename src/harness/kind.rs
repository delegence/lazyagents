use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HarnessKind {
    Codex,
    Claude,
    Gemini,
    OpenCode,
    Pi,
}

impl HarnessKind {
    pub fn parse(id: &str) -> anyhow::Result<Self> {
        match id {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            "gemini" => Ok(Self::Gemini),
            "opencode" => Ok(Self::OpenCode),
            "pi" => Ok(Self::Pi),
            other => anyhow::bail!("unsupported harness type {other}"),
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::OpenCode => "opencode",
            Self::Pi => "pi",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
            Self::Gemini => "Gemini",
            Self::OpenCode => "OpenCode",
            Self::Pi => "Pi",
        }
    }

    pub fn binary_name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::OpenCode => "opencode",
            Self::Pi => "pi",
        }
    }
}

impl fmt::Display for HarnessKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HarnessInstanceId(String);

impl HarnessInstanceId {
    pub fn parse(value: impl Into<String>) -> anyhow::Result<Self> {
        let value = value.into();
        if value.is_empty() {
            anyhow::bail!("harness instance id cannot be empty");
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            anyhow::bail!(
                "harness instance id {value} may contain only lowercase ASCII letters, numbers, and dash"
            );
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HarnessInstanceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessInstance {
    pub id: HarnessInstanceId,
    pub kind: HarnessKind,
    pub display_name: String,
    pub binary: String,
    pub config_dir: PathBuf,
}

impl HarnessInstance {
    pub fn alias_key(&self) -> (HarnessKind, PathBuf) {
        (self.kind, normalize_path_lexically(&self.config_dir))
    }
}

pub fn normalize_path_lexically(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}
