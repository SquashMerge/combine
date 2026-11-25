#![cfg(all(feature = "test-support", target_os = "macos"))]
//! A `.workspace/` that can't be watched is reported and retried, whatever the reason and
//! whatever else the watcher is busy with.

mod daemon_support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use combined::adapters::fs::watcher::SpaceWatcher;
use combined::model::SpaceId;
use daemon_support::ShortTempDir;
use tokio::sync::mpsc;

const BOUND: Duration = Duration::from_secs(10);

/// Puts a folder's mode back when dropped, so the temp dir can be removed.
struct Restore(PathBuf);

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
    }
}

fn is_root() -> bool {
    // SAFETY: `geteuid` takes no arguments and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn an_unreadable_project_is_an_error_and_arms_once_readable() {
    if is_root() {
        eprintln!("skipped: root ignores folder modes");
        return;
    }
    let scratch = ShortTempDir::new();
    let folder = scratch.path().join("p");
    fs::create_dir_all(folder.join(".workspace")).unwrap();
    let _restore = Restore(folder.clone());
    let space = SpaceId(folder.to_str().unwrap().to_string());
    let (tx, _rx) = mpsc::unbounded_channel();
    let watcher = SpaceWatcher::new(tx).unwrap();

    for mode in [0o000, 0o400] {
        chmod(&folder, mode);
        assert!(
            watcher.watch(&space).is_err(),
            "mode {mode:o}: reported watched but cannot arm"
        );
        assert!(!watcher.is_armed(&space), "mode {mode:o}");
        assert!(watcher.watched_paths().is_empty(), "mode {mode:o}");
    }
    chmod(&folder, 0o700);
    watcher.watch(&space).unwrap();
    assert!(watcher.is_armed(&space));
    assert_eq!(watcher.watched_paths().len(), 2);
}

const CHILD: &str = "COMBINED_ARM_RETRY_CHILD";
const CHILD_TEST: &str = "child_retries_a_failed_late_arm_under_steady_requests";
const SMALL_LIMIT: u64 = 256;
const RAISED_LIMIT: u64 = 4096;
const RETRY: Duration = Duration::from_millis(1500);

fn set_soft_limit(soft: u64) {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a valid, writable `rlimit`.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
        0
    );
    assert!(
        limit.rlim_max >= soft,
        "hard limit {} is too low",
        limit.rlim_max
    );
    limit.rlim_cur = soft;
    // SAFETY: `limit` is a valid `rlimit` that the call only reads.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
}

fn space_in(scratch: &ShortTempDir, name: &str, workspace: bool) -> SpaceId {
    let folder = scratch.path().join(name);
    fs::create_dir_all(&folder).unwrap();
    if workspace {
        fs::create_dir(folder.join(".workspace")).unwrap();
    }
    SpaceId(folder.to_str().unwrap().to_string())
}

/// The first FSEvents stream in a process raises its soft descriptor limit once, so one runs
/// to an event before the limit is pinned.
fn warm_up_fsevents(scratch: &ShortTempDir) {
    let space = space_in(scratch, "warm-up", true);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let watcher = SpaceWatcher::new(tx).unwrap();
    watcher.watch(&space).unwrap();
    let deadline = Instant::now() + BOUND;
    let mut round = 0;
    while rx.try_recv().is_err() {
        assert!(Instant::now() < deadline, "the warm-up watch saw no event");
        round += 1;
        fs::write(
            Path::new(&space.0).join(".workspace/layout.json"),
            format!("{round}"),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_failed_late_arm_is_retried_on_time_under_steady_requests() {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .status()
        .unwrap();
    assert!(status.success(), "the child scenario failed: {status}");
}

#[test]
fn child_retries_a_failed_late_arm_under_steady_requests() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let scratch = ShortTempDir::new();
    warm_up_fsevents(&scratch);
    set_soft_limit(SMALL_LIMIT);
    let (tx, _rx) = mpsc::unbounded_channel();
    let watcher = SpaceWatcher::with_arm_retry(tx, RETRY).unwrap();
    // 9 spaces, the busy one and the failing one's folder fill the platform's 21 paths but one.
    for n in 0..9 {
        watcher
            .watch(&space_in(&scratch, &format!("s{n}"), true))
            .unwrap();
    }
    let busy = space_in(&scratch, "busy", true);
    watcher.watch(&busy).unwrap();
    let failing = space_in(&scratch, "failing", false);
    watcher.watch(&failing).unwrap();

    // Its `.workspace/` appears with no path left to watch it with, so the arm fails.
    fs::create_dir(Path::new(&failing.0).join(".workspace")).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(!watcher.is_armed(&failing));
    set_soft_limit(RAISED_LIMIT);

    // Another space's `.workspace/` comes and goes faster than the retry interval.
    let workspace = Path::new(&busy.0).join(".workspace");
    let deadline = Instant::now() + RETRY * 5;
    while Instant::now() < deadline && !watcher.is_armed(&failing) {
        let _ = fs::remove_dir_all(&workspace);
        std::thread::sleep(Duration::from_millis(100));
        let _ = fs::create_dir(&workspace);
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        watcher.is_armed(&failing),
        "the failed arm was never retried under steady requests"
    );
}
