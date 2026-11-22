//! The daemon's data directory, owned by one daemon at a time.

use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const LOCK_FILE: &str = "combined.lock";

#[derive(Debug)]
pub enum DataDirError {
    /// The directory could not be created or its mode set.
    Prepare { path: PathBuf, source: io::Error },
    /// The lock file could not be opened or locked.
    Lock { path: PathBuf, source: io::Error },
    /// Another daemon holds the lock.
    Locked { path: PathBuf },
}

impl fmt::Display for DataDirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataDirError::Prepare { path, source } => {
                write!(f, "cannot prepare data dir {}: {source}", path.display())
            }
            DataDirError::Lock { path, source } => {
                write!(f, "cannot lock {}: {source}", path.display())
            }
            DataDirError::Locked { path } => {
                write!(f, "another combined is using data dir {}", path.display())
            }
        }
    }
}

impl std::error::Error for DataDirError {}

/// An open data directory. It holds `combined.lock` until dropped, so its files have one writer.
#[derive(Debug)]
pub struct DataDir {
    path: PathBuf,
    _lock: File,
}

impl DataDir {
    /// Creates `path` with mode 0700 (tightening it if it exists) and takes its lock.
    pub fn open(path: &Path) -> Result<DataDir, DataDirError> {
        let prepare = |source| DataDirError::Prepare {
            path: path.to_path_buf(),
            source,
        };
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(prepare)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(prepare)?;

        let lock_path = path.join(LOCK_FILE);
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&lock_path)
            .map_err(|source| DataDirError::Lock {
                path: lock_path.clone(),
                source,
            })?;
        match lock.try_lock() {
            Ok(()) => Ok(DataDir {
                path: path.to_path_buf(),
                _lock: lock,
            }),
            Err(TryLockError::WouldBlock) => Err(DataDirError::Locked {
                path: path.to_path_buf(),
            }),
            Err(TryLockError::Error(source)) => Err(DataDirError::Lock {
                path: lock_path,
                source,
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn host_json(&self) -> PathBuf {
        self.path.join("host.json")
    }

    pub fn ids_json(&self) -> PathBuf {
        self.path.join("ids.json")
    }
}
