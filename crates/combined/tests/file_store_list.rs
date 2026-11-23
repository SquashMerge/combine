#![cfg(feature = "test-support")]
//! `FileStore::list_dir` on disk and in memory.
//!
//! Names that aren't UTF-8 aren't tested: APFS refuses to create them.

mod daemon_support;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use combined::adapters::fs::file_store::FsFileStore;
use combined::model::SpaceId;
use combined::ports::{EntryKind, FileStore, ListedEntry, StoreError};
use combined::testing::MemoryFileStore;
use daemon_support::ShortTempDir;
use futures::executor::block_on;

fn space(scratch: &ShortTempDir) -> SpaceId {
    SpaceId(scratch.path().join("project").to_str().unwrap().to_string())
}

fn entry(name: &str, kind: EntryKind) -> ListedEntry {
    ListedEntry {
        name: name.to_string(),
        kind,
    }
}

fn make_panels(scratch: &ShortTempDir) -> std::path::PathBuf {
    let panels = scratch.path().join("project/.workspace/panels");
    fs::create_dir_all(&panels).unwrap();
    panels
}

#[test]
fn fs_missing_folder_is_none() {
    let scratch = ShortTempDir::new();
    let space = space(&scratch);
    let store = FsFileStore::new();
    assert_eq!(block_on(store.list_dir(&space, "panels", 10)), Ok(None));
    fs::create_dir_all(scratch.path().join("project/.workspace")).unwrap();
    assert_eq!(block_on(store.list_dir(&space, "panels", 10)), Ok(None));
    assert_eq!(block_on(store.list_dir(&space, "a/b", 10)), Ok(None));
    assert_eq!(
        block_on(store.list_dir(&space, "../x", 10)),
        Err(StoreError::InvalidPath)
    );
    assert_eq!(
        block_on(store.list_dir(&space, "", 10)),
        Err(StoreError::InvalidPath)
    );
}

#[test]
fn fs_lists_kinds_sorted() {
    let scratch = ShortTempDir::new();
    let space = space(&scratch);
    let panels = make_panels(&scratch);
    fs::create_dir(panels.join("zeta")).unwrap();
    fs::write(panels.join("beta"), b"x").unwrap();
    fs::create_dir(panels.join("alpha")).unwrap();
    symlink(scratch.path(), panels.join("link")).unwrap();
    let listing = block_on(FsFileStore::new().list_dir(&space, "panels", 10))
        .unwrap()
        .unwrap();
    assert!(!listing.truncated);
    assert_eq!(
        listing.entries,
        vec![
            entry("alpha", EntryKind::Directory),
            entry("beta", EntryKind::File),
            entry("link", EntryKind::Symlink),
            entry("zeta", EntryKind::Directory),
        ]
    );
}

#[test]
fn fs_symlinked_workspace_or_panels_refused() {
    let scratch = ShortTempDir::new();
    let space = space(&scratch);
    let elsewhere = scratch.path().join("elsewhere");
    fs::create_dir_all(elsewhere.join("panels/p")).unwrap();
    let store = FsFileStore::new();

    fs::create_dir_all(scratch.path().join("project")).unwrap();
    symlink(&elsewhere, scratch.path().join("project/.workspace")).unwrap();
    let refused = block_on(store.list_dir(&space, "panels", 10));
    assert!(
        matches!(refused, Err(StoreError::Unavailable(_))),
        "{refused:?}"
    );

    fs::remove_file(scratch.path().join("project/.workspace")).unwrap();
    fs::create_dir(scratch.path().join("project/.workspace")).unwrap();
    symlink(
        elsewhere.join("panels"),
        scratch.path().join("project/.workspace/panels"),
    )
    .unwrap();
    let refused = block_on(store.list_dir(&space, "panels", 10));
    assert!(
        matches!(refused, Err(StoreError::Unavailable(_))),
        "{refused:?}"
    );

    let nested = block_on(store.list_dir(&space, "panels/p", 10));
    assert!(
        matches!(nested, Err(StoreError::Unavailable(_))),
        "{nested:?}"
    );
}

#[test]
fn fs_truncates_at_max() {
    let scratch = ShortTempDir::new();
    let space = space(&scratch);
    let panels = make_panels(&scratch);
    for name in ["a", "b", "c", "d"] {
        fs::create_dir(Path::new(&panels).join(name)).unwrap();
    }
    let store = FsFileStore::new();
    let cut = block_on(store.list_dir(&space, "panels", 3))
        .unwrap()
        .unwrap();
    assert!(cut.truncated);
    assert_eq!(cut.entries.len(), 3);
    let exact = block_on(store.list_dir(&space, "panels", 4))
        .unwrap()
        .unwrap();
    assert!(!exact.truncated);
    assert_eq!(exact.entries.len(), 4);
    let none = block_on(store.list_dir(&space, "panels", 0))
        .unwrap()
        .unwrap();
    assert!(none.truncated);
    assert!(none.entries.is_empty());
}

#[test]
fn memory_lists_dirs_and_files() {
    let store = MemoryFileStore::new();
    let space = SpaceId("/p".to_string());
    assert_eq!(block_on(store.list_dir(&space, "panels", 10)), Ok(None));
    block_on(store.write(&space, "panels/b/panel.json", b"{}".to_vec())).unwrap();
    block_on(store.write(&space, "panels/a/panel.json", b"{}".to_vec())).unwrap();
    block_on(store.write(&space, "panels/c.txt", b"x".to_vec())).unwrap();
    block_on(store.write(&space, "other/z", b"x".to_vec())).unwrap();
    let listing = block_on(store.list_dir(&space, "panels", 10))
        .unwrap()
        .unwrap();
    assert!(!listing.truncated);
    assert_eq!(
        listing.entries,
        vec![
            entry("a", EntryKind::Directory),
            entry("b", EntryKind::Directory),
            entry("c.txt", EntryKind::File),
        ]
    );
    let cut = block_on(store.list_dir(&space, "panels", 2))
        .unwrap()
        .unwrap();
    assert!(cut.truncated);
    assert_eq!(cut.entries.len(), 2);
    assert_eq!(store.list_count(&space, "panels"), 3);
}

#[test]
fn memory_rejects_invalid_path() {
    let store = MemoryFileStore::new();
    let space = SpaceId("/p".to_string());
    for path in ["", "../x", "a//b", "/abs", "a/./b"] {
        assert_eq!(
            block_on(store.list_dir(&space, path, 10)),
            Err(StoreError::InvalidPath),
            "{path:?}"
        );
        assert_eq!(store.list_count(&space, path), 0);
    }
}
