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

/// How many temp names [`write_atomic_at`] tries before it gives up on a folder where each
/// one it picks is already taken.
const TEMP_ATTEMPTS: usize = 8;

/// A temp name for [`write_atomic_at`]: `.<name>.tmp-<pid>-<n>-<random>`, so a name taken by
/// another entry is never picked twice.
fn fresh_temp_name(name: &str) -> String {
    use std::hash::{BuildHasher, RandomState};
    let call = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let random = RandomState::new().hash_one(call);
    format!(".{name}.tmp-{}-{call}-{random:016x}", std::process::id())
}

/// A file's device and inode.
type Identity = (rustix::fs::Dev, u64);

fn identity(stat: &rustix::fs::Stat) -> Identity {
    (stat.st_dev, stat.st_ino)
}

/// The identity of the entry `name` in `dir`, without following a symlink.
fn entry_identity(dir: &File, name: &str) -> io::Result<Identity> {
    Ok(identity(&rustix::fs::statat(
        dir,
        name,
        AtFlags::SYMLINK_NOFOLLOW,
    )?))
}

fn not_ours(what: &str) -> io::Error {
    io::Error::other(format!(
        "{what} was replaced by another entry while it was written"
    ))
}

/// [`write_atomic`] relative to an open directory: replaces `name` in `dir` with `bytes`, with
/// `mode`, and succeeds only when the file now at `name` is the one it wrote and synced.
///
/// The temp file is created in `dir` with `O_EXCL | O_NOFOLLOW` under a fresh name; a name
/// that is taken is left alone and another one tried. It is written and synced, checked by
/// device and inode to still be the entry at its name, `renameat`ed over `name`, and `name` is
/// then checked the same way; `dir` is synced on a best-effort basis. No step resolves a path
/// through anything but `dir`, and the rename replaces a symlink at `name` instead of
/// following it. If the temp entry or `name` turns out to be another file, the write fails and
/// that entry is left alone; on any failure the temp name is removed only while it is still
/// the file this call created.
///
/// What it can't rule out: a process that can write `dir` can still replace `name` after the
/// final check, before anyone reads it. SPEC's threat model trusts such a process.
pub fn write_atomic_at(dir: &File, name: &str, bytes: &[u8], mode: Mode) -> io::Result<()> {
    let steps = Steps {
        temp_names: &mut || fresh_temp_name(name),
        before_rename: &mut |_| {},
        after_rename: &mut || {},
    };
    write_atomic_at_with(dir, name, bytes, mode, steps)
}

/// What [`write_atomic_at_with`] takes from its caller: the temp names to try, and steps run
/// just before the rename (given the temp name) and just after it. [`write_atomic_at`] passes
/// fresh names and no-ops; tests collide with or replace the entries there.
struct Steps<'a> {
    temp_names: &'a mut dyn FnMut() -> String,
    before_rename: &'a mut dyn FnMut(&str),
    after_rename: &'a mut dyn FnMut(),
}

/// [`write_atomic_at`], with its [`Steps`] passed in.
fn write_atomic_at_with(
    dir: &File,
    name: &str,
    bytes: &[u8],
    mode: Mode,
    steps: Steps<'_>,
) -> io::Result<()> {
    let (temp, mut file) = create_temp_at(dir, mode, steps.temp_names)?;
    let ours = identity(&rustix::fs::fstat(&file)?);
    let published = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            (steps.before_rename)(&temp);
            if entry_identity(dir, &temp)? != ours {
                return Err(not_ours("the temp file"));
            }
            rustix::fs::renameat(dir, &temp, dir, name).map_err(io::Error::from)
        });
    if let Err(error) = published {
        if entry_identity(dir, &temp).ok() == Some(ours) {
            let _ = rustix::fs::unlinkat(dir, &temp, AtFlags::empty());
        }
        return Err(error);
    }
    (steps.after_rename)();
    if entry_identity(dir, name).ok() != Some(ours) {
        return Err(not_ours(name));
    }
    let _ = dir.sync_all();
    Ok(())
}

