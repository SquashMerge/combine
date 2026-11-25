#![cfg(feature = "test-support")]
//! A space the file store can't watch is tried again until it can be, and what was missed while
//! it wasn't watched is read once it is.

mod daemon_support;

use std::sync::Arc;
use std::time::Duration;

use combine_protocol::{ClientMessage, DaemonMessage, RequestId, WatchLayout};
use combined::core::LAYOUT_FILE;
use combined::model::SpaceId;
use combined::ports::StoreError;
use combined::rpc::ServeOptions;
use combined::testing::{FakeBackend, FakeConfig, MemoryFileStore};
use daemon_support::{
    refuse_default_herdr_core, seed_project_terminal, server_on_with, wait_until, DaemonPaths,
    ShortTempDir, WireClient,
};

const WAIT: Duration = Duration::from_secs(10);
const RETRY: Duration = Duration::from_millis(50);

#[test]
fn a_refused_watch_is_retried_until_it_succeeds_and_then_reread() {
    let scratch = ShortTempDir::new();
    let store = Arc::new(MemoryFileStore::new());
    let (backend, _control) = FakeBackend::new(FakeConfig::default());
    seed_project_terminal(&backend, &scratch);
    let _server = server_on_with(
        &scratch,
        "w",
        backend,
        store.clone(),
        ServeOptions {
            watch_retry: RETRY,
            ..ServeOptions::default()
        },
    );
    let socket = DaemonPaths::in_scratch(&scratch, "w").socket;
    refuse_default_herdr_core(&socket, &scratch);
    let space = SpaceId(scratch.path().join("work").to_str().unwrap().to_string());
    store.fail_watch(
        &space,
        Some(StoreError::Unavailable(
            "too many watched paths".to_string(),
        )),
    );

    let mut client = WireClient::open(&socket).0;
    client.send(&ClientMessage::WatchLayout(WatchLayout {
        request: RequestId(1),
        space: space.0.clone(),
    }));
    assert!(matches!(client.recv(), DaemonMessage::Layout(_)));
    assert!(
        wait_until(WAIT, || store.watch_count(&space) >= 3),
        "the refused watch was tried {} times",
        store.watch_count(&space)
    );

    let reads = store.read_count(&space, LAYOUT_FILE);
    store.fail_watch(&space, None);
    assert!(
        wait_until(WAIT, || store.read_count(&space, LAYOUT_FILE) > reads),
        "the layout was not read again once the watch took hold"
    );
    let tried = store.watch_count(&space);
    std::thread::sleep(RETRY * 6);
    assert_eq!(
        store.watch_count(&space),
        tried,
        "a space that is watched is still being retried"
    );
}
