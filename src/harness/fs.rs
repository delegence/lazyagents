use crate::file_system::write_text_atomic;
use crate::harness::drift::DriftItem;
use crate::harness::integration::{
    AppEnvironment, HarnessDetection, ImportedDirectoryEntry, ImportedFile,
};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

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

pub fn copy_file(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(source, target).with_context(|| {
        format!(
            "failed to copy {} to {}",
            source.display(),
            target.display()
        )
    })?;
    Ok(())
}

pub fn copy_directory(source: &Path, target: &Path) -> Result<()> {
    let root = fs::canonicalize(source)
        .with_context(|| format!("failed to resolve {}", source.display()))?;
    copy_directory_at(source, target, &root, &mut BTreeSet::new())
}

fn copy_directory_at(
    source: &Path,
    target: &Path,
    root: &Path,
    active: &mut BTreeSet<std::path::PathBuf>,
) -> Result<()> {
    let identity = fs::canonicalize(source)
        .with_context(|| format!("failed to resolve {}", source.display()))?;
    if !identity.starts_with(root) {
        anyhow::bail!(
            "directory {} resolves outside {}",
            source.display(),
            root.display()
        );
    }
    if !active.insert(identity.clone()) {
        anyhow::bail!("directory cycle detected at {}", source.display());
    }
    fs::create_dir_all(target).with_context(|| format!("failed to create {}", target.display()))?;
    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry?;
        if is_hidden_name(&entry.file_name()) {
            continue;
        }
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let link_metadata = fs::symlink_metadata(&source_path)
            .with_context(|| format!("failed to inspect {}", source_path.display()))?;
        let metadata = fs::metadata(&source_path)
            .with_context(|| format!("failed to inspect {}", source_path.display()))?;
        if link_metadata.file_type().is_symlink() {
            let resolved = fs::canonicalize(&source_path)?;
            if !resolved.starts_with(root) {
                anyhow::bail!(
                    "symlink {} resolves outside {}",
                    source_path.display(),
                    root.display()
                );
            }
        }
        if metadata.is_dir() {
            copy_directory_at(&source_path, &target_path, root, active)?;
        } else if metadata.is_file() {
            copy_file(&source_path, &target_path)?;
        } else {
            anyhow::bail!("unsupported filesystem entry {}", source_path.display());
        }
    }
    active.remove(&identity);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(source)?.permissions().mode() & 0o777;
        fs::set_permissions(target, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

pub fn collect_directory_content_drift(
    surface: &str,
    source: &Path,
    target: &Path,
    items: &mut Vec<DriftItem>,
) -> Result<()> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            items.push(DriftItem {
                surface: surface.to_string(),
                detail: format!("{} differs from active profile", target.display()),
            });
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            items.push(DriftItem {
                surface: surface.to_string(),
                detail: format!("{} is missing", target.display()),
            });
            return Ok(());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", target.display()))
        }
    }

    let mut expected_names = BTreeSet::new();
    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry?;
        if is_hidden_name(&entry.file_name()) {
            continue;
        }
        expected_names.insert(entry.file_name());
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let source_metadata = fs::metadata(&source_path)
            .with_context(|| format!("failed to inspect {}", source_path.display()))?;
        if source_metadata.is_dir() {
            collect_directory_content_drift(surface, &source_path, &target_path, items)?;
        } else if source_metadata.is_file() {
            collect_file_content_drift(surface, &source_path, &target_path, items)?;
        }
    }

    for entry in
        fs::read_dir(target).with_context(|| format!("failed to read {}", target.display()))?
    {
        let entry = entry?;
        if is_hidden_name(&entry.file_name()) || expected_names.contains(&entry.file_name()) {
            continue;
        }
        if entry.file_type()?.is_dir() && !has_visible_entries(&entry.path())? {
            continue;
        }
        items.push(DriftItem {
            surface: surface.to_string(),
            detail: format!("unexpected harness entry {}", entry.path().display()),
        });
    }
    Ok(())
}

