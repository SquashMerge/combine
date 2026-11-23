#![cfg(feature = "test-support")]
//! AC4 end to end: the conflict policy through the running server over real files
//! (`FsFileStore`, the real watcher, a real outside edit by temp file and rename). The core
//! policy is unit-tested in `layout_sync.rs`; this proves the wiring around it.

mod daemon_support;

use std::fs;
use std::path::Path;
use std::sync::Arc;

use combine_protocol::{
    ClientMessage, DaemonMessage, LayoutNode, LayoutOp, LayoutSnapshot, MutateLayout, Ratio,
    RequestId, SplitDirection, UnwatchLayout, WatchLayout,
};
use combined::adapters::fs::file_store::FsFileStore;
use combined::core::{parse_layout, LayoutTiming, LAYOUT_FILE};
use combined::model::SplitNode;
use daemon_support::{
    layout_server, refuse_default_herdr_core, seed_project_terminal, wait_until, ShortTempDir,
    WireClient,
};

const HOUR: LayoutTiming = LayoutTiming {
    debounce_ms: 3_600_000,
    max_wait_ms: 3_600_000,
    retry_ms: 3_600_000,
};

/// FSEvents can be slow, so a push caused by an outside edit gets the bound `fs_watcher.rs` uses.
const PUSH_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

fn layout_reply(client: &WireClient, request: Option<u64>) -> LayoutSnapshot {
    let message = match request {
        Some(_) => client.recv(),
        None => client
            .recv_within(PUSH_WAIT)
            .unwrap_or_else(|| panic!("no push within {PUSH_WAIT:?}")),
    };
    match message {
        DaemonMessage::Layout(layout) => {
            assert_eq!(layout.request, request.map(RequestId));
            layout.layout
        }
        other => panic!("expected a layout, got {other:?}"),
    }
}

fn watch(client: &mut WireClient, space: &str, request: u64) -> LayoutSnapshot {
    client.send(&ClientMessage::WatchLayout(WatchLayout {
        request: RequestId(request),
        space: space.to_string(),
    }));
    layout_reply(client, Some(request))
}

fn mutate(
    client: &mut WireClient,
    space: &str,
    request: u64,
    base: u64,
    ops: Vec<LayoutOp>,
) -> LayoutSnapshot {
    client.send(&ClientMessage::MutateLayout(MutateLayout {
        request: RequestId(request),
        space: space.to_string(),
        base_layout_revision: base,
        ops,
    }));
    layout_reply(client, Some(request))
}

fn ratio_of(root: &LayoutNode) -> f64 {
    match root {
        LayoutNode::Split { ratio, .. } => ratio.get(),
        LayoutNode::Tile { .. } => panic!("the root is a tile"),
    }
}

fn main_panels(root: &LayoutNode) -> Vec<String> {
    match root {
        LayoutNode::Split { a, .. } => main_panels(a),
        LayoutNode::Tile { id, panels, .. } if id == "main" => panels.clone(),
        LayoutNode::Tile { .. } => panic!("not the main tile"),
    }
}

/// What someone else would write: `main` | `side`, with the given ratio and `main` panels.
fn outside_json(ratio: f64, panels: &[&str]) -> String {
    let list = panels
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(",");
    let active = panels
        .first()
        .map_or("null".to_string(), |p| format!("\"{p}\""));
    format!(
        r#"{{"version":1,"root":{{"type":"split","dir":"h","ratio":{ratio},
        "a":{{"type":"tile","id":"main","panels":[{list}],"active":{active}}},
        "b":{{"type":"tile","id":"side","panels":[],"active":null}}}},"groups":[]}}"#
    )
}

/// Replaces the file the way an editor does: a temp file renamed onto it.
fn outside_write(work: &Path, json: &str) {
    let dir = work.join(".workspace");
    let temp = dir.join("layout.json.outside-tmp");
    fs::write(&temp, json).unwrap();
    fs::rename(&temp, dir.join(LAYOUT_FILE)).unwrap();
}

fn stored(work: &Path) -> combined::model::SpaceLayout {
    let bytes = fs::read(work.join(".workspace").join(LAYOUT_FILE)).unwrap();
    parse_layout(&serde_json::from_slice(&bytes).unwrap()).unwrap()
}

fn stored_ratio(work: &Path) -> f64 {
    match stored(work).root {
        SplitNode::Split(split) => split.ratio,
        SplitNode::Tile(_) => panic!("the root is a tile"),
    }
}

/// Serves a project over real files, splits it and flushes, then leaves one pending
/// `set_ratio(0.3)` at revision 2. Returns what the tests need.
struct Pending {
    _scratch: ShortTempDir,
    server: daemon_support::LayoutServer,
    client: WireClient,
}

