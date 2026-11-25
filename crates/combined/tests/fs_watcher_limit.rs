#![cfg(all(feature = "test-support", target_os = "macos"))]
//! A watch the platform refuses is an error and is retried, never a silently unwatched space.
//!
//! macOS caps the FSEvents paths a process watches at a twelfth of its soft `RLIMIT_NOFILE`. The
//! scenario runs in a child process, this test binary run again with [`CHILD`] set, so the limit
//! it lowers is not the test binary's own.

mod daemon_support;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use combined::adapters::fs::watcher::SpaceWatcher;
use combined::model::SpaceId;
use combined::ports::FileChange;
use daemon_support::ShortTempDir;
use tokio::sync::mpsc;

const CHILD: &str = "COMBINED_WATCH_LIMIT_CHILD";
const CHILD_TEST: &str = "child_watches_under_a_small_file_limit";
const SPACES: usize = 30;
const SMALL_LIMIT: u64 = 256;
const RAISED_LIMIT: u64 = 4096;
const BOUND: Duration = Duration::from_secs(10);

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

#[test]
fn watching_past_the_platform_cap_fails_loudly_and_retries() {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .status()
        .unwrap();
    assert!(status.success(), "the child scenario failed: {status}");
}

#[test]
fn child_watches_under_a_small_file_limit() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let scratch = ShortTempDir::new();
    warm_up_fsevents(&scratch);
    set_soft_limit(SMALL_LIMIT);
    let spaces: Vec<SpaceId> = (0..SPACES)
        .map(|n| {
            let folder = scratch.path().join(format!("s{n}"));
            fs::create_dir_all(folder.join(".workspace")).unwrap();
            SpaceId(folder.to_str().unwrap().to_string())
        })
        .collect();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let watcher = SpaceWatcher::new(tx).unwrap();

    let results: Vec<_> = spaces.iter().map(|space| watcher.watch(space)).collect();
    let failed: Vec<&SpaceId> = spaces
        .iter()
        .zip(&results)
        .filter_map(|(space, result)| result.is_err().then_some(space))
        .collect();
    assert!(
        !failed.is_empty() && failed.len() < SPACES,
        "expected some spaces over the cap and some under it, got {} of {SPACES} failed",
        failed.len()
    );
    let paths = watcher.watched_paths();
    for (space, result) in spaces.iter().zip(&results) {
        let folder = Path::new(&space.0);
        let workspace = folder.join(".workspace");
        let held = paths.contains(&folder.to_path_buf()) || paths.contains(&workspace);
        match result {
            Ok(()) => assert!(
                watcher.is_armed(space) && paths.contains(&workspace),
                "{} reported watched but is not",
                space.0
            ),
            Err(error) => {
                assert!(
                    !watcher.is_armed(space),
                    "{} is armed after {error}",
                    space.0
                );
                assert!(
                    !held,
                    "{} left a registration behind after {error}",
                    space.0
                );
            }
        }
    }

    for space in &failed {
        assert!(
            watcher.watch(space).is_err(),
            "{} was reported watched on a retry with the cap still reached",
            space.0
        );
    }

    set_soft_limit(RAISED_LIMIT);
    for space in &failed {
        watcher
            .watch(space)
            .unwrap_or_else(|error| panic!("{} is still refused: {error}", space.0));
        assert!(watcher.is_armed(space));
    }
    drop_pending(&mut rx);
    let last = failed.last().unwrap();
    let layout = Path::new(&last.0).join(".workspace/layout.json");
    let deadline = Instant::now() + BOUND;
    let mut round = 0;
    loop {
        round += 1;
        fs::write(&layout, format!("edit {round}")).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        if let Some(change) = drain(&mut rx)
            .into_iter()
            .find(|change| &change.space == *last && change.path == "layout.json")
        {
            assert_eq!(change.path, "layout.json");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "an edit to a space that was retried was never reported"
        );
    }
}

/// The first FSEvents stream in a process raises its soft descriptor limit once, so one runs
/// to an event before the limit is pinned.
fn warm_up_fsevents(scratch: &ShortTempDir) {
    let folder = scratch.path().join("warm-up");
    fs::create_dir_all(folder.join(".workspace")).unwrap();
    let space = SpaceId(folder.to_str().unwrap().to_string());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let watcher = SpaceWatcher::new(tx).unwrap();
    watcher.watch(&space).unwrap();
    let deadline = Instant::now() + BOUND;
    let mut round = 0;
    while drain(&mut rx).is_empty() {
        assert!(Instant::now() < deadline, "the warm-up watch saw no event");
        round += 1;
        fs::write(folder.join(".workspace/layout.json"), format!("{round}")).unwrap();
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn drain(rx: &mut mpsc::UnboundedReceiver<FileChange>) -> Vec<FileChange> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

fn drop_pending(rx: &mut mpsc::UnboundedReceiver<FileChange>) {
    drain(rx);
}
