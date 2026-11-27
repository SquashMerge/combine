#![cfg(feature = "test-support")]
//! AC4 end to end: the conflict policy through the running server over real files
//! (`FsFileStore`, the real watcher, a real outside edit by temp file and rename). The core
//! policy is unit-tested in `layout_sync.rs`; this proves the wiring around it.

mod common;
mod daemon_support;
mod panel_support;

use std::fs;
use std::path::Path;
use std::sync::Arc;

use combine_protocol::{
    ClientMessage, DaemonMessage, LayoutNode, LayoutOp, LayoutSnapshot, MutateLayout, Ratio,
    RequestId, SplitDirection, UnwatchLayout, WatchLayout,
};
use combined::adapters::fs::file_store::FsFileStore;
use combined::core::{parse_layout, LayoutTiming, LAYOUT_FILE};
use combined::model::{SpaceId, SplitNode};
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

/// The next message, skipping the `panels` frames a client that watches a space is sent. Use it
/// only on a client that has sent `watch_layout`.
fn recv_skipping_panels(client: &WireClient) -> DaemonMessage {
    loop {
        match client.recv() {
            DaemonMessage::Panels(_) => {}
            other => return other,
        }
    }
}

/// [`WireClient::recv_within`], skipping `panels` frames. `None` when nothing else arrives
/// within `wait`.
fn recv_within_skipping_panels(
    client: &WireClient,
    wait: std::time::Duration,
) -> Option<DaemonMessage> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match client.recv_within(left)? {
            DaemonMessage::Panels(_) => {}
            other => return Some(other),
        }
    }
}

