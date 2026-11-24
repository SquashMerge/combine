#![cfg(feature = "test-support")]
//! Layout sync through the running server: a real Unix socket, the fake backend and an
//! in-memory or real file store. Waits are bounded polls; nothing asserts a duration.

mod daemon_support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use combine_protocol::{
    ClientMessage, DaemonMessage, ErrorCode, LayoutNode, LayoutOp, LayoutSnapshot, MutateLayout,
    Ratio, RequestId, SplitDirection, UnwatchLayout, WatchLayout, WatchTerminals,
};
use combined::adapters::fs::file_store::FsFileStore;
use combined::core::{encode_layout, parse_layout, LayoutTiming, LAYOUT_FILE};
use combined::model::{SpaceId, SpaceLayout, SplitDir, SplitNode, Tile, TileId};
use combined::testing::MemoryFileStore;
use daemon_support::{
    layout_server, refuse_default_herdr_core, seed_project_terminal, wait_until, LayoutServer,
    ShortTempDir, WireClient,
};
use serde_json::{json, Map};

const WAIT: Duration = Duration::from_secs(10);
/// How long a test waits to be sure nothing more arrives.
const QUIET: Duration = Duration::from_millis(300);

/// Writes only when told to, so a test controls when the write happens.
const HOUR: LayoutTiming = LayoutTiming {
    debounce_ms: 3_600_000,
    max_wait_ms: 3_600_000,
    retry_ms: 3_600_000,
};
/// Writes soon after a mutation.
const PROMPT: LayoutTiming = LayoutTiming {
    debounce_ms: 20,
    max_wait_ms: 100,
    retry_ms: 3_600_000,
};

struct Env {
    _scratch: ShortTempDir,
    server: LayoutServer,
    store: Arc<MemoryFileStore>,
    space: SpaceId,
}

impl Env {
    fn new(timing: LayoutTiming) -> Env {
        let scratch = ShortTempDir::new();
        let store = Arc::new(MemoryFileStore::new());
        let server = layout_server(&scratch, "d", store.clone(), timing, |_, backend| {
            seed_project_terminal(backend, &scratch)
        });
        refuse_default_herdr_core(&server.socket, &scratch);
        let space = SpaceId(server.work.to_str().unwrap().to_string());
        Env {
            _scratch: scratch,
            server,
            store,
            space,
        }
    }

    fn name(&self) -> String {
        self.space.0.clone()
    }

    fn client(&self) -> WireClient {
        WireClient::open(&self.server.socket).0
    }

    fn watch(&self, client: &mut WireClient, request: u64) -> LayoutSnapshot {
        watch_space(client, &self.name(), request)
    }

    fn mutate(
        &self,
        client: &mut WireClient,
        request: u64,
        base: u64,
        ops: Vec<LayoutOp>,
    ) -> LayoutSnapshot {
        send_mutate(client, &self.name(), request, base, ops);
        expect_layout(client, Some(request))
    }

    fn stored(&self) -> SpaceLayout {
        let bytes = self.store.bytes(&self.space, LAYOUT_FILE).expect("a file");
        parse_layout(&serde_json::from_slice(&bytes).unwrap()).unwrap()
    }

    fn writes(&self) -> u64 {
        self.store.write_count(&self.space, LAYOUT_FILE)
    }

    fn reads(&self) -> u64 {
        self.store.read_count(&self.space, LAYOUT_FILE)
    }

    fn wait_writes(&self, count: u64) {
        assert!(
            wait_until(WAIT, || self.writes() == count),
            "write_count is {}, expected {count}",
            self.writes()
        );
    }
}

fn watch_space(client: &mut WireClient, space: &str, request: u64) -> LayoutSnapshot {
    client.send(&ClientMessage::WatchLayout(WatchLayout {
        request: RequestId(request),
        space: space.to_string(),
    }));
    expect_layout(client, Some(request))
}

