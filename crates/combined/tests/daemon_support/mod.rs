#![allow(dead_code)]
//! Scratch paths and safety checks shared by the daemon's test crates. Every test passes
//! explicit paths under its own scratch directory and never touches a default location.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// The longest socket path macOS accepts.
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

/// Panics on a path outside the scratch dir and on every spelling of herdr-core's default socket.
pub fn refuse_default_herdr_core(path: &Path, scratch: &ShortTempDir) {
    check_in_scratch(path, scratch);
    let mut defaults = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        defaults.push(Path::new(&home).join(".local/state/herdr-core/default/default.sock"));
    }
    if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        defaults.push(Path::new(&state).join("herdr-core/default/default.sock"));
    }
    if let Some(socket) = std::env::var_os("HC_SOCKET") {
        defaults.push(PathBuf::from(socket));
    }
    for default in defaults {
        assert!(
            path != default,
            "{} is herdr-core's default socket",
            path.display()
        );
    }
}

/// The three paths a daemon needs, all inside the scratch dir.
#[derive(Debug, Clone)]
pub struct DaemonPaths {
    pub data_dir: PathBuf,
    pub socket: PathBuf,
    pub herdr_core_socket: PathBuf,
}

impl DaemonPaths {
    pub fn in_scratch(scratch: &ShortTempDir, name: &str) -> DaemonPaths {
        let base = scratch.path().join(name);
        let data_dir = base.join("data");
        DaemonPaths {
            socket: data_dir.join("combined.sock"),
            data_dir,
            herdr_core_socket: base.join("no-hc.sock"),
        }
    }

    pub fn with_socket(mut self, scratch: &ShortTempDir, socket: PathBuf) -> DaemonPaths {
        refuse_default_herdr_core(&socket, scratch);
        self.socket = socket;
        self
    }

    pub fn with_herdr_core_socket(
        mut self,
        scratch: &ShortTempDir,
        socket: PathBuf,
    ) -> DaemonPaths {
        refuse_default_herdr_core(&socket, scratch);
        self.herdr_core_socket = socket;
        self
    }
}

/// Writes `<scratch>/<name>/daemon.json` with every field set, and returns its path.
pub fn write_daemon_config(scratch: &ShortTempDir, paths: &DaemonPaths, backend: &str) -> PathBuf {
    for path in [&paths.data_dir, &paths.socket, &paths.herdr_core_socket] {
        refuse_default_herdr_core(path, scratch);
    }
    let config = scratch.path().join("daemon.json");
    let text = serde_json::json!({
        "version": 1,
        "backend": backend,
        "herdr_core_socket": paths.herdr_core_socket,
        "data_dir": paths.data_dir,
        "socket": paths.socket,
    });
    fs::write(&config, serde_json::to_vec_pretty(&text).unwrap()).unwrap();
    config
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
