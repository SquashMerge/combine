#![cfg(feature = "test-support")]
//! Scratch directories and polls shared by the test crates. Every test passes explicit paths
//! under its own scratch directory and never touches a default location.

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

/// The longest socket path a unix socket takes on macOS, with the terminating NUL left out.
const MAX_SOCKET_PATH: usize = 103;

/// How many names [`fresh_dir`] tries before it gives up.
const FRESH_DIR_ATTEMPTS: u32 = 16;

/// Creates and returns a new directory `<parent>/<prefix>-<8 random hex digits>`, mode 0700,
/// as `mkdtemp` does: a name that already exists, such as one a killed run left behind, is
/// never reused, but retried with a fresh suffix. The name has no pid or counter in it, since
/// pids are reused across the many test runs on one machine.
pub fn fresh_dir(parent: &Path, prefix: &str) -> PathBuf {
    fresh_dir_from(parent, prefix, random_u32)
}

/// [`fresh_dir`], with the suffixes drawn from `next`.
fn fresh_dir_from(parent: &Path, prefix: &str, mut next: impl FnMut() -> u32) -> PathBuf {
    for _ in 0..FRESH_DIR_ATTEMPTS {
        let path = parent.join(format!("{prefix}-{:08x}", next()));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {
                // The umask can only clear bits, but say 0700 outright.
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
                return path;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("creating {}: {error}", path.display()),
        }
    }
    panic!(
        "no free {prefix}-* name under {} after {FRESH_DIR_ATTEMPTS} tries",
        parent.display()
    );
}

/// 32 random bits from std's per-thread random hash keys, which the OS seeds and each
/// `RandomState::new` steps, so successive calls differ within a process and across them.
fn random_u32() -> u32 {
    RandomState::new().build_hasher().finish() as u32
}

/// `/tmp/cs4-<8 random hex digits>`, mode 0700, removed on drop. It is short because macOS caps
/// a socket path at [`MAX_SOCKET_PATH`] bytes, and made by [`fresh_dir`], so it is always new.
pub struct ShortTempDir {
    path: PathBuf,
}

impl ShortTempDir {
    pub fn new() -> ShortTempDir {
        ShortTempDir {
            path: fresh_dir(Path::new("/tmp"), "cs4"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A socket path in the scratch dir, checked against the length limit.
    pub fn sock(&self, name: &str) -> PathBuf {
        let path = self.path.join(name);
        assert!(
            path.as_os_str().len() <= MAX_SOCKET_PATH,
            "socket path {} is over {MAX_SOCKET_PATH} bytes",
            path.display()
        );
        path
    }
}

impl Default for ShortTempDir {
    fn default() -> Self {
        ShortTempDir::new()
    }
}

impl Drop for ShortTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Panics unless `path` is inside the scratch dir and has no `..` component.
pub fn check_in_scratch(path: &Path, scratch: &ShortTempDir) {
    assert!(
        !path.components().any(|part| part == Component::ParentDir),
        "{} has a `..` component",
        path.display()
    );
    assert!(
        path.starts_with(scratch.path()),
        "{} is outside the scratch dir {}",
        path.display(),
        scratch.path().display()
    );
}

/// Polls `cond` until it holds or `timeout` passes; returns whether it held.
pub fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `<scratch>/work`: where in-process servers' terminals run.
pub fn work_dir(scratch: &ShortTempDir) -> PathBuf {
    scratch.path().join("work")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn short_temp_dirs_are_new_short_private_and_removed_on_drop() {
        let a = ShortTempDir::new();
        let b = ShortTempDir::new();
        assert_ne!(a.path(), b.path());
        for dir in [&a, &b] {
            assert_eq!(mode(dir.path()), 0o700);
            // "/tmp/cs4-" and eight hex digits: no longer than the old "/tmp/cs4-<pid>-<n>".
            assert_eq!(dir.path().as_os_str().len(), 17, "{}", dir.path().display());
        }
        let gone = a.path().to_path_buf();
        drop(a);
        assert!(!gone.exists());
    }

    #[test]
    fn a_name_that_already_exists_is_skipped_not_reused() {
        let parent = ShortTempDir::new();
        let stale = parent.path().join("t-00000001");
        fs::create_dir(&stale).unwrap();
        fs::write(stale.join("left-behind"), b"x").unwrap();
        let mut suffixes = [1, 1, 2].into_iter();
        let fresh = fresh_dir_from(parent.path(), "t", || suffixes.next().unwrap());
        assert_eq!(fresh, parent.path().join("t-00000002"));
        assert_eq!(mode(&fresh), 0o700);
        assert!(
            fs::read_dir(&fresh).unwrap().next().is_none(),
            "it is empty"
        );
        assert!(
            stale.join("left-behind").exists(),
            "the stale dir is left alone"
        );
    }

    #[test]
    #[should_panic(expected = "no free t-* name")]
    fn it_gives_up_after_a_bounded_number_of_tries() {
        let parent = ShortTempDir::new();
        fs::create_dir(parent.path().join("t-00000007")).unwrap();
        fresh_dir_from(parent.path(), "t", || 7);
    }
}