fn pending_ratio_change() -> Pending {
    let scratch = ShortTempDir::new();
    let fallback = scratch.path().join("spaces");
    let store = Arc::new(FsFileStore::with_fallback(&fallback));
    let server = layout_server(&scratch, "c", store, HOUR, |_, backend| {
        seed_project_terminal(backend, &scratch)
    });
    refuse_default_herdr_core(&server.socket, &scratch);
    let space = server.work.to_str().unwrap().to_string();
    let mut client = WireClient::open(&server.socket).0;
    watch(&mut client, &space, 1);
    let split = vec![LayoutOp::SplitTile {
        tile: "main".to_string(),
        dir: SplitDirection::H,
        new_tile: "side".to_string(),
        ratio: Ratio::new(0.5).unwrap(),
        panel: None,
    }];
    assert_eq!(mutate(&mut client, &space, 2, 0, split).layout_revision, 1);
    // An unwatch flushes; the reply to the next watch proves it was handled.
    client.send(&ClientMessage::UnwatchLayout(UnwatchLayout {}));
    assert_eq!(watch(&mut client, &space, 3).layout_revision, 1);
    assert_eq!(stored_ratio(&server.work), 0.5);

    let set = vec![LayoutOp::SetRatio {
        leaves: vec!["main".to_string(), "side".to_string()],
        ratio: Ratio::new(0.3).unwrap(),
    }];
    let reply = mutate(&mut client, &space, 4, 1, set);
    assert_eq!(reply.layout_revision, 2);
    assert_eq!(
        stored_ratio(&server.work),
        0.5,
        "the change is still pending"
    );
    Pending {
        _scratch: scratch,
        server,
        client,
    }
}

#[test]
fn outside_edit_to_other_fields_keeps_pending_change_and_writes_both() {
    let mut p = pending_ratio_change();
    outside_write(&p.server.work, &outside_json(0.5, &["p1"]));

    let push = layout_reply(&p.client, None);
    assert_eq!(push.layout_revision, 3);
    assert_eq!(push.dropped, None, "a disjoint edit drops nothing");
    assert_eq!(main_panels(&push.root), ["p1"], "the outside edit wins");
    assert_eq!(
        ratio_of(&push.root),
        0.3,
        "the pending change is re-applied"
    );

    assert!(p.server.server.shutdown().is_none());
    let file = stored(&p.server.work);
    match file.root {
        SplitNode::Split(split) => {
            assert_eq!(split.ratio, 0.3);
            match *split.a {
                SplitNode::Tile(tile) => assert_eq!(tile.panels.len(), 1),
                SplitNode::Split(_) => panic!("not a tile"),
            }
        }
        SplitNode::Tile(_) => panic!("the root is a tile"),
    }
}

#[test]
fn outside_edit_to_same_field_wins_and_reports_dropped_change() {
    let mut p = pending_ratio_change();
    outside_write(&p.server.work, &outside_json(0.7, &[]));

    let push = layout_reply(&p.client, None);
    assert_eq!(push.layout_revision, 3);
    assert_eq!(ratio_of(&push.root), 0.7, "the outside edit wins");
    let dropped = push
        .dropped
        .expect("the pending change is reported dropped");
    assert_eq!(dropped.op_count, 1);

    assert!(p.server.server.shutdown().is_none());
    assert_eq!(stored_ratio(&p.server.work), 0.7, "nothing was re-applied");
}

/// AC2 "a quit flushes immediately", on the path a normal client quit takes: the socket closes,
/// the server sees `Closed`, and removing the connection unwatches its space (which flushes).
#[test]
fn normal_connection_close_flushes_pending_layout() {
    let scratch = ShortTempDir::new();
    let fallback = scratch.path().join("spaces");
    let store = Arc::new(FsFileStore::with_fallback(&fallback));
    let server = layout_server(&scratch, "q", store, HOUR, |_, backend| {
        seed_project_terminal(backend, &scratch)
    });
    refuse_default_herdr_core(&server.socket, &scratch);
    let space = server.work.to_str().unwrap().to_string();
    let layout_path = server.work.join(".workspace").join(LAYOUT_FILE);
    let mut client = WireClient::open(&server.socket).0;
    watch(&mut client, &space, 1);
    let split = vec![LayoutOp::SplitTile {
        tile: "main".to_string(),
        dir: SplitDirection::H,
        new_tile: "side".to_string(),
        ratio: Ratio::new(0.5).unwrap(),
        panel: None,
    }];
    assert_eq!(mutate(&mut client, &space, 2, 0, split).layout_revision, 1);
    assert!(!layout_path.exists(), "nothing is written before the close");

    drop(client);

    assert!(
        wait_until(std::time::Duration::from_secs(10), || layout_path.exists()),
        "closing the connection did not flush the pending layout"
    );
    assert_eq!(stored_ratio(&server.work), 0.5);
}