pub fn collect_file_content_drift(
    surface: &str,
    source: &Path,
    target: &Path,
    items: &mut Vec<DriftItem>,
) -> Result<()> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            let expected =
                fs::read(source).with_context(|| format!("failed to read {}", source.display()))?;
            let actual =
                fs::read(target).with_context(|| format!("failed to read {}", target.display()))?;
            if actual != expected {
                items.push(DriftItem {
                    surface: surface.to_string(),
                    detail: format!("{} differs from active profile", target.display()),
                });
            }
        }
        Ok(_) => items.push(DriftItem {
            surface: surface.to_string(),
            detail: format!("{} differs from active profile", target.display()),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => items.push(DriftItem {
            surface: surface.to_string(),
            detail: format!("{} is missing", target.display()),
        }),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", target.display()))
        }
    }
    Ok(())
}

pub use crate::file_system::is_hidden_name;

pub fn has_visible_entries(path: &Path) -> Result<bool> {
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        if is_hidden_name(&entry.file_name()) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            if has_visible_entries(&entry.path())? {
                return Ok(true);
            }
        } else {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(all(test, unix))]
pub fn symlink_file(source: impl AsRef<Path>, target: impl AsRef<Path>) -> Result<()> {
    std::os::unix::fs::symlink(source.as_ref(), target.as_ref())
        .with_context(|| format!("failed to link {}", target.as_ref().display()))
}

#[cfg(all(test, unix))]
pub fn symlink_dir(source: impl AsRef<Path>, target: impl AsRef<Path>) -> Result<()> {
    std::os::unix::fs::symlink(source.as_ref(), target.as_ref())
        .with_context(|| format!("failed to link {}", target.as_ref().display()))
}

#[cfg(all(test, windows))]
pub fn symlink_file(source: impl AsRef<Path>, target: impl AsRef<Path>) -> Result<()> {
    std::os::windows::fs::symlink_file(source.as_ref(), target.as_ref())
        .with_context(|| format!("failed to link {}", target.as_ref().display()))
}

#[cfg(all(test, windows))]
pub fn symlink_dir(source: impl AsRef<Path>, target: impl AsRef<Path>) -> Result<()> {
    std::os::windows::fs::symlink_dir(source.as_ref(), target.as_ref())
        .with_context(|| format!("failed to link {}", target.as_ref().display()))
}

#[cfg(test)]
pub fn normalize_json_text(text: &str) -> serde_json::Value {
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
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            items.push(DriftItem {
                surface: "instructions".to_string(),
                detail: format!("{} has the wrong type", target.display()),
            });
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            items.push(DriftItem {
                surface: "instructions".to_string(),
                detail: format!("{} is missing", target.display()),
            });
            return Ok(());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", target.display()))
        }
    }
    match fs::read_to_string(target) {
        Ok(actual) if actual == expected => {}
        Ok(_) => items.push(DriftItem {
            surface: "instructions".to_string(),
            detail: format!("{} differs from active profile", target.display()),
        }),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", target.display()))
        }
    }
    Ok(())
}

pub fn import_files_recursive(root: &Path, path: &Path) -> Result<Vec<ImportedFile>> {
    import_files_recursive_filtered(root, path, &|_| true)
}