fn send_mutate(client: &mut WireClient, space: &str, request: u64, base: u64, ops: Vec<LayoutOp>) {
    client.send(&ClientMessage::MutateLayout(MutateLayout {
        request: RequestId(request),
        space: space.to_string(),
        base_layout_revision: base,
        ops,
    }));
}

/// The next message, which must be a `layout` answering `request` (`None` for a push).
fn expect_layout(client: &WireClient, request: Option<u64>) -> LayoutSnapshot {
    match client.recv() {
        DaemonMessage::Layout(layout) => {
            assert_eq!(layout.request, request.map(RequestId));
            layout.layout
        }
        other => panic!("expected a layout, got {other:?}"),
    }
}

fn expect_error(client: &WireClient, code: ErrorCode, request: u64) -> String {
    match client.recv() {
        DaemonMessage::Error(error) => {
            assert_eq!(error.code, code, "{}", error.message);
            assert_eq!(error.request, Some(RequestId(request)));
            error.message
        }
        other => panic!("expected an error, got {other:?}"),
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

fn set_ratio(ratio: f64) -> LayoutOp {
    LayoutOp::SetRatio {
        leaves: vec!["main".to_string(), "side".to_string()],
        ratio: Ratio::new(ratio).unwrap(),
    }
}

fn model_tile(id: &str) -> SplitNode {
    SplitNode::Tile(Tile {
        id: TileId::new(id).unwrap(),
        panels: Vec::new(),
        active: None,
        extra: Map::new(),
    })
}

/// The bytes of a two-tile layout with the given ratio, as someone else would write them.
fn outside_layout(ratio: f64) -> Vec<u8> {
    let layout = SpaceLayout {
        version: 1,
        root: SplitNode::Split(combined::model::Split {
            dir: SplitDir::H,
            ratio,
            a: Box::new(model_tile("main")),
            b: Box::new(model_tile("side")),
            extra: Map::new(),
        }),
        groups: json!([]),
        extra: Map::new(),
    };
    encode_layout(&layout).unwrap()
}

fn root_ratio(root: &LayoutNode) -> f64 {
    match root {
        LayoutNode::Split { ratio, .. } => ratio.get(),
        LayoutNode::Tile { .. } => panic!("the root is a tile"),
    }
}

fn stored_ratio(layout: &SpaceLayout) -> f64 {
    match &layout.root {
        SplitNode::Split(split) => split.ratio,
        SplitNode::Tile(_) => panic!("the root is a tile"),
    }
}

#[test]
fn shutdown_flushes_pending_layout() {
    let mut env = Env::new(HOUR);
    let mut client = env.client();
    assert_eq!(env.watch(&mut client, 1).layout_revision, 0);
    assert_eq!(
        env.mutate(&mut client, 2, 0, split_main()).layout_revision,
        1
    );
    assert_eq!(env.writes(), 0);

    assert!(env.server.server.shutdown().is_none());

    assert_eq!(env.writes(), 1);
    assert_eq!(stored_ratio(&env.stored()), 0.5);
}

#[test]
fn own_write_causes_no_reload_end_to_end() {
    let env = Env::new(PROMPT);
    let mut client = env.client();
    env.watch(&mut client, 1);
    let reads = env.reads();
    let revision = env.mutate(&mut client, 2, 0, split_main()).layout_revision;
    assert_eq!(revision, 1);

    env.wait_writes(1);
    // The write is preceded by one comparing read, and followed by the echo's read.
    assert!(
        wait_until(WAIT, || env.reads() == reads + 2),
        "the echo of the write was never re-read"
    );
    assert!(
        client.recv_within(QUIET).is_none(),
        "the write caused a push"
    );
    assert_eq!(env.reads(), reads + 2);

    env.store
        .write_outside(&env.space, LAYOUT_FILE, outside_layout(0.3));
    let push = expect_layout(&client, None);
    assert_eq!(push.layout_revision, revision + 1);
    assert_eq!(root_ratio(&push.root), 0.3);
}

#[test]
fn outside_edit_pushes_new_layout() {
    let env = Env::new(HOUR);
    let mut client = env.client();
    assert_eq!(env.watch(&mut client, 1).layout_revision, 0);

    env.store
        .write_outside(&env.space, LAYOUT_FILE, outside_layout(0.25));

    let push = expect_layout(&client, None);
    assert_eq!(push.layout_revision, 1);
    assert_eq!(root_ratio(&push.root), 0.25);
    assert_eq!(push.error, None);
}

#[test]
fn invalid_file_bytes_unchanged_after_mutation() {
    let mut env = Env::new(PROMPT);
    let invalid = b"{ not json".to_vec();
    env.store
        .write_outside(&env.space, LAYOUT_FILE, invalid.clone());
    let mut client = env.client();
    let snapshot = env.watch(&mut client, 1);
    assert!(snapshot.error.is_some());

    env.mutate(&mut client, 2, 0, split_main());
    assert_eq!(
        env.store.bytes(&env.space, LAYOUT_FILE),
        Some(invalid.clone())
    );

    client.send(&ClientMessage::UnwatchLayout(UnwatchLayout {}));
    // A reply to a later request proves the unwatch was handled, including its flush.
    env.watch(&mut client, 3);
    assert_eq!(
        env.store.bytes(&env.space, LAYOUT_FILE),
        Some(invalid.clone())
    );

    assert!(env.server.server.shutdown().is_none());
    assert_eq!(env.store.bytes(&env.space, LAYOUT_FILE), Some(invalid));
    assert_eq!(env.writes(), 0);
}

fn files_named(dir: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_named(&path, name));
        } else if path.file_name().is_some_and(|file| file == name) {
            found.push(path);
        }
    }
    found
}

