//! Crash-safe file replacement.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{AtFlags, Mode, OFlags};

/// Makes each call's temp name unique within the process.
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// `.<name>.tmp-<pid>-<n>`, where `n` is unique to the call.
fn temp_name(name: &str) -> String {
    format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    )
}

/// Replaces `path` with `bytes`: writes `.<name>.tmp-<pid>-<n>` beside it, where `n` is unique to the call with `mode`, syncs it and
/// renames it over `path`, then syncs the directory on a best-effort basis.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no file name", path.display()),
        )
    })?;
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let temp = dir.join(temp_name(&name.to_string_lossy()));

    let mut file = create_temp(&temp, mode)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&temp, path));
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    if let Ok(directory) = File::open(dir) {
        let _ = directory.sync_all();
    }
    Ok(())
}

/// A leftover from an earlier process with the same PID is removed and the create retried once.
fn create_temp(temp: &PathBuf, mode: u32) -> io::Result<File> {
    let open = || {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(temp)
    };
    match open() {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            fs::remove_file(temp)?;
            open()
        }
        other => other,
    }
}

/// [`write_atomic`] relative to an open directory: replaces `name` in `dir` with `bytes`. The
/// temp file is created in `dir` with `O_EXCL` and `O_NOFOLLOW`, synced and `renameat`ed over
/// `name`, and `dir` is then synced on a best-effort basis. No step resolves a path through
/// anything but `dir`, and the rename replaces a symlink at `name` instead of following it.
pub fn write_atomic_at(dir: &File, name: &str, bytes: &[u8], mode: Mode) -> io::Result<()> {
    let temp = temp_name(name);
    let mut file = create_temp_at(dir, &temp, mode)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| rustix::fs::renameat(dir, &temp, dir, name).map_err(io::Error::from));
    if let Err(error) = written {
        let _ = rustix::fs::unlinkat(dir, &temp, AtFlags::empty());
        return Err(error);
    }
    let _ = dir.sync_all();
    Ok(())
}

/// [`create_temp`] relative to `dir`.
fn create_temp_at(dir: &File, temp: &str, mode: Mode) -> io::Result<File> {
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let open = || {
        rustix::fs::openat(dir, temp, flags, mode)
            .map(File::from)
            .map_err(io::Error::from)
    };
    match open() {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            rustix::fs::unlinkat(dir, temp, AtFlags::empty())?;
            open()
        }
        other => other,
    }
}
