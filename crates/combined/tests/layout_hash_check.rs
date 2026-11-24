#![cfg(feature = "test-support")]
//! #34 for `layout.json`: the server reads the file right before every write and writes only
//! when it is what the daemon last saw. An outside edit before that read is kept; one after it
//! is overwritten, which is the window the daemon can't close.

mod common;
mod daemon_support;
mod panel_support;

use std::sync::Arc;
use std::time::Duration;

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
use panel_support::{settled_read_count, Race, RacingStore};

const WAIT: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(400);
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
    _scratch: ShortTempDir,
    server: LayoutServer,
    memory: Arc<MemoryFileStore>,
    racing: Arc<RacingStore>,
    space: SpaceId,
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
    match client.recv() {
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
        client.recv_within(QUIET).is_none(),
        "the lost outside edit caused a push"
    );
}
