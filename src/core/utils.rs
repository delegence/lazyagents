use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub fn expand_home(path: &str) -> Result<PathBuf> {
    if path == "~" {
        let home = dirs::home_dir().ok_or(Error::MissingHomeDir)?;
        return Ok(home);
    }

    if let Some(stripped) = path.strip_prefix("~/") {
        let home = dirs::home_dir().ok_or(Error::MissingHomeDir)?;
        return Ok(home.join(stripped));
    }

    Ok(Path::new(path).to_path_buf())
}

pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.exists() {
        fs::create_dir_all(parent).map_err(|err| Error::io(parent, err))?;
    }

    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "mews.tmp".to_string());
    let tmp_path = parent.join(format!("{}.tmp", file_name));

    {
        let mut file = fs::File::create(&tmp_path).map_err(|err| Error::io(&tmp_path, err))?;
        file.write_all(contents)
            .map_err(|err| Error::io(&tmp_path, err))?;
        file.sync_all().map_err(|err| Error::io(&tmp_path, err))?;
    }

    match fs::rename(&tmp_path, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            if err.kind() == std::io::ErrorKind::AlreadyExists {
                fs::remove_file(path).map_err(|err| Error::io(path, err))?;
                fs::rename(&tmp_path, path).map_err(|err| Error::io(path, err))
            } else {
                Err(Error::io(path, err))
            }
        }
    }
}

pub fn clear_dir_contents(path: &Path) -> Result<()> {
    if path.exists() {
        if !path.is_dir() {
            return Err(Error::InvalidInput(format!(
                "expected directory at {}",
                path.display()
            )));
        }
        for entry in fs::read_dir(path).map_err(|err| Error::io(path, err))? {
            let entry = entry.map_err(|err| Error::io(path, err))?;
            let entry_path = entry.path();
            if entry_path.is_dir() {
                fs::remove_dir_all(&entry_path).map_err(|err| Error::io(&entry_path, err))?;
            } else {
                fs::remove_file(&entry_path).map_err(|err| Error::io(&entry_path, err))?;
            }
        }
    } else {
        fs::create_dir_all(path).map_err(|err| Error::io(path, err))?;
    }

    Ok(())
}

pub fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|err| Error::io(path, err))
}

pub fn read_to_string(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|err| Error::io(path, err))
}

pub fn write_string(path: &Path, contents: &str) -> Result<()> {
    write_atomic(path, contents.as_bytes())
}

pub fn exists(path: &Path) -> bool {
    path.exists()
}

pub fn copy_file(src: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|err| Error::io(parent, err))?;
    }
    fs::copy(src, dest).map_err(|err| Error::io(dest, err))?;
    Ok(())
}

pub fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    if !src.exists() {
        return Err(Error::NotFound(format!(
            "missing directory {}",
            src.display()
        )));
    }
    fs::create_dir_all(dest).map_err(|err| Error::io(dest, err))?;
    for entry in fs::read_dir(src).map_err(|err| Error::io(src, err))? {
        let entry = entry.map_err(|err| Error::io(src, err))?;
        let path = entry.path();
        let target = dest.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            copy_file(&path, &target)?;
        }
    }
    Ok(())
}

pub fn remove_file_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).map_err(|err| Error::io(path, err))?;
    }
    Ok(())
}
