#![cfg(feature = "test-support")]
//! `terminals` snapshots name the backend and its instance.

mod common;
mod daemon_support;

use std::sync::Arc;

use combine_protocol::{BackendInfo, ClientMessage, DaemonMessage, RequestId, WatchTerminals};
use combined::core::TerminalList;
use combined::rpc::ServeOptions;
use combined::testing::{FakeBackend, FakeConfig};
use common::FakeGitProbe;
use daemon_support::{fake_server, DaemonPaths, ShortTempDir, WireClient};

#[test]
fn watch_terminals_names_the_backend_and_its_instance() {
    let scratch = ShortTempDir::new();
    let (backend, _control) = FakeBackend::new(FakeConfig::default());
    let _server = fake_server(
        &scratch,
        "d",
        Arc::clone(&backend),
        "5b0c1f3e-6a3d-4c52-9d0e-2f7a1b8c4d90",
        ServeOptions::default(),
    );
    let (mut client, _) = WireClient::open(&DaemonPaths::in_scratch(&scratch, "d").socket);
    client.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(1),
    }));
    let DaemonMessage::Terminals(snapshot) = client.recv() else {
        panic!("the first message answers the watch");
    };
    assert_eq!(snapshot.request, Some(RequestId(1)));
    assert_eq!(
        snapshot.backend,
        Some(BackendInfo {
            id: "fake".to_string(),
            instance: Some("fake-core".to_string()),
        })
    );
}

#[test]
fn a_list_names_no_backend_until_it_is_told() {
    let mut list = TerminalList::new(Box::new(FakeGitProbe::default()));
    assert_eq!(list.snapshot(None).backend, None);
    let backend = BackendInfo {
        id: "fake".to_string(),
        instance: None,
    };
    list.set_backend(backend.clone());
    assert_eq!(list.snapshot(None).backend, Some(backend));
}
