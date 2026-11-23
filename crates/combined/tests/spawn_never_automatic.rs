#![cfg(feature = "test-support")]
//! The daemon starts a terminal only in answer to `spawn_terminal`: nothing else it does, from
//! starting up to restarting the backend, calls the backend's spawn.

mod daemon_support;
mod layout_support;
mod spawn_support;

use std::sync::Arc;
use std::time::Duration;

use combine_protocol::{ClientMessage, DaemonMessage, RequestId, WatchLayout};
use combined::core::LAYOUT_FILE;
use combined::ports::FileStore;
use combined::testing::MemoryFileStore;
use daemon_support::{server_on, DaemonPaths, WireClient};
use layout_support::{bytes_of, layout, split, tile};
use spawn_support::*;

const QUIET: Duration = Duration::from_millis(300);

/// A rig whose project has valid templates and a layout naming them.
fn rig_with_templates() -> (Rig, Arc<MemoryFileStore>) {
    let (rig, store) = Rig::in_memory();
    for panel in ["agent", "build"] {
        write_panel_memory(
            &store,
            &rig.space,
            panel,
            &terminal_panel_json(&["sh"], None),
        );
    }
    let tiles = split(
        0.5,
        tile("left", &["agent"], Some("agent")),
        tile("right", &["build"], Some("build")),
    );
    store.write_outside(&rig.space, LAYOUT_FILE, bytes_of(&layout(tiles)));
    assert_eq!(spawn_attempts(&rig.control), 1);
    (rig, store)
}

fn watch_layout(client: &mut WireClient, rig: &Rig, request: u64) {
    client.send(&ClientMessage::WatchLayout(WatchLayout {
        request: RequestId(request),
        space: rig.name(),
    }));
    loop {
        if let DaemonMessage::Layout(layout) = client.recv() {
            if layout.request == Some(RequestId(request)) {
                return;
            }
        }
    }
}

#[test]
fn no_spawn_on_connect_watch_list_and_mutate() {
    let (rig, store) = rig_with_templates();
    let mut client = rig.client();
    watch_terminals(&mut client, 1);
    watch_layout(&mut client, &rig, 2);
    client.send(&ClientMessage::MutateLayout(
        combine_protocol::MutateLayout {
            request: RequestId(3),
            space: rig.name(),
            base_layout_revision: 0,
            ops: vec![combine_protocol::LayoutOp::SetActive {
                tile: "left".to_string(),
                panel: "agent".to_string(),
            }],
        },
    ));
    assert!(client.recv_within(QUIET).is_some());
    std::thread::sleep(QUIET);

    assert_eq!(spawn_attempts(&rig.control), 1);
    assert_eq!(store.list_count(&rig.space, "panels"), 0);
    assert_eq!(store.read_count(&rig.space, "panels/agent/panel.json"), 0);
    assert_eq!(store.read_count(&rig.space, "panels/build/panel.json"), 0);

    let templates = rig.list(&mut client, 4);
    assert_eq!(templates.templates.len(), 2);
    assert_eq!(store.list_count(&rig.space, "panels"), 1);
    assert_eq!(spawn_attempts(&rig.control), 1);
}

#[test]
fn no_spawn_on_layout_or_panel_file_change() {
    let (rig, store) = rig_with_templates();
    let mut client = rig.client();
    watch_layout(&mut client, &rig, 1);

    for panel in ["agent", "build"] {
        write_panel_memory(
            &store,
            &rig.space,
            panel,
            &terminal_panel_json(&["zsh"], None),
        );
    }
    store.write_outside(
        &rig.space,
        LAYOUT_FILE,
        bytes_of(&layout(tile("only", &["agent"], Some("agent")))),
    );
    match client.recv() {
        DaemonMessage::Layout(push) => assert_eq!(push.request, None),
        other => panic!("expected the layout push, got {other:?}"),
    }
    std::thread::sleep(QUIET);

    assert_eq!(spawn_attempts(&rig.control), 1);
    assert_eq!(store.read_count(&rig.space, "panels/agent/panel.json"), 0);
}

#[test]
fn no_spawn_on_backend_restart() {
    let (rig, _store) = rig_with_templates();
    let mut client = rig.client();
    watch_terminals(&mut client, 1);

    rig.control.restart_backend();
    std::thread::sleep(QUIET);
    let _ = client.recv_within(QUIET);

    assert_eq!(spawn_attempts(&rig.control), 1);
}

#[test]
fn no_spawn_on_daemon_restart() {
    let (mut rig, store) = rig_with_templates();
    assert!(rig.server.shutdown().is_none());

    let store: Arc<dyn FileStore> = store;
    let _again = server_on(&rig.scratch, "d2", Arc::clone(&rig.backend), store);
    let socket = DaemonPaths::in_scratch(&rig.scratch, "d2").socket;
    let mut client = WireClient::open(&socket).0;
    watch_terminals(&mut client, 1);
    std::thread::sleep(QUIET);

    assert_eq!(spawn_attempts(&rig.control), 1);
}

#[test]
fn exactly_one_spawn_per_spawn_terminal() {
    let (rig, _store) = rig_with_templates();
    let mut client = rig.client();

    for request in 1..=2 {
        rig.spawn(&mut client, request, combine_protocol::SpawnSource::Shell);
        expect_spawned(&client, request);
        assert_eq!(spawn_attempts(&rig.control), 1 + request as usize);
    }
    std::thread::sleep(QUIET);
    assert_eq!(spawn_attempts(&rig.control), 3);
}
