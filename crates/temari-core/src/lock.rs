use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

use fs2::FileExt;

use crate::{
    Error, FsIdentity,
    filesystem::{canonical_directory, io_error},
};

/// An exclusive advisory lock for one canonical source directory.
///
/// The lock is attached to the open directory descriptor, so it leaves no
/// marker inside the organized tree and is released if the process exits.
#[derive(Debug)]
pub struct SourceLock {
    directory: File,
    source: PathBuf,
    identity: FsIdentity,
}

impl SourceLock {
    pub fn acquire(source: &Path) -> Result<Self, Error> {
        let (source, identity) = canonical_directory(source)?;
        let directory = File::open(&source).map_err(|error| io_error("open", &source, error))?;
        FileExt::try_lock_exclusive(&directory).map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                Error::InvalidState(format!(
                    "source directory is already locked: {:?}",
                    source.display().to_string()
                ))
            } else {
                io_error("lock", &source, error)
            }
        })?;
        Ok(Self {
            directory,
            source,
            identity,
        })
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    pub fn identity(&self) -> &FsIdentity {
        &self.identity
    }

    pub(crate) fn validate_source(&self, source: &str, identity: &FsIdentity) -> Result<(), Error> {
        if self.source != Path::new(source) || self.identity != *identity {
            return Err(Error::InvalidState(
                "source lock does not belong to the requested source identity".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_recovery_source(
        &self,
        source: &str,
        identity: &FsIdentity,
    ) -> Result<(), Error> {
        if self.source != Path::new(source) || self.identity.inode != identity.inode {
            return Err(Error::InvalidState(
                "source lock does not belong to the requested recovery source".into(),
            ));
        }
        Ok(())
    }
}

impl Drop for SourceLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.directory);
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn excludes_a_second_lock_and_releases_on_drop() {
        let source = tempdir().unwrap();
        let first = SourceLock::acquire(source.path()).unwrap();

        let error = SourceLock::acquire(source.path()).unwrap_err();
        assert!(error.to_string().contains("already locked"));

        drop(first);
        SourceLock::acquire(source.path()).unwrap();
    }

    #[test]
    fn validates_the_locked_source_identity() {
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        let lock = SourceLock::acquire(first.path()).unwrap();
        let (second_path, second_identity) = canonical_directory(second.path()).unwrap();

        assert!(
            lock.validate_source(second_path.to_str().unwrap(), &second_identity)
                .is_err()
        );
    }
}
