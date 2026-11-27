#![cfg(feature = "test-support")]
//! #34 for `layout.json`: the server reads the file right before every write and writes only
//! when it is what the daemon last saw. An outside edit before that read is kept; one after it
//! is overwritten, which is the window the daemon can't close.

mod common;
mod daemon_support;
mod panel_support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use combine_protocol::{
    ClientMessage, DaemonMessage, LayoutNode, LayoutOp, LayoutSnapshot, MutateLayout, Ratio,
    RequestId, SplitDirection, WatchLayout,
};
use combined::core::{parse_layout, LayoutTiming, LAYOUT_FILE};
use combined::model::{SpaceId, SplitNode};
use combined::testing::MemoryFileStore;
use daemon_support::{
    layout_server, refuse_default_herdr_core, seed_project_terminal, wait_until, LayoutServer,
    ShortTempDir, WireClient,
};
use panel_support::{markdown, named, settled_read_count, Race, RacingStore};

const WAIT: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(400);

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
fn recv_within_skipping_panels(client: &WireClient, wait: Duration) -> Option<DaemonMessage> {
    let deadline = Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match client.recv_within(left)? {
            DaemonMessage::Panels(_) => {}
            other => return Some(other),
        }
    }
}
const PROMPT: LayoutTiming = LayoutTiming {
    debounce_ms: 20,
    max_wait_ms: 100,
    retry_ms: 3_600_000,
};
/// What someone else writes: the same two tiles, with another ratio.
const OUTSIDE: &str = r#"{"version":1,"root":{"type":"split","dir":"h","ratio":0.3,
    "a":{"type":"tile","id":"main","panels":[],"active":null},
    "b":{"type":"tile","id":"side","panels":[],"active":null}},"groups":[]}"#;

struct Env {
    server: LayoutServer,
    memory: Arc<MemoryFileStore>,
    racing: Arc<RacingStore>,
    space: SpaceId,
    /// Last: fields drop in order, so the server stops (and flushes) before its folder goes.
    _scratch: ShortTempDir,
}

impl Env {
    /// A server whose layout file holds the split `main` | `side` at ratio 0.5, written by the
    /// daemon, with the client watching it.
    fn new() -> (Env, WireClient) {
        let scratch = ShortTempDir::new();
        let memory = Arc::new(MemoryFileStore::new());
        let racing = Arc::new(RacingStore::new(Arc::clone(&memory)));
        let server = layout_server(&scratch, "d", racing.clone(), PROMPT, |_, backend| {
            seed_project_terminal(backend, &scratch)
        });
        refuse_default_herdr_core(&server.socket, &scratch);
        let space = SpaceId(server.work.to_str().unwrap().to_string());
        let env = Env {
            _scratch: scratch,
            server,
            memory,
            racing,
            space,
        };
        let mut client = WireClient::open(&env.server.socket).0;
        env.watch(&mut client, 1);
        let split = vec![LayoutOp::SplitTile {
            tile: "main".to_string(),
            dir: SplitDirection::H,
            new_tile: "side".to_string(),
            ratio: Ratio::new(0.5).unwrap(),
            panel: None,
        }];
        assert_eq!(env.mutate(&mut client, 2, 0, split).layout_revision, 1);
        assert!(wait_until(WAIT, || env.writes() == 1));
        settled_read_count(&env.memory, &env.space, LAYOUT_FILE, 1);
        (env, client)
    }

    fn watch(&self, client: &mut WireClient, request: u64) {
        client.send(&ClientMessage::WatchLayout(WatchLayout {
            request: RequestId(request),
            space: self.space.0.clone(),
        }));
        expect_layout(client, Some(request));
    }

    fn mutate(
        &self,
        client: &mut WireClient,
        request: u64,
        base: u64,
        ops: Vec<LayoutOp>,
    ) -> LayoutSnapshot {
        client.send(&ClientMessage::MutateLayout(MutateLayout {
            request: RequestId(request),
            space: self.space.0.clone(),
            base_layout_revision: base,
            ops,
        }));
        expect_layout(client, Some(request))
    }

    fn writes(&self) -> u64 {
        self.memory.write_count(&self.space, LAYOUT_FILE)
    }

    fn stored_ratio(&self) -> f64 {
        let bytes = self.memory.bytes(&self.space, LAYOUT_FILE).unwrap();
        match parse_layout(&serde_json::from_slice(&bytes).unwrap())
            .unwrap()
            .root
        {
            SplitNode::Split(split) => split.ratio,
            SplitNode::Tile(_) => panic!("the root is a tile"),
        }
    }
}

fn expect_layout(client: &WireClient, request: Option<u64>) -> LayoutSnapshot {
    match recv_skipping_panels(client) {
        DaemonMessage::Layout(layout) => {
            assert_eq!(layout.request, request.map(RequestId));
            layout.layout
        }
        other => panic!("expected a layout, got {other:?}"),
    }
}

