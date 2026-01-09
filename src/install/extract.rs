use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub fn extract_zip(zip_path: &Path, dest_dir: &Path) -> Result<()> {
    let file = fs::File::open(zip_path).map_err(|err| Error::io(zip_path, err))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|err| Error::InvalidInput(format!("invalid zip file: {}", err)))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|err| Error::InvalidInput(format!("invalid zip entry: {}", err)))?;
        let outpath = dest_dir.join(file.mangled_name());

        if file.name().ends_with('/') {
            fs::create_dir_all(&outpath).map_err(|err| Error::io(&outpath, err))?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent).map_err(|err| Error::io(parent, err))?;
            }
            let mut outfile = fs::File::create(&outpath).map_err(|err| Error::io(&outpath, err))?;
            io::copy(&mut file, &mut outfile).map_err(|err| Error::io(&outpath, err))?;
        }
    }

    Ok(())
}

pub fn find_repo_root(extract_dir: &Path) -> PathBuf {
    let mut entries = match fs::read_dir(extract_dir) {
        Ok(entries) => entries,
        Err(_) => return extract_dir.to_path_buf(),
    };

    if let Some(Ok(entry)) = entries.next() {
        let path = entry.path();
        if path.is_dir() && entries.next().is_none() {
            return path;
        }
    }

    extract_dir.to_path_buf()
}
