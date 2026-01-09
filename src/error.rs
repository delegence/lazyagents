use std::fmt;
use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum Error {
    MissingHomeDir,
    Io {
        path: PathBuf,
        source: io::Error,
    },
    SerdeJson {
        path: PathBuf,
        source: serde_json::Error,
    },
    InvalidInput(String),
    NotFound(String),
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub fn serde_json(path: impl Into<PathBuf>, source: serde_json::Error) -> Self {
        Self::SerdeJson {
            path: path.into(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::MissingHomeDir => {
                write!(f, "could not determine a home directory for mews")
            }
            Error::Io { path, source } => {
                write!(f, "could not access {}: {}", path.display(), source)
            }
            Error::SerdeJson { path, source } => {
                write!(
                    f,
                    "could not parse JSON from {}: {}",
                    path.display(),
                    source
                )
            }
            Error::InvalidInput(message) => write!(f, "{message}"),
            Error::NotFound(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::MissingHomeDir => None,
            Error::Io { source, .. } => Some(source),
            Error::SerdeJson { source, .. } => Some(source),
            Error::InvalidInput(_) => None,
            Error::NotFound(_) => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
