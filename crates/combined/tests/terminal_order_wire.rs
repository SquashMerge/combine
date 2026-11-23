#![cfg(feature = "test-support")]
//! The daemon lists a space's terminals in the backend's order, not in ID order.

mod common;
mod daemon_support;

use std::sync::Arc;

use combine_protocol::{ClientMessage, DaemonMessage, RequestId, TermId, WatchTerminals};
use combined::ports::TerminalBackend;
use combined::rpc::ServeOptions;
use combined::testing::{FakeBackend, FakeConfig};
use common::spawn_spec;
use daemon_support::{fake_server, work_dir, DaemonPaths, ShortTempDir, WireClient};
use futures::executor::block_on;

#[test]
fn fake_server_lists_eleven_terminals_in_creation_order() {
    let scratch = ShortTempDir::new();
    let (backend, _control) = FakeBackend::new(FakeConfig::default());
    let _server = fake_server(
        &scratch,
        "d",
        Arc::clone(&backend),
        "5b0c1f3e-6a3d-4c52-9d0e-2f7a1b8c4d90",
        ServeOptions::default(),
    );
    let work = work_dir(&scratch);
    let spawned: Vec<TermId> = (0..11)
        .map(|_| {
            let spec = spawn_spec(work.to_str().unwrap(), 80, 24);
            block_on(backend.spawn(spec)).unwrap()
        })
        .collect();

    let (mut client, _) = WireClient::open(&DaemonPaths::in_scratch(&scratch, "d").socket);
    client.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(1),
    }));
    // Wait (bounded; `recv` itself times out) until all eleven are listed, so a snapshot taken
    // before the daemon's list caught up with the seeding cannot fail the test.
    let snapshot = (0..200)
        .find_map(|_| match client.recv() {
            DaemonMessage::Terminals(terminals) if terminals.terminals.len() == spawned.len() => {
                Some(terminals)
            }
            _ => None,
        })
        .expect("all eleven terminals were never listed");

    let listed: Vec<TermId> = snapshot.terminals.iter().map(|t| t.id.clone()).collect();
    assert_eq!(listed, spawned);
    assert_eq!(snapshot.spaces.len(), 1);
    assert_eq!(snapshot.spaces[0].terminals, spawned);
    let mut by_id = spawned.clone();
    by_id.sort_by(|a, b| a.0.cmp(&b.0));
    assert_ne!(
        spawned, by_id,
        "the ids must sort differently from creation order"
    );
}