pub fn import_files_recursive_filtered(
    root: &Path,
    path: &Path,
    include: &dyn Fn(&Path) -> bool,
) -> Result<Vec<ImportedFile>> {
    let canonical_root =
        fs::canonicalize(root).with_context(|| format!("failed to resolve {}", root.display()))?;
    let mut files = Vec::new();
    import_files_at(
        root,
        path,
        &canonical_root,
        &mut BTreeSet::new(),
        include,
        &mut files,
    )?;
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

fn import_files_at(
    root: &Path,
    path: &Path,
    canonical_root: &Path,
    active: &mut BTreeSet<std::path::PathBuf>,
    include: &dyn Fn(&Path) -> bool,
    files: &mut Vec<ImportedFile>,
) -> Result<()> {
    let identity =
        fs::canonicalize(path).with_context(|| format!("failed to resolve {}", path.display()))?;
    if !identity.starts_with(canonical_root) {
        anyhow::bail!(
            "directory {} resolves outside {}",
            path.display(),
            root.display()
        );
    }
    if !active.insert(identity.clone()) {
        anyhow::bail!("directory cycle detected at {}", path.display());
    }
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let link_metadata = fs::symlink_metadata(&path)?;
        let metadata = fs::metadata(&path)?;
        if link_metadata.file_type().is_symlink() {
            let resolved = fs::canonicalize(&path)?;
            if !resolved.starts_with(canonical_root) {
                anyhow::bail!(
                    "symlink {} resolves outside {}",
                    path.display(),
                    root.display()
                );
            }
        }
        if metadata.is_dir() {
            import_files_at(root, &path, canonical_root, active, include, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .with_context(|| format!("{} is not under {}", path.display(), root.display()))?;
            if !include(relative) {
                continue;
            }
            files.push(ImportedFile {
                relative_path: relative.to_path_buf(),
                contents: fs::read(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?,
                unix_mode: unix_mode(&path)?,
            });
        } else {
            anyhow::bail!("unsupported filesystem entry {}", path.display());
        }
    }
    active.remove(&identity);
    Ok(())
}

pub struct ImportedTree {
    pub files: Vec<ImportedFile>,
    pub directories: Vec<ImportedDirectoryEntry>,
    pub root_unix_mode: Option<u32>,
}

pub fn import_tree_lossless(root: &Path) -> Result<ImportedTree> {
    let canonical_root =
        fs::canonicalize(root).with_context(|| format!("failed to resolve {}", root.display()))?;
    let mut tree = ImportedTree {
        files: Vec::new(),
        directories: Vec::new(),
        root_unix_mode: unix_mode(root)?,
    };
    import_tree_lossless_at(root, root, &canonical_root, &mut tree)?;
    tree.files
        .sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    tree.directories
        .sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(tree)
}

fn import_tree_lossless_at(
    root: &Path,
    path: &Path,
    canonical_root: &Path,
    tree: &mut ImportedTree,
) -> Result<()> {
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path)
            .with_context(|| format!("failed to inspect {}", entry_path.display()))?;
        let relative = entry_path
            .strip_prefix(root)
            .with_context(|| format!("{} is not under {}", entry_path.display(), root.display()))?
            .to_path_buf();
        if metadata.file_type().is_symlink() {
            let resolved = fs::canonicalize(&entry_path)
                .with_context(|| format!("failed to resolve symlink {}", entry_path.display()))?;
            if !resolved.starts_with(canonical_root) {
                anyhow::bail!(
                    "shared skill symlink {} points outside its skill",
                    entry_path.display()
                );
            }
            anyhow::bail!(
                "shared skill symlink {} cannot be represented safely",
                entry_path.display()
            );
        } else if metadata.is_dir() {
            tree.directories.push(ImportedDirectoryEntry {
                relative_path: relative,
                unix_mode: unix_mode(&entry_path)?,
            });
            import_tree_lossless_at(root, &entry_path, canonical_root, tree)?;
        } else if metadata.is_file() {
            tree.files.push(ImportedFile {
                relative_path: relative,
                contents: fs::read(&entry_path)
                    .with_context(|| format!("failed to read {}", entry_path.display()))?,
                unix_mode: unix_mode(&entry_path)?,
            });
        } else {
            anyhow::bail!(
                "shared skill entry {} has an unsupported filesystem type",
                entry_path.display()
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
fn unix_mode(path: &Path) -> Result<Option<u32>> {
    use std::os::unix::fs::PermissionsExt;
    Ok(Some(
        fs::metadata(path)
            .with_context(|| format!("failed to inspect {}", path.display()))?
            .permissions()
            .mode()
            & 0o777,
    ))
}

#[cfg(not(unix))]
fn unix_mode(_path: &Path) -> Result<Option<u32>> {
    Ok(None)
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

    #[cfg(unix)]
    #[test]
    fn recursive_import_rejects_cycles_and_external_links() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        symlink(".", root.join("self")).unwrap();
        assert!(import_files_recursive(&root, &root).is_err());

        fs::remove_file(root.join("self")).unwrap();
        fs::write(temp.path().join("outside"), "outside").unwrap();
        symlink(temp.path().join("outside"), root.join("outside")).unwrap();
        assert!(import_files_recursive(&root, &root).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn recursive_import_filters_before_reading_file_contents() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("keep.md"), "keep").unwrap();
        let ignored = temp.path().join("ignored.bin");
        fs::write(&ignored, vec![0u8; 1024 * 1024]).unwrap();
        fs::set_permissions(&ignored, fs::Permissions::from_mode(0o000)).unwrap();

        let files = import_files_recursive_filtered(temp.path(), temp.path(), &|relative| {
            relative
                .extension()
                .is_some_and(|extension| extension == "md")
        })
        .unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, Path::new("keep.md"));
    }
}
