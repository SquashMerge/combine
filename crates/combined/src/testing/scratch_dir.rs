#![cfg(feature = "test-support")]
//! Scratch directories and polls shared by the test crates. Every test passes explicit paths
//! under its own scratch directory and never touches a default location.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// The longest socket path a unix socket takes on macOS, with the terminating NUL left out.
const MAX_SOCKET_PATH: usize = 103;

static NEXT: AtomicU32 = AtomicU32::new(0);

/// `/tmp/cs4-<pid>-<n>`, mode 0700, removed on drop.
pub struct ShortTempDir {
    path: PathBuf,
}

impl ShortTempDir {
    pub fn new() -> ShortTempDir {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let path = PathBuf::from(format!("/tmp/cs4-{}-{n}", std::process::id()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        ShortTempDir { path }
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
