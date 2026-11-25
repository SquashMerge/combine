#![cfg(feature = "test-support")]
//! The longest panel folder name the daemon allows is one the file system can hold.

mod daemon_support;

use combined::adapters::fs::file_store::FsFileStore;
use combined::core::PANEL_ID_MAX_BYTES;
use combined::model::SpaceId;
use combined::ports::{FileStore, StoreError};
use daemon_support::{check_in_scratch, ShortTempDir};
use futures::executor::block_on;

#[test]
fn fs_store_writes_a_255_byte_panel_folder() {
    let scratch = ShortTempDir::new();
    check_in_scratch(scratch.path(), &scratch);
    let space = SpaceId(scratch.path().to_str().unwrap().to_owned());
    let store = FsFileStore::new();

    let ok = "a".repeat(PANEL_ID_MAX_BYTES);
    let path = format!("panels/{ok}/panel.json");
    block_on(store.write_project(&space, &path, b"{}".to_vec())).unwrap();
    assert!(scratch
        .path()
        .join(".workspace/panels")
        .join(&ok)
        .join("panel.json")
        .is_file());

    let too_long = "b".repeat(PANEL_ID_MAX_BYTES + 1);
    let path = format!("panels/{too_long}/panel.json");
    let error = block_on(store.write_project(&space, &path, b"{}".to_vec())).unwrap_err();
    let StoreError::Unavailable(text) = error else {
        panic!("{error:?}");
    };
    assert!(text.to_lowercase().contains("name too long"), "{text}");
}
