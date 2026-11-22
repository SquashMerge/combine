//! The daemon's listening socket: one owner per path, mode 0600 in a directory only the user
//! can open.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// The longest socket path macOS accepts.
const MAX_SOCKET_PATH: usize = 103;

#[derive(Debug)]
pub enum BindError {
    /// The path is over [`MAX_SOCKET_PATH`] bytes.
    TooLong {
        path: PathBuf,
        len: usize,
    },
    /// The socket's directory is open to the group or others, or is not a directory.
    DirectoryOpen {
        path: PathBuf,
    },
    /// Something other than a socket is at the path.
    NotASocket {
        path: PathBuf,
    },
    /// Another daemon, or another listener, serves the path.
    InUse {
        path: PathBuf,
    },
    /// Probing an existing socket failed for a reason other than a refusal.
    Probe {
        path: PathBuf,
        source: io::Error,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
}

impl BindError {
    /// 2 when the setup is wrong, 1 when the system failed.
    pub fn exit_code(&self) -> u8 {
        match self {
            BindError::TooLong { .. }
            | BindError::DirectoryOpen { .. }
            | BindError::NotASocket { .. }
            | BindError::InUse { .. } => 2,
            BindError::Probe { .. } | BindError::Io { .. } => 1,
        }
    }
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BindError::TooLong { path, len } => write!(
                f,
                "socket path {} is {len} bytes; the limit is {MAX_SOCKET_PATH}",
                path.display()
            ),
            BindError::DirectoryOpen { path } => write!(
                f,
                "{} must be a directory only its owner can open (mode 0700)",
                path.display()
            ),
            BindError::NotASocket { path } => {
                write!(f, "{} exists and is not a socket", path.display())
            }
            BindError::InUse { path } => {
                write!(f, "another combined is serving {}", path.display())
            }
            BindError::Probe { path, source } => {
                write!(f, "cannot probe {}: {source}", path.display())
            }
            BindError::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for BindError {}

/// A bound listener and the `<socket>.lock` that makes it this daemon's. The lock is held for as
/// long as the value lives, and the OS releases it when the process dies.
#[derive(Debug)]
pub struct BoundSocket {
    pub listener: UnixListener,
    pub(super) lock: File,
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> BindError + '_ {
    move |source| BindError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Checks the socket's directory, creating it with mode 0700 if it is missing.
fn prepare_parent(path: &Path) -> Result<(), BindError> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    match fs::metadata(parent) {
        Ok(meta) if meta.is_dir() && meta.permissions().mode() & 0o077 == 0 => Ok(()),
        Ok(_) => Err(BindError::DirectoryOpen {
            path: parent.to_path_buf(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(io_error(parent)),
        Err(error) => Err(io_error(parent)(error)),
    }
}

/// Binds `path`. In order:
///
/// 1. the path is at most 103 bytes;
/// 2. its directory is closed to others (and is created 0700 if missing);
/// 3. an exclusive lock on `<path>.lock` is taken, which is what guarantees one daemon per path;
/// 4. an existing file must be a socket nobody answers on, and is then removed. The probe is a
///    second check, not proof of staleness: a full backlog is also refused on macOS;
/// 5. bind, then mode 0600.
pub fn bind(path: &Path) -> Result<BoundSocket, BindError> {
    let len = path.as_os_str().as_bytes().len();
    if len > MAX_SOCKET_PATH {
        return Err(BindError::TooLong {
            path: path.to_path_buf(),
            len,
        });
    }
    prepare_parent(path)?;

    let lock_path = lock_path(path);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .map_err(io_error(&lock_path))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            return Err(BindError::InUse {
                path: path.to_path_buf(),
            })
        }
        Err(TryLockError::Error(source)) => return Err(io_error(&lock_path)(source)),
    }

    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => remove_if_stale(path)?,
        Ok(_) => {
            return Err(BindError::NotASocket {
                path: path.to_path_buf(),
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(path)(error)),
    }

    let listener = UnixListener::bind(path).map_err(io_error(path))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(io_error(path))?;
    Ok(BoundSocket { listener, lock })
}

/// Removes the socket file only if connecting is refused.
fn remove_if_stale(path: &Path) -> Result<(), BindError> {
    match UnixStream::connect(path) {
        Ok(_) => Err(BindError::InUse {
            path: path.to_path_buf(),
        }),
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
            fs::remove_file(path).map_err(io_error(path))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(BindError::Probe {
            path: path.to_path_buf(),
            source,
        }),
    }
}
