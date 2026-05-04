use crate::harness::drift::DriftItem;
use crate::harness::integration::{AppEnvironment, HarnessDetection, ImportedFile};
use crate::harness::managed::write_text_atomic;
use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub fn detect_binary(env: &AppEnvironment, binary_name: &str) -> HarnessDetection {
    for path in &env.path_entries {
        let binary_path = path.join(binary_name);
        if is_executable_file(&binary_path) {
            return HarnessDetection::Detected { binary_path };
        }
    }
    HarnessDetection::NotDetected
}

#[cfg(unix)]
pub(crate) fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub(crate) fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

pub fn symlink_points_to(link: &Path, source: &Path) -> bool {
    fs::read_link(link)
        .map(|target| target == source)
        .unwrap_or(false)
}

pub fn collect_directory_link_drift(
    surface: &str,
    expected_sources: Vec<PathBuf>,
    target_dir: &Path,
    items: &mut Vec<DriftItem>,
) -> Result<()> {
    let mut expected_names = BTreeSet::new();
    for source in expected_sources {
        let name = source
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("invalid source path {}", source.display()))?
            .to_string_lossy()
            .into_owned();
        expected_names.insert(name.clone());
        if !symlink_points_to(&target_dir.join(&name), &source) {
            items.push(DriftItem {
                surface: surface.to_string(),
                detail: format!(
                    "{} is not linked to active profile",
                    target_dir.join(&name).display()
                ),
            });
        }
    }
    if target_dir.exists() {
        for entry in fs::read_dir(target_dir)
            .with_context(|| format!("failed to read {}", target_dir.display()))?
        {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if !expected_names.contains(&name) {
                items.push(DriftItem {
                    surface: surface.to_string(),
                    detail: format!("unexpected harness entry {}", entry.path().display()),
                });
            }
        }
    }
    Ok(())
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

#[cfg(test)]
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

pub fn write_profile_instructions(profile_path: &Path, target: &Path) -> Result<()> {
    let instructions = crate::profile::read_profile_instructions(profile_path)?;
    write_text_atomic(target, &instructions)
        .with_context(|| format!("failed to write {}", target.display()))
}

pub fn verify_profile_instructions(
    harness_name: &str,
    profile_path: &Path,
    target: &Path,
) -> Result<()> {
    let expected = crate::profile::read_profile_instructions(profile_path)?;
    let actual = fs::read_to_string(target)
        .with_context(|| format!("failed to read instruction target {}", target.display()))?;
    if actual != expected {
        anyhow::bail!(
            "{harness_name} instruction target {} does not match profile instructions",
            target.display()
        );
    }
    Ok(())
}

pub fn collect_instruction_content_drift(
    profile_path: &Path,
    target: &Path,
    items: &mut Vec<DriftItem>,
) -> Result<()> {
    let expected = crate::profile::read_profile_instructions(profile_path)?;
    match fs::read_to_string(target) {
        Ok(actual) if actual == expected => {}
        Ok(_) => items.push(DriftItem {
            surface: "instructions".to_string(),
            detail: format!("{} differs from active profile", target.display()),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => items.push(DriftItem {
            surface: "instructions".to_string(),
            detail: format!("{} is missing", target.display()),
        }),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", target.display()))
        }
    }
    Ok(())
}

pub fn import_files_recursive(root: &Path, path: &Path) -> Result<Vec<ImportedFile>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.metadata()?.is_dir() {
            files.extend(import_files_recursive(root, &path)?);
        } else if path.metadata()?.is_file() {
            files.push(ImportedFile {
                relative_path: path
                    .strip_prefix(root)
                    .with_context(|| format!("{} is not under {}", path.display(), root.display()))?
                    .to_path_buf(),
                contents: fs::read(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?,
            });
        }
    }
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with_path(path: std::path::PathBuf) -> AppEnvironment {
        AppEnvironment {
            lazyagents_home: path.join("lazyagents"),
            user_home: path.join("user"),
            path_entries: vec![path],
        }
    }

    #[cfg(unix)]
    #[test]
    fn detect_binary_requires_executable_file_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("tool");
        fs::write(&bin, "").unwrap();

        let env = env_with_path(temp.path().to_path_buf());
        assert_eq!(detect_binary(&env, "tool"), HarnessDetection::NotDetected);

        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            detect_binary(&env, "tool"),
            HarnessDetection::Detected { binary_path: bin }
        );
    }
}