/// Creates a new temp file in `dir` under the first name from `temp_names` that isn't taken,
/// trying at most [`TEMP_ATTEMPTS`] names. A taken name is never removed.
fn create_temp_at(
    dir: &File,
    mode: Mode,
    temp_names: &mut dyn FnMut() -> String,
) -> io::Result<(String, File)> {
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut last = None;
    for _ in 0..TEMP_ATTEMPTS {
        let temp = temp_names();
        match rustix::fs::openat(dir, temp.as_str(), flags, mode) {
            Ok(fd) => return Ok((temp, File::from(fd))),
            Err(rustix::io::Errno::EXIST) => last = Some(rustix::io::Errno::EXIST),
            Err(errno) => return Err(errno.into()),
        }
    }
    Err(last.map_or_else(
        || io::Error::other("no temp name was tried"),
        io::Error::from,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A fresh folder under the system temp folder, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let path = std::env::temp_dir().join(format!(
                "combined-safe-write-{tag}-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }

        fn dir(&self) -> File {
            File::open(&self.0).unwrap()
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const MODE: Mode = Mode::RUSR.union(Mode::WUSR);

    /// Writes `out` in `scratch` with these steps.
    fn write(
        scratch: &Scratch,
        temp_names: &mut dyn FnMut() -> String,
        before_rename: &mut dyn FnMut(&str),
        after_rename: &mut dyn FnMut(),
    ) -> io::Result<()> {
        let steps = Steps {
            temp_names,
            before_rename,
            after_rename,
        };
        write_atomic_at_with(&scratch.dir(), "out", b"ours", MODE, steps)
    }

    #[test]
    fn a_taken_temp_name_is_left_alone_and_another_one_tried() {
        let scratch = Scratch::new("taken");
        fs::write(scratch.0.join(".out.tmp-taken"), b"someone else's").unwrap();
        let mut names = [".out.tmp-taken", ".out.tmp-free"].into_iter();
        let mut next = || names.next().unwrap().to_string();
        write(&scratch, &mut next, &mut |_| {}, &mut || {}).unwrap();
        assert_eq!(fs::read(scratch.0.join("out")).unwrap(), b"ours");
        assert_eq!(
            fs::read(scratch.0.join(".out.tmp-taken")).unwrap(),
            b"someone else's"
        );
        assert_eq!(scratch.names(), [".out.tmp-taken", "out"]);
    }

    #[test]
    fn every_temp_name_taken_fails_and_removes_none() {
        let scratch = Scratch::new("all-taken");
        fs::write(scratch.0.join(".out.tmp-taken"), b"someone else's").unwrap();
        let mut tries = 0;
        let mut same = || {
            tries += 1;
            ".out.tmp-taken".to_string()
        };
        let error = write(&scratch, &mut same, &mut |_| {}, &mut || {}).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{error}");
        assert_eq!(tries, TEMP_ATTEMPTS);
        assert_eq!(scratch.names(), [".out.tmp-taken"]);
    }

    #[test]
    fn a_temp_replaced_before_the_rename_fails_and_is_left_alone() {
        let scratch = Scratch::new("temp-replaced");
        fs::write(scratch.0.join("out"), b"before").unwrap();
        let root = scratch.0.clone();
        let mut replace = |temp: &str| {
            fs::write(root.join("foreign"), b"foreign").unwrap();
            fs::rename(root.join("foreign"), root.join(temp)).unwrap();
        };
        let mut names = || ".out.tmp-1".to_string();
        let error = write(&scratch, &mut names, &mut replace, &mut || {}).unwrap_err();
        assert!(error.to_string().contains("replaced"), "{error}");
        assert_eq!(fs::read(scratch.0.join("out")).unwrap(), b"before");
        assert_eq!(fs::read(scratch.0.join(".out.tmp-1")).unwrap(), b"foreign");
    }

    #[test]
    fn a_target_replaced_after_the_rename_fails_and_is_left_alone() {
        let scratch = Scratch::new("target-replaced");
        let root = scratch.0.clone();
        let mut replace = || {
            fs::write(root.join("foreign"), b"foreign").unwrap();
            fs::rename(root.join("foreign"), root.join("out")).unwrap();
        };
        let mut names = || ".out.tmp-1".to_string();
        let error = write(&scratch, &mut names, &mut |_| {}, &mut replace).unwrap_err();
        assert!(error.to_string().contains("replaced"), "{error}");
        assert_eq!(fs::read(scratch.0.join("out")).unwrap(), b"foreign");
        assert_eq!(scratch.names(), ["out"]);
    }

    #[test]
    fn a_failed_rename_removes_its_own_temp() {
        let scratch = Scratch::new("rename-fails");
        // A folder with an entry at the target: renaming a file over it fails.
        fs::create_dir_all(scratch.0.join("out/inside")).unwrap();
        let mut names = || ".out.tmp-1".to_string();
        write(&scratch, &mut names, &mut |_| {}, &mut || {}).unwrap_err();
        assert_eq!(scratch.names(), ["out"]);
    }

    #[test]
    fn fresh_temp_names_differ_and_stay_hidden() {
        let (a, b) = (fresh_temp_name("out"), fresh_temp_name("out"));
        assert_ne!(a, b);
        assert!(a.starts_with(".out.tmp-"), "{a}");
    }
}