fn ratio_of(root: &LayoutNode) -> f64 {
    match root {
        LayoutNode::Split { ratio, .. } => ratio.get(),
        LayoutNode::Tile { .. } => panic!("the root is a tile"),
    }
}

fn set_ratio(ratio: f64) -> Vec<LayoutOp> {
    vec![LayoutOp::SetRatio {
        leaves: vec!["main".to_string(), "side".to_string()],
        ratio: Ratio::new(ratio).unwrap(),
    }]
}

#[test]
fn an_outside_edit_before_the_comparing_read_is_not_overwritten() {
    let (env, mut client) = Env::new();
    env.racing.arm(
        &env.space,
        LAYOUT_FILE,
        Race::BeforeCompare,
        OUTSIDE.as_bytes().to_vec(),
    );
    env.mutate(&mut client, 3, 1, set_ratio(0.7));

    assert!(
        wait_until(WAIT, || !env.racing.armed()),
        "the write never compared the file"
    );
    let push = loop {
        match client.recv_within(WAIT) {
            Some(DaemonMessage::Layout(push)) if push.request.is_none() => {
                if ratio_of(&push.layout.root) == 0.3 {
                    break push;
                }
            }
            Some(_) => {}
            None => panic!("the outside edit never reached the client"),
        }
    };
    assert_eq!(ratio_of(&push.layout.root), 0.3);
    std::thread::sleep(QUIET);
    assert_eq!(env.writes(), 1);
    assert_eq!(
        env.memory.bytes(&env.space, LAYOUT_FILE).unwrap(),
        OUTSIDE.as_bytes()
    );
}

#[test]
fn the_window_after_the_comparing_read_overwrites() {
    let (env, mut client) = Env::new();
    env.racing.arm(
        &env.space,
        LAYOUT_FILE,
        Race::AfterCompare,
        OUTSIDE.as_bytes().to_vec(),
    );
    env.mutate(&mut client, 3, 1, set_ratio(0.7));

    assert!(wait_until(WAIT, || env.writes() == 2), "no second write");
    assert!(!env.racing.armed());
    assert_eq!(env.stored_ratio(), 0.7);
    assert_ne!(
        env.memory.bytes(&env.space, LAYOUT_FILE).unwrap(),
        OUTSIDE.as_bytes()
    );
    assert!(
        recv_within_skipping_panels(&client, QUIET).is_none(),
        "the lost outside edit caused a push"
    );
}

/// The tiles of a stored layout: each tile's id and panels.
fn tiles_of(bytes: &[u8]) -> Vec<(String, Vec<String>)> {
    fn walk(node: &serde_json::Value, into: &mut Vec<(String, Vec<String>)>) {
        if node["type"] == "tile" {
            let panels = node["panels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|panel| panel.as_str().unwrap().to_string())
                .collect();
            into.push((node["id"].as_str().unwrap().to_string(), panels));
        } else {
            walk(&node["a"], into);
            walk(&node["b"], into);
        }
    }
    let layout: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    let mut tiles = Vec::new();
    walk(&layout["root"], &mut tiles);
    tiles
}

#[test]
fn a_placement_write_finds_an_outside_edit_and_does_not_overwrite_it() {
    let scratch = ShortTempDir::new();
    let memory = Arc::new(MemoryFileStore::new());
    let racing = Arc::new(RacingStore::new(Arc::clone(&memory)));
    let server = layout_server(&scratch, "d", racing.clone(), PROMPT, |_, backend| {
        seed_project_terminal(backend, &scratch)
    });
    refuse_default_herdr_core(&server.socket, &scratch);
    let space = SpaceId(server.work.to_str().unwrap().to_string());
    racing.arm(
        &space,
        LAYOUT_FILE,
        Race::BeforeCompare,
        OUTSIDE.as_bytes().to_vec(),
    );

    server
        .server
        .panels()
        .create(&space, named("p", markdown("x")))
        .unwrap();

    assert!(
        wait_until(WAIT, || !racing.armed()),
        "the placement write never compared the file"
    );
    // The outside edit is kept, and the placement is applied to it and written once.
    assert!(
        wait_until(WAIT, || memory.write_count(&space, LAYOUT_FILE) == 1),
        "the placement was never written over the outside edit's layout"
    );
    std::thread::sleep(QUIET);
    assert_eq!(memory.write_count(&space, LAYOUT_FILE), 1);
    let stored = memory.bytes(&space, LAYOUT_FILE).unwrap();
    assert_eq!(
        tiles_of(&stored),
        vec![
            ("main".to_string(), vec!["p".to_string()]),
            ("side".to_string(), Vec::new())
        ]
    );
    let stored: serde_json::Value = serde_json::from_slice(&stored).unwrap();
    assert_eq!(stored["root"]["ratio"], 0.3);
}