fn layout_reply(client: &WireClient, request: Option<u64>) -> LayoutSnapshot {
    let message = match request {
        Some(_) => recv_skipping_panels(client),
        None => recv_within_skipping_panels(client, PUSH_WAIT)
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
    server: daemon_support::LayoutServer,
    client: WireClient,
    /// Last: fields drop in order, so the server stops (and flushes) before its folder goes.
    _scratch: ShortTempDir,
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

/// #57: dropping a fixture stops its server before its scratch dir goes. The other way round,
/// the server's last flush wrote a layout never yet written into a folder already removed, and
/// recreated the dir (mode 0755, holding only `work/.workspace/layout.json`) after `Drop`.
#[test]
fn dropping_a_fixture_with_an_unwritten_layout_leaves_no_folder_behind() {
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
    assert!(
        !server.work.join(".workspace").join(LAYOUT_FILE).exists(),
        "the change is still pending"
    );
    let root = scratch.path().to_path_buf();
    drop(Pending {
        server,
        client,
        _scratch: scratch,
    });
    assert!(
        !root.exists(),
        "{} was recreated after Drop",
        root.display()
    );
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

const QUICK: LayoutTiming = LayoutTiming {
    debounce_ms: 20,
    max_wait_ms: 40,
    retry_ms: 3_600_000,
};

/// Makes `file` unreadable and says whether that holds for this user: it doesn't for root.
fn lock_file(file: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(file, fs::Permissions::from_mode(0o000)).unwrap();
    fs::read(file).is_err()
}

fn unlock_file(file: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(file, fs::Permissions::from_mode(0o644)).unwrap();
}

/// Spawns a terminal in `cwd`, which makes `cwd` a space.
fn spawn_in(backend: &Arc<combined::testing::FakeBackend>, cwd: &Path) {
    use combined::ports::TerminalBackend;
    let spec = combined::model::SpawnSpec {
        argv: vec!["sh".to_string()],
        cwd: cwd.to_str().unwrap().to_string(),
        env: Default::default(),
        labels: Default::default(),
        cols: 80,
        rows: 24,
    };
    futures::executor::block_on(TerminalBackend::spawn(&**backend, spec)).unwrap();
}

/// The answer to `request`, skipping any pushes before it.
fn answer(client: &WireClient, request: u64) -> LayoutSnapshot {
    loop {
        match client.recv() {
            DaemonMessage::Panels(_) => continue,
            DaemonMessage::Layout(layout) if layout.request.is_none() => continue,
            DaemonMessage::Layout(layout) => {
                assert_eq!(layout.request, Some(RequestId(request)));
                return layout.layout;
            }
            other => panic!("expected a layout, got {other:?}"),
        }
    }
}

fn split_main() -> Vec<LayoutOp> {
    vec![LayoutOp::SplitTile {
        tile: "main".to_string(),
        dir: SplitDirection::H,
        new_tile: "side".to_string(),
        ratio: Ratio::new(0.5).unwrap(),
        panel: None,
    }]
}

/// Finding 1, over real files: an invalid `layout.json` the daemon can't read (mode 000, in a
/// writable folder, so a rename over it would succeed) is never replaced, whether the held
/// change would go out on its debounce, an unwatch, a switch to another space or shutdown.
#[test]
fn unreadable_layout_file_is_never_replaced() {
    let scratch = ShortTempDir::new();
    let fallback = scratch.path().join("spaces");
    let store = Arc::new(FsFileStore::with_fallback(&fallback));
    let other = scratch.path().join("other");
    fs::create_dir_all(&other).unwrap();
    let mut server = layout_server(&scratch, "u", store, QUICK, |_, backend| {
        seed_project_terminal(backend, &scratch);
        spawn_in(backend, &other);
    });
    refuse_default_herdr_core(&server.socket, &scratch);
    let space = server.work.to_str().unwrap().to_string();
    let dir = server.work.join(".workspace");
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join(LAYOUT_FILE);
    let original = b"{ \"version\": 1, \"root\": unread and invalid".to_vec();
    fs::write(&file, &original).unwrap();
    if !lock_file(&file) {
        unlock_file(&file);
        eprintln!("skipped: this user can read a mode 000 file");
        return;
    }

    let mut client = WireClient::open(&server.socket).0;
    let snapshot = watch(&mut client, &space, 1);
    assert_eq!(snapshot.layout_revision, 0);
    let error = snapshot.error.expect("the failed read is reported");
    assert!(error.starts_with("Couldn't read layout.json ("), "{error}");
    assert!(error.contains("until layout.json can be read"), "{error}");

    // Accepted and held in memory.
    assert_eq!(
        mutate(&mut client, &space, 2, 0, split_main()).layout_revision,
        1
    );
    // Well past the debounce and the max wait.
    std::thread::sleep(std::time::Duration::from_millis(300));
    // An unwatch flushes; the reply to the next watch proves it was handled.
    client.send(&ClientMessage::UnwatchLayout(UnwatchLayout {}));
    client.send(&ClientMessage::WatchLayout(WatchLayout {
        request: RequestId(3),
        space: space.clone(),
    }));
    answer(&client, 3);
    // A switch to another space flushes the old one.
    client.send(&ClientMessage::WatchLayout(WatchLayout {
        request: RequestId(4),
        space: other.to_str().unwrap().to_string(),
    }));
    answer(&client, 4);
    assert!(server.server.shutdown().is_none());

    unlock_file(&file);
    assert_eq!(
        fs::read(&file).unwrap(),
        original,
        "the unread file was replaced"
    );
    assert!(!fallback.exists(), "the held change went to the fallback");
    let leftovers: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name != LAYOUT_FILE)
        .collect();
    assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
}

/// Finding 1, over real files: once the file can be read again, the held change goes through
/// the conflict rule against what it holds, and is written.
#[test]
fn held_change_is_written_once_the_file_can_be_read() {
    let scratch = ShortTempDir::new();
    let fallback = scratch.path().join("spaces");
    let store = Arc::new(FsFileStore::with_fallback(&fallback));
    let mut server = layout_server(&scratch, "r", store, HOUR, |_, backend| {
        seed_project_terminal(backend, &scratch)
    });
    refuse_default_herdr_core(&server.socket, &scratch);
    let space = server.work.to_str().unwrap().to_string();
    let dir = server.work.join(".workspace");
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join(LAYOUT_FILE);
    fs::write(&file, b"garbage").unwrap();
    if !lock_file(&file) {
        unlock_file(&file);
        eprintln!("skipped: this user can read a mode 000 file");
        return;
    }

    let mut client = WireClient::open(&server.socket).0;
    assert!(watch(&mut client, &space, 1).error.is_some());
    assert_eq!(
        mutate(&mut client, &space, 2, 0, split_main()).layout_revision,
        1
    );

    // Someone fixes the file: the default layout, so the held split still applies.
    unlock_file(&file);
    outside_write(
        &server.work,
        r#"{"version":1,"root":{"type":"tile","id":"main","panels":[],"active":null},"groups":[]}"#,
    );
    let push = layout_reply(&client, None);
    assert_eq!(push.error, None);
    assert_eq!(push.dropped, None);
    assert_eq!(ratio_of(&push.root), 0.5);

    assert!(server.server.shutdown().is_none());
    assert_eq!(stored_ratio(&server.work), 0.5);
    assert!(!fallback.exists());
}

/// Finding 4, over real files: an 8 MiB sparse `layout.json` is reported as too large, the
/// way an invalid file is, and is never replaced.
#[test]
fn oversized_layout_file_is_invalid_and_never_replaced() {
    let scratch = ShortTempDir::new();
    let fallback = scratch.path().join("spaces");
    let store = Arc::new(FsFileStore::with_fallback(&fallback));
    let mut server = layout_server(&scratch, "o", store, QUICK, |_, backend| {
        seed_project_terminal(backend, &scratch)
    });
    refuse_default_herdr_core(&server.socket, &scratch);
    let space = server.work.to_str().unwrap().to_string();
    let dir = server.work.join(".workspace");
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join(LAYOUT_FILE);
    let size: u64 = 8 << 20;
    fs::File::create(&file).unwrap().set_len(size).unwrap();

    let mut client = WireClient::open(&server.socket).0;
    let snapshot = watch(&mut client, &space, 1);
    assert_eq!(
        snapshot.error.as_deref(),
        Some(
            "layout.json can't be used: the layout is 8388608 bytes; the limit is 262144. \
             Showing the last good layout. Changes made now aren't saved, and are lost if the \
             daemon restarts, until layout.json is fixed."
        )
    );
    assert_eq!(
        mutate(&mut client, &space, 2, 0, split_main()).layout_revision,
        1
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    client.send(&ClientMessage::UnwatchLayout(UnwatchLayout {}));
    client.send(&ClientMessage::WatchLayout(WatchLayout {
        request: RequestId(3),
        space: space.clone(),
    }));
    answer(&client, 3);
    assert!(server.server.shutdown().is_none());

    assert_eq!(fs::metadata(&file).unwrap().len(), size);
    let head = &fs::read(&file).unwrap()[..64];
    assert!(head.iter().all(|byte| *byte == 0), "the file was replaced");
    assert!(!fallback.exists());
}

fn placed(name: &str) -> combined::core::PanelCreate {
    let mut create = panel_support::named(name, panel_support::markdown("x"));
    create.placement = Some("main".to_string());
    create
}

/// AC7, over real files: a placement waits for a layout file the daemon can't read and is never
/// written over it, whether it would go out on the debounce, an unwatch or shutdown.
#[test]
fn a_placement_never_replaces_an_unreadable_layout_file() {
    let scratch = ShortTempDir::new();
    let fallback = scratch.path().join("spaces");
    let store = Arc::new(FsFileStore::with_fallback(&fallback));
    let mut server = layout_server(&scratch, "p", store, QUICK, |_, backend| {
        seed_project_terminal(backend, &scratch)
    });
    refuse_default_herdr_core(&server.socket, &scratch);
    let space = server.work.to_str().unwrap().to_string();
    let dir = server.work.join(".workspace");
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join(LAYOUT_FILE);
    let original = b"{ \"version\": 1, \"root\": unread and invalid".to_vec();
    fs::write(&file, &original).unwrap();
    if !lock_file(&file) {
        unlock_file(&file);
        eprintln!("skipped: this user can read a mode 000 file");
        return;
    }

    let mut client = WireClient::open(&server.socket).0;
    assert!(watch(&mut client, &space, 1).error.is_some());
    server
        .server
        .panels()
        .create(&SpaceId(space.clone()), placed("p9"))
        .unwrap();
    // Well past the debounce and the max wait.
    std::thread::sleep(std::time::Duration::from_millis(300));
    client.send(&ClientMessage::UnwatchLayout(UnwatchLayout {}));
    client.send(&ClientMessage::WatchLayout(WatchLayout {
        request: RequestId(3),
        space,
    }));
    answer(&client, 3);
    assert!(server.server.shutdown().is_none());

    unlock_file(&file);
    assert_eq!(
        fs::read(&file).unwrap(),
        original,
        "the unread file was replaced"
    );
    assert!(!fallback.exists(), "the placement went to the fallback");
    let leftovers: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name != LAYOUT_FILE && name != "panels")
        .collect();
    assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
}

/// AC17, over real files: an outside edit to the placed tile's `panels` wins and the placement
/// is reported dropped. The panel is then placed again, without taking the tile's focus.
#[test]
fn a_placement_and_an_overlapping_outside_edit() {
    let scratch = ShortTempDir::new();
    let fallback = scratch.path().join("spaces");
    let store = Arc::new(FsFileStore::with_fallback(&fallback));
    let mut server = layout_server(&scratch, "o", store, HOUR, |_, backend| {
        seed_project_terminal(backend, &scratch)
    });
    refuse_default_herdr_core(&server.socket, &scratch);
    let space = server.work.to_str().unwrap().to_string();
    fs::create_dir_all(server.work.join(".workspace")).unwrap();
    outside_write(&server.work, &outside_json(0.5, &["p1"]));

    let mut client = WireClient::open(&server.socket).0;
    assert_eq!(main_panels(&watch(&mut client, &space, 1).root), ["p1"]);
    server
        .server
        .panels()
        .create(&SpaceId(space.clone()), placed("p9"))
        .unwrap();
    let push = layout_reply(&client, None);
    assert_eq!(main_panels(&push.root), ["p1", "p9"]);
    assert_eq!(push.dropped, None);

    outside_write(&server.work, &outside_json(0.5, &["p1", "p2"]));
    let push = loop {
        let push = layout_reply(&client, None);
        if push.dropped.is_some() {
            break push;
        }
    };
    assert_eq!(push.dropped.unwrap().op_count, 1);
    assert_eq!(main_panels(&push.root), ["p1", "p2", "p9"]);
    match &push.root {
        LayoutNode::Split { a, .. } => match &**a {
            LayoutNode::Tile { active, .. } => assert_eq!(active.as_deref(), Some("p1")),
            LayoutNode::Split { .. } => panic!("not a tile"),
        },
        LayoutNode::Tile { .. } => panic!("the root is a tile"),
    }

    assert!(server.server.shutdown().is_none());
    let file = stored(&server.work);
    let SplitNode::Split(split) = file.root else {
        panic!("the root is a tile");
    };
    let SplitNode::Tile(main) = *split.a else {
        panic!("not a tile");
    };
    let held: Vec<&str> = main.panels.iter().map(|panel| panel.as_str()).collect();
    assert_eq!(held, ["p1", "p2", "p9"]);
}
