use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

pub fn is_hidden_name(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| name.starts_with('.'))
}

pub fn write_text_atomic(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let existing_permissions = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Some(metadata.permissions()),
        Ok(_) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()))
        }
    };

    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    let mut temp = tempfile::Builder::new()
        .prefix(".lazyagents-write-")
        .tempfile_in(parent)
        .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;
    if let Some(permissions) = existing_permissions {
        temp.as_file()
            .set_permissions(permissions)
            .with_context(|| format!("failed to preserve permissions for {}", path.display()))?;
    }
    temp.write_all(contents.as_bytes())
        .with_context(|| format!("failed to write temporary file for {}", path.display()))?;
    temp.as_file()
        .sync_all()
        .with_context(|| format!("failed to flush temporary file for {}", path.display()))?;
    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    sync_directory(parent)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("failed to sync directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

pub fn resolve_path_identity(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory")?
            .join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Prefix(_) | Component::RootDir => {
                resolved.push(component.as_os_str());
            }
            Component::Normal(name) => {
                let candidate = resolved.join(name);
                match fs::symlink_metadata(&candidate) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        resolved = fs::canonicalize(&candidate).with_context(|| {
                            format!("failed to resolve path {}", candidate.display())
                        })?;
                    }
                    Ok(_) => resolved = candidate,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        resolved = candidate;
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("failed to inspect path {}", candidate.display())
                        });
                    }
                }
            }
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_existing_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("config.json");
        fs::write(&target, "old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();

        write_text_atomic(&target, "new").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn atomic_write_replaces_content_without_leaving_temp_file() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("config.json");

        write_text_atomic(&target, "{\"old\":true}").unwrap();
        write_text_atomic(&target, "{\"new\":true}").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "{\"new\":true}");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_does_not_follow_a_prepared_pid_temp_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("config.json");
        let victim = temp.path().join("victim");
        fs::write(&victim, "safe").unwrap();
        let predictable = temp
            .path()
            .join(format!(".config.json.{}.tmp", std::process::id()));
        symlink(&victim, predictable).unwrap();

        write_text_atomic(&target, "new").unwrap();

        assert_eq!(fs::read_to_string(target).unwrap(), "new");
        assert_eq!(fs::read_to_string(victim).unwrap(), "safe");
    }

    #[cfg(unix)]
    #[test]
    fn path_identity_resolves_missing_leaves_through_symlinked_ancestors() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        symlink(&real, temp.path().join("link")).unwrap();

        assert_eq!(
            resolve_path_identity(&temp.path().join("link/a/../b")).unwrap(),
            fs::canonicalize(real).unwrap().join("b")
        );
    }

    #[cfg(unix)]
    #[test]
    fn path_identity_keeps_symlink_then_parent_semantics() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        let config = temp.path().join("config");
        fs::create_dir_all(profile.join("sub")).unwrap();
        fs::create_dir(&config).unwrap();
        symlink(profile.join("sub"), config.join("link")).unwrap();

        assert_eq!(
            resolve_path_identity(&config.join("link/../skills")).unwrap(),
            fs::canonicalize(profile).unwrap().join("skills")
        );
    }
}
