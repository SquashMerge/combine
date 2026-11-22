//! Crash-safe file replacement.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// Replaces `path` with `bytes`: writes `.<name>.tmp-<pid>` beside it with `mode`, syncs it and
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
    let temp = dir.join(format!(
        ".{}.tmp-{}",
        name.to_string_lossy(),
        std::process::id()
    ));

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

/// A leftover from an earlier run with the same PID is removed and the create retried once.
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
