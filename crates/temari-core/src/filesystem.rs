use std::{
    fs::{self, File, Metadata},
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, artifact::normalize_relative_path};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FsIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileFingerprint {
    pub identity: FsIdentity,
    pub size: u64,
    pub sha256: String,
}

pub(crate) fn canonical_directory(path: &Path) -> Result<(PathBuf, FsIdentity), Error> {
    let canonical = fs::canonicalize(path).map_err(|source| io_error("resolve", path, source))?;
    let metadata = fs::symlink_metadata(&canonical)
        .map_err(|source| io_error("inspect", &canonical, source))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(Error::InvalidArtifact(format!(
            "source must be a real directory: {:?}",
            canonical.display().to_string()
        )));
    }
    Ok((canonical, identity(&metadata)))
}

pub(crate) fn fingerprint(path: &Path) -> Result<FileFingerprint, Error> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("inspect", path, source))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(Error::InvalidArtifact(format!(
            "planned source must be a regular non-symlink file: {:?}",
            path.display().to_string()
        )));
    }
    let mut file = File::open(path).map_err(|source| io_error("open", path, source))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|source| io_error("read", path, source))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(FileFingerprint {
        identity: identity(&metadata),
        size: metadata.len(),
        sha256: format!("{:x}", hasher.finalize()),
    })
}

pub(crate) fn identity(metadata: &Metadata) -> FsIdentity {
    FsIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

pub(crate) fn checked_join(root: &Path, relative: &str) -> Result<PathBuf, Error> {
    normalize_relative_path(relative)?;
    Ok(root.join(relative))
}

pub(crate) fn verify_directory_chain(root: &Path, relative: &str) -> Result<(), Error> {
    normalize_relative_path(relative)?;
    let mut current = root.to_path_buf();
    for component in relative.split('/') {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                    return Err(Error::InvalidArtifact(format!(
                        "destination component must be a real directory: {:?}",
                        current.display().to_string()
                    )));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error("inspect", &current, source)),
        }
    }
    Ok(())
}

pub(crate) fn path_exists(path: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error("inspect", path, source)),
    }
}

pub(crate) fn io_error(action: &'static str, path: &Path, source: io::Error) -> Error {
    Error::FileSystem {
        action,
        path: path.display().to_string(),
        source,
    }
}