/// Gives the permissions back so the scratch dir can be removed.
struct Restore(PathBuf);

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn unwritable_workspace_marks_read_only_and_writes_daemon_data() {
    let scratch = ShortTempDir::new();
    let fallback = scratch.path().join("spaces");
    let work = daemon_support::work_dir(&scratch);
    fs::create_dir_all(work.join(".git")).unwrap();
    refuse_default_herdr_core(&fallback, &scratch);
    let _restore = Restore(work.clone());
    fs::set_permissions(&work, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(work.join("probe"), b"x").is_ok() {
        eprintln!("skipped: the project is writable despite chmod 0o555 (running as root?)");
        return;
    }

    let server = layout_server(
        &scratch,
        "d",
        Arc::new(FsFileStore::with_fallback(&fallback)),
        PROMPT,
        |_, backend| seed_project_terminal(backend, &scratch),
    );
    refuse_default_herdr_core(&server.socket, &scratch);
    let space = server.work.to_str().unwrap().to_string();
    let mut client = WireClient::open(&server.socket).0;
    assert!(!watch_space(&mut client, &space, 1).read_only);

    send_mutate(&mut client, &space, 2, 0, split_main());
    assert_eq!(expect_layout(&client, Some(2)).layout_revision, 1);
    let push = expect_layout(&client, None);
    assert!(push.read_only);
    assert_eq!(push.layout_revision, 2);

    assert_eq!(files_named(&fallback, LAYOUT_FILE).len(), 1);
    assert!(!work.join(".workspace").exists());
}

#[test]
fn daemon_restart_restores_identical_layout() {
    let scratch = ShortTempDir::new();
    let fallback = scratch.path().join("spaces");
    let store = || -> Arc<FsFileStore> { Arc::new(FsFileStore::with_fallback(&fallback)) };
    refuse_default_herdr_core(&fallback, &scratch);
    let seed = |_: &_, backend: &_| seed_project_terminal(backend, &scratch);

    let mut first = layout_server(&scratch, "a", store(), HOUR, seed);
    refuse_default_herdr_core(&first.socket, &scratch);
    let space = first.work.to_str().unwrap().to_string();
    let mut client = WireClient::open(&first.socket).0;
    watch_space(&mut client, &space, 1);
    send_mutate(&mut client, &space, 2, 0, split_main());
    expect_layout(&client, Some(2));
    send_mutate(&mut client, &space, 3, 1, vec![set_ratio(0.3)]);
    let before = expect_layout(&client, Some(3));
    assert_eq!(root_ratio(&before.root), 0.3);
    assert!(first.server.shutdown().is_none());
    drop(client);

    let second = layout_server(&scratch, "b", store(), HOUR, seed);
    let mut client = WireClient::open(&second.socket).0;
    let after = watch_space(&mut client, &space, 1);
    assert_eq!(after.root, before.root);
    assert_eq!(after.error, None);
}

#[test]
fn stale_layout_error_end_to_end() {
    let env = Env::new(HOUR);
    let mut client = env.client();
    env.watch(&mut client, 1);
    env.mutate(&mut client, 2, 0, split_main());

    send_mutate(&mut client, &env.name(), 3, 0, vec![set_ratio(0.4)]);
    expect_error(&client, ErrorCode::StaleLayout, 3);
}

#[test]
fn unknown_space_error_end_to_end() {
    let env = Env::new(HOUR);
    let mut client = env.client();
    let other = "/no/such/space";
    client.send(&ClientMessage::WatchLayout(WatchLayout {
        request: RequestId(1),
        space: other.to_string(),
    }));
    expect_error(&client, ErrorCode::UnknownSpace, 1);

    send_mutate(&mut client, other, 2, 0, split_main());
    expect_error(&client, ErrorCode::UnknownSpace, 2);
}

#[test]
fn not_watched_mutate_is_invalid_request() {
    let env = Env::new(HOUR);
    let mut watcher = env.client();
    env.watch(&mut watcher, 1);
    let mut other = env.client();

    send_mutate(&mut other, &env.name(), 2, 0, split_main());
    expect_error(&other, ErrorCode::InvalidRequest, 2);
    assert!(watcher.recv_within(QUIET).is_none());
}

#[test]
fn push_goes_only_to_watching_connection() {
    let env = Env::new(HOUR);
    let mut writer = env.client();
    let mut watcher = env.client();
    let idle = env.client();
    env.watch(&mut writer, 1);
    env.watch(&mut watcher, 1);

    env.mutate(&mut writer, 2, 0, split_main());

    let push = expect_layout(&watcher, None);
    assert_eq!(push.layout_revision, 1);
    assert!(idle.recv_within(QUIET).is_none());
    assert!(
        writer.recv_within(QUIET).is_none(),
        "a reply is not followed by a push"
    );
}

#[test]
fn unwatch_layout_gets_no_reply_and_flushes() {
    let env = Env::new(HOUR);
    let mut client = env.client();
    env.watch(&mut client, 1);
    env.mutate(&mut client, 2, 0, split_main());
    assert_eq!(env.writes(), 0);

    client.send(&ClientMessage::UnwatchLayout(UnwatchLayout {}));

    env.wait_writes(1);
    assert!(client.recv_within(QUIET).is_none());
}

#[test]
fn closing_connection_gets_no_layout_push_and_is_unwatched() {
    let env = Env::new(HOUR);
    let mut closing = env.client();
    env.watch(&mut closing, 1);
    env.mutate(&mut closing, 2, 0, split_main());
    assert_eq!(env.writes(), 0);

    let mut garbage = 9u32.to_be_bytes().to_vec();
    garbage.push(1);
    garbage.extend_from_slice(b"not json!");
    closing.send_bytes(&garbage);
    match closing.recv() {
        DaemonMessage::Error(error) => assert_eq!(error.code, ErrorCode::Malformed),
        other => panic!("expected the malformed error, got {other:?}"),
    }
    env.wait_writes(1);

    let mut watcher = env.client();
    assert_eq!(env.watch(&mut watcher, 1).layout_revision, 1);
    env.mutate(&mut watcher, 2, 1, vec![set_ratio(0.4)]);
    closing.expect_closed();
}

#[test]
fn zero_ops_is_invalid_request_end_to_end() {
    let env = Env::new(HOUR);
    let mut client = env.client();
    env.watch(&mut client, 1);

    send_mutate(&mut client, &env.name(), 2, 0, Vec::new());
    expect_error(&client, ErrorCode::InvalidRequest, 2);

    // Nothing changed: the same base revision is still current.
    assert_eq!(
        env.mutate(&mut client, 3, 0, split_main()).layout_revision,
        1
    );
}

#[test]
fn thirty_three_ops_is_invalid_request_end_to_end() {
    let env = Env::new(HOUR);
    let mut client = env.client();
    env.watch(&mut client, 1);
    env.mutate(&mut client, 2, 0, split_main());

    // Each op is valid alone: only the batch size is wrong.
    let ops = vec![set_ratio(0.5); 33];
    send_mutate(&mut client, &env.name(), 3, 1, ops);
    expect_error(&client, ErrorCode::InvalidRequest, 3);

    // Nothing changed: the same base revision is still current.
    assert_eq!(
        env.mutate(&mut client, 4, 1, vec![set_ratio(0.4)])
            .layout_revision,
        2
    );
}

#[test]
fn client_that_never_reads_layout_replies_is_disconnected() {
    let env = Env::new(HOUR);
    let mut client = env.client();
    env.watch(&mut client, 1);
    let stats = env.server.server.stats();
    assert_eq!(stats.connections(), 1);

    let mut frame = Vec::new();
    combine_protocol::write_message(
        &mut frame,
        &ClientMessage::MutateLayout(MutateLayout {
            request: RequestId(2),
            space: env.name(),
            base_layout_revision: 0,
            ops: Vec::new(),
        }),
    )
    .unwrap();
    let burst = frame.repeat(1_000);
    // Each frame is answered with an error the client never reads. A write failing means the
    // daemon has already closed the connection.
    for _ in 0..200 {
        if client.try_send_bytes(&burst).is_err() {
            break;
        }
    }
    assert!(
        wait_until(WAIT, || stats.connections() == 0),
        "the daemon kept a client that never reads its layout replies"
    );
}

/// A FIFO at `layout.json` is reported as a read error, not opened for a blocking read: the
/// watch answers at once and another client is served meanwhile.
#[test]
fn fifo_layout_file_is_refused_without_blocking_the_daemon() {
    let scratch = ShortTempDir::new();
    let fallback = scratch.path().join("spaces");
    refuse_default_herdr_core(&fallback, &scratch);
    let fifo = daemon_support::work_dir(&scratch)
        .join(".workspace")
        .join(LAYOUT_FILE);
    daemon_support::make_fifo(&fifo);
    let server = layout_server(
        &scratch,
        "d",
        Arc::new(FsFileStore::with_fallback(&fallback)),
        HOUR,
        |_, backend| seed_project_terminal(backend, &scratch),
    );
    refuse_default_herdr_core(&server.socket, &scratch);
    let space = server.work.to_str().unwrap().to_string();

    let mut watcher = WireClient::open(&server.socket).0;
    let mut other = WireClient::open(&server.socket).0;
    watcher.send(&ClientMessage::WatchLayout(WatchLayout {
        request: RequestId(1),
        space,
    }));
    let watched = watcher.recv_within(WAIT);
    other.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(2),
    }));
    let listed = other.recv_within(WAIT);
    // Frees a daemon stuck on the FIFO, so a regression fails below instead of hanging.
    daemon_support::release_fifo(&fifo, b"{}");

    let Some(DaemonMessage::Layout(layout)) = watched else {
        panic!("no layout reply while a FIFO sat at layout.json: {watched:?}");
    };
    assert_eq!(layout.request, Some(RequestId(1)));
    let error = layout.layout.error.unwrap_or_default();
    assert!(
        error.contains(".workspace/layout.json is not a regular file"),
        "{error}"
    );
    assert!(
        matches!(listed, Some(DaemonMessage::Terminals(_))),
        "the other client got no terminal list: {listed:?}"
    );
}
