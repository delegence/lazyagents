use crate::harness::integration::{AppEnvironment, HarnessDetection};
use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::fs;
use std::path::Path;

pub fn detect_binary(env: &AppEnvironment, binary_name: &str) -> HarnessDetection {
    for path in &env.path_entries {
        let binary_path = path.join(binary_name);
        if binary_path.is_file() {
            return HarnessDetection::Detected { binary_path };
        }
    }
    HarnessDetection::NotDetected
}

pub fn symlink_points_to(link: &Path, source: &Path) -> bool {
    fs::read_link(link)
        .map(|target| target == source)
        .unwrap_or(false)
}

#[cfg(unix)]
pub fn symlink_file(source: impl AsRef<Path>, target: impl AsRef<Path>) -> Result<()> {
    std::os::unix::fs::symlink(source.as_ref(), target.as_ref())
        .with_context(|| format!("failed to link {}", target.as_ref().display()))
}

#[cfg(unix)]
pub fn symlink_dir(source: impl AsRef<Path>, target: impl AsRef<Path>) -> Result<()> {
    std::os::unix::fs::symlink(source.as_ref(), target.as_ref())
        .with_context(|| format!("failed to link {}", target.as_ref().display()))
}

#[cfg(windows)]
pub fn symlink_file(source: impl AsRef<Path>, target: impl AsRef<Path>) -> Result<()> {
    std::os::windows::fs::symlink_file(source.as_ref(), target.as_ref())
        .with_context(|| format!("failed to link {}", target.as_ref().display()))
}

#[cfg(windows)]
pub fn symlink_dir(source: impl AsRef<Path>, target: impl AsRef<Path>) -> Result<()> {
    std::os::windows::fs::symlink_dir(source.as_ref(), target.as_ref())
        .with_context(|| format!("failed to link {}", target.as_ref().display()))
}

pub fn read_json(path: &Path) -> Result<Map<String, Value>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()))
        }
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("invalid JSON at {}", path.display()))?;
    if let Value::Object(map) = value {
        Ok(map)
    } else {
        anyhow::bail!("JSON at {} is not an object", path.display())
    }
}

pub fn normalize_json_text(text: &str) -> Value {
    if text.trim().is_empty() {
        serde_json::json!([])
    } else {
        serde_json::from_str(text).unwrap_or_else(|_| serde_json::json!(text.trim()))
    }
}

pub fn read_optional_string(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}
