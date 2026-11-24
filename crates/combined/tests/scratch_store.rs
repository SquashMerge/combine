#![cfg(feature = "test-support")]
//! The on-disk scratch panel store: listing, reading, writing, removing, the symlink refusal
//! and the folder and file modes.

mod daemon_support;

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;

use combined::adapters::fs::scratch_store::FsScratchStore;
use combined::ports::{ScratchDir, ScratchStore, StoreError, StoreRevision};
use daemon_support::ShortTempDir;
use futures::executor::block_on;

const BOUND: usize = 1 << 20;

fn key(digit: char) -> String {
    digit.to_string().repeat(64)
}

fn dir(digit: char, panel: &str) -> ScratchDir {
    ScratchDir {
        key: key(digit),
        panel: panel.to_string(),
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn open() -> (ShortTempDir, FsScratchStore) {
    let scratch = ShortTempDir::new();
    let store = FsScratchStore::new(scratch.path().join("scratch"));
    (scratch, store)
}

fn unavailable(result: &Result<impl std::fmt::Debug, StoreError>) -> &str {
    match result {
        Err(StoreError::Unavailable(message)) => message,
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[test]
fn write_then_read_round_trips() {
    let (_scratch, store) = open();
    let panel = dir('a', "p1");
    assert_eq!(block_on(store.read(&panel, "panel.json", BOUND)), Ok(None));
    let first = block_on(store.write(&panel, "panel.json", b"one".to_vec())).unwrap();
    let second = block_on(store.write(&dir('b', "p2"), "content.md", b"two".to_vec())).unwrap();
    assert_eq!(first.revision, StoreRevision(1));
    assert_eq!(second.revision, StoreRevision(2));
    assert!(!first.read_only);
    let file = block_on(store.read(&panel, "panel.json", BOUND))
        .unwrap()
        .unwrap();
    assert_eq!(file.bytes, b"one");
    assert_eq!(file.hash, first.hash);
    assert!(!file.read_only);
    block_on(store.write(&panel, "panel.json", b"three".to_vec())).unwrap();
    let file = block_on(store.read(&panel, "panel.json", BOUND))
        .unwrap()
        .unwrap();
    assert_eq!(file.bytes, b"three");
    assert_eq!(block_on(store.read(&panel, "diagram.mmd", BOUND)), Ok(None));
    assert!(matches!(
        block_on(store.read(&panel, "panel.json", 2)),
        Err(StoreError::TooLarge {
            size: 5,
            read_only: false
        })
    ));
}

#[test]
fn names_outside_the_rules_are_invalid() {
    let (scratch, store) = open();
    let good = dir('a', "p1");
    for file in [
        "",
        "other.txt",
        "../panel.json",
        "a/panel.json",
        ".panel.json",
    ] {
        assert_eq!(
            block_on(store.write(&good, file, b"x".to_vec())),
            Err(StoreError::InvalidPath),
            "{file:?}"
        );
        assert_eq!(
            block_on(store.read(&good, file, BOUND)),
            Err(StoreError::InvalidPath),
            "{file:?}"
        );
    }
    for bad in [
        ScratchDir {
            key: "a".repeat(63),
            panel: "p".into(),
        },
        ScratchDir {
            key: "A".repeat(64),
            panel: "p".into(),
        },
        ScratchDir {
            key: "g".repeat(64),
            panel: "p".into(),
        },
        ScratchDir {
            key: key('a'),
            panel: String::new(),
        },
        ScratchDir {
            key: key('a'),
            panel: "a/b".into(),
        },
        ScratchDir {
            key: key('a'),
            panel: ".hidden".into(),
        },
    ] {
        assert_eq!(
            block_on(store.write(&bad, "panel.json", b"x".to_vec())),
            Err(StoreError::InvalidPath),
            "{bad:?}"
        );
        assert_eq!(
            block_on(store.read(&bad, "panel.json", BOUND)),
            Err(StoreError::InvalidPath),
            "{bad:?}"
        );
        assert_eq!(block_on(store.remove(&bad)), Err(StoreError::InvalidPath));
    }
    assert!(!scratch.path().join("scratch").exists());
}

#[test]
fn list_is_sorted_real_directories_only() {
    let (scratch, store) = open();
    let root = scratch.path().join("scratch");
    assert_eq!(
        block_on(store.list(10)).unwrap().dirs,
        [],
        "a missing root lists nothing"
    );
    for (digit, panel) in [('b', "p2"), ('a', "p9"), ('a', "p1")] {
        block_on(store.write(&dir(digit, panel), "panel.json", b"x".to_vec())).unwrap();
    }
    fs::write(root.join("a-file"), b"x").unwrap();
    fs::write(root.join(key('a')).join("a-file"), b"x").unwrap();
    let outside = scratch.path().join("outside");
    fs::create_dir_all(outside.join("inner")).unwrap();
    symlink(&outside, root.join(key('c'))).unwrap();
    symlink(&outside, root.join(key('a')).join("linked")).unwrap();

    let listing = block_on(store.list(10)).unwrap();
    assert!(!listing.truncated);
    assert_eq!(
        listing.dirs,
        [dir('a', "p1"), dir('a', "p9"), dir('b', "p2")]
    );
    let cut = block_on(store.list(2)).unwrap();
    assert!(cut.truncated);
    assert_eq!(cut.dirs, [dir('a', "p1"), dir('a', "p9")]);
    let exact = block_on(store.list(3)).unwrap();
    assert!(!exact.truncated);
}

#[test]
fn remove_deletes_the_panel_and_then_an_empty_key_folder() {
    let (scratch, store) = open();
    let root = scratch.path().join("scratch");
    block_on(store.write(&dir('a', "p1"), "panel.json", b"x".to_vec())).unwrap();
    block_on(store.write(&dir('a', "p1"), "diagram.mmd", b"x".to_vec())).unwrap();
    block_on(store.write(&dir('a', "p2"), "panel.json", b"x".to_vec())).unwrap();

    block_on(store.remove(&dir('a', "p1"))).unwrap();
    assert!(!root.join(key('a')).join("p1").exists());
    assert!(root.join(key('a')).join("p2").is_dir());
    block_on(store.remove(&dir('a', "p2"))).unwrap();
    assert!(!root.join(key('a')).exists());
    assert!(root.is_dir());
    block_on(store.remove(&dir('a', "p2"))).unwrap();
    block_on(store.remove(&dir('f', "none"))).unwrap();
}

#[test]
fn symlinks_are_refused_at_every_component() {
    let (scratch, store) = open();
    let root = scratch.path().join("scratch");
    let outside = scratch.path().join("outside");
    fs::create_dir_all(outside.join("p1")).unwrap();
    fs::write(outside.join("p1").join("panel.json"), b"secret").unwrap();
    fs::create_dir(&root).unwrap();
    let panel = dir('a', "p1");

    // A symlinked key folder.
    symlink(&outside, root.join(key('a'))).unwrap();
    let read = block_on(store.read(&panel, "panel.json", BOUND));
    assert!(unavailable(&read).contains("is a symlink; the scratch store does not follow it"));
    assert!(
        unavailable(&block_on(store.write(&panel, "panel.json", b"x".to_vec())))
            .contains("is a symlink")
    );
    assert!(unavailable(&block_on(store.remove(&panel))).contains("is a symlink"));
    assert!(outside.join("p1").join("panel.json").exists());
    fs::remove_file(root.join(key('a'))).unwrap();

    // A symlinked panel folder.
    fs::create_dir(root.join(key('a'))).unwrap();
    symlink(outside.join("p1"), root.join(key('a')).join("p1")).unwrap();
    assert!(
        unavailable(&block_on(store.read(&panel, "panel.json", BOUND))).contains("is a symlink")
    );
    assert!(unavailable(&block_on(store.remove(&panel))).contains("is a symlink"));
    fs::remove_file(root.join(key('a')).join("p1")).unwrap();

    // A symlinked file.
    fs::create_dir(root.join(key('a')).join("p1")).unwrap();
    symlink(
        outside.join("p1").join("panel.json"),
        root.join(key('a')).join("p1").join("panel.json"),
    )
    .unwrap();
    assert!(
        unavailable(&block_on(store.read(&panel, "panel.json", BOUND))).contains("is a symlink")
    );
    assert!(
        unavailable(&block_on(store.write(&panel, "panel.json", b"x".to_vec())))
            .contains("is a symlink")
    );
    assert_eq!(
        fs::read(outside.join("p1").join("panel.json")).unwrap(),
        b"secret"
    );

    // A symlinked root.
    let linked = FsScratchStore::new(scratch.path().join("link"));
    symlink(&root, scratch.path().join("link")).unwrap();
    assert!(unavailable(&block_on(linked.list(10))).contains("is a symlink"));
    assert!(
        unavailable(&block_on(linked.write(&panel, "content.md", b"x".to_vec())))
            .contains("is a symlink")
    );
}

#[test]
fn folders_are_0700_and_files_0600() {
    let (scratch, store) = open();
    let root = scratch.path().join("scratch");
    block_on(store.write(&dir('a', "p1"), "scratch.json", b"{}".to_vec())).unwrap();
    assert_eq!(mode(&root), 0o700);
    assert_eq!(mode(&root.join(key('a'))), 0o700);
    assert_eq!(mode(&root.join(key('a')).join("p1")), 0o700);
    assert_eq!(
        mode(&root.join(key('a')).join("p1").join("scratch.json")),
        0o600
    );
}
