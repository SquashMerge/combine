#![cfg(feature = "test-support")]
//! The daemon places the panels the MCP tools create: a tab in a tile of the space's
//! `layout.json`, with no client watching. Over the running server with in-memory stores, except
//! the unwritable project, which needs real folders.

mod common;
mod daemon_support;
mod mcp_support;
mod panel_support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use combine_protocol::PanelState;
use combined::adapters::fs::file_store::FsFileStore;
use combined::core::{LayoutTiming, PanelTiming, LAYOUT_FILE, SCRATCH_TTL_MS};
use combined::ports::FileStore;
use daemon_support::{wait_until, ShortTempDir};
use mcp_support::{cwd_caller, wire_options, McpWire};
use panel_support::{watch_panels, PanelRig, PanelServerConfig, TEST_TIMING};
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(10);
/// How long a test waits to be sure nothing more happens.
const QUIET: Duration = Duration::from_millis(400);
const MERMAID: &str = "graph TD; A-->B";

/// Writes soon after a placement.
const PROMPT: LayoutTiming = LayoutTiming {
    debounce_ms: 20,
    max_wait_ms: 100,
    retry_ms: 3_600_000,
};
/// Writes only at shutdown.
const HOUR: LayoutTiming = LayoutTiming {
    debounce_ms: 3_600_000,
    max_wait_ms: 3_600_000,
    retry_ms: 3_600_000,
};
/// Two tiles, `main` and `side`, someone else wrote.
const TWO_TILES: &str = r#"{"version":1,"root":{"type":"split","dir":"h","ratio":0.5,
    "a":{"type":"tile","id":"main","panels":[],"active":null},
    "b":{"type":"tile","id":"side","panels":[],"active":null}},"groups":[]}"#;

/// One tile of a stored layout: its id, its panels and its active panel.
type Tile = (String, Vec<String>, Option<String>);

fn config(timing: LayoutTiming) -> PanelServerConfig {
    PanelServerConfig {
        layout_timing: timing,
        options: wire_options(),
        ..PanelServerConfig::default()
    }
}

/// A rig with a terminal in the project folder and an open channel on its space.
fn rig_and_wire(timing: LayoutTiming) -> (PanelRig, McpWire) {
    wire_for(PanelRig::with_config(config(timing)))
}

fn wire_for(rig: PanelRig) -> (PanelRig, McpWire) {
    rig.spawn_terminal();
    let mut wire = McpWire::open(&rig, cwd_caller(&rig));
    wire.wait_loaded();
    (rig, wire)
}

fn markdown_args(id: &str) -> Value {
    json!({"id": id, "renderer": "markdown", "content": "x"})
}

fn collect(node: &Value, into: &mut Vec<Tile>) {
    if node["type"] == "tile" {
        let panels = node["panels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|panel| panel.as_str().unwrap().to_string())
            .collect();
        into.push((
            node["id"].as_str().unwrap().to_string(),
            panels,
            node["active"].as_str().map(str::to_string),
        ));
    } else {
        collect(&node["a"], into);
        collect(&node["b"], into);
    }
}

fn tiles_of(bytes: &[u8]) -> Vec<Tile> {
    let layout: Value = serde_json::from_slice(bytes).unwrap();
    let mut tiles = Vec::new();
    collect(&layout["root"], &mut tiles);
    tiles
}

/// The tiles of the stored layout, `None` when there is no file.
fn stored(rig: &PanelRig) -> Option<Vec<Tile>> {
    rig.bytes(LAYOUT_FILE).map(|bytes| tiles_of(&bytes))
}

fn tile(id: &str, panels: &[&str], active: Option<&str>) -> Tile {
    (
        id.to_string(),
        panels.iter().map(|panel| panel.to_string()).collect(),
        active.map(str::to_string),
    )
}

/// Waits until the stored layout is `expected`.
fn wait_stored(rig: &PanelRig, expected: &[Tile]) {
    assert!(
        wait_until(WAIT, || stored(rig).as_deref() == Some(expected)),
        "the layout never became {expected:?}; it is {:?}",
        stored(rig)
    );
}

fn wait_read(rig: &PanelRig) {
    assert!(
        wait_until(WAIT, || rig.store.read_count(&rig.space, LAYOUT_FILE) >= 1),
        "the layout was never read"
    );
}

#[test]
fn show_diagram_with_no_client_writes_layout_json() {
    let (rig, mut wire) = rig_and_wire(PROMPT);
    assert_eq!(rig.write_count(LAYOUT_FILE), 0);
    assert_eq!(rig.bytes(LAYOUT_FILE), None);

    let shown = wire.tool("show_diagram", json!({"id": "d1", "source": MERMAID}));
    assert!(!shown.is_error, "{shown:?}");

    wait_stored(&rig, &[tile("main", &["d1"], Some("d1"))]);
    assert_eq!(rig.write_count(LAYOUT_FILE), 1);
}

#[test]
fn create_panel_places_persistent_and_scratch_panels() {
    let (rig, mut wire) = rig_and_wire(PROMPT);
    let persistent = wire.tool("create_panel", markdown_args("a"));
    assert!(!persistent.is_error, "{persistent:?}");
    let mut scratch = markdown_args("b");
    scratch["lifetime"] = json!("scratch");
    let scratch = wire.tool("create_panel", scratch);
    assert!(!scratch.is_error, "{scratch:?}");

    wait_stored(&rig, &[tile("main", &["a", "b"], Some("b"))]);
}

#[test]
fn placement_names_a_tile_and_the_tab_lands_there() {
    let (rig, mut wire) = rig_and_wire(PROMPT);
    rig.store
        .write_outside(&rig.space, LAYOUT_FILE, TWO_TILES.as_bytes().to_vec());

    let mut named = markdown_args("p");
    named["placement"] = json!("side");
    assert!(!wire.tool("create_panel", named).is_error);
    wait_stored(
        &rig,
        &[tile("main", &[], None), tile("side", &["p"], Some("p"))],
    );

    let mut unknown = markdown_args("q");
    unknown["placement"] = json!("nowhere");
    assert!(!wire.tool("create_panel", unknown).is_error);
    wait_stored(
        &rig,
        &[
            tile("main", &["q"], Some("q")),
            tile("side", &["p"], Some("p")),
        ],
    );
}

#[test]
fn a_late_client_sees_the_placed_tab() {
    let (rig, mut wire) = rig_and_wire(PROMPT);
    let shown = wire.tool("show_diagram", json!({"id": "d1", "source": MERMAID}));
    assert!(!shown.is_error, "{shown:?}");

    let mut client = rig.client();
    let (layout, panels) = watch_panels(&mut client, &rig.space, 1);
    let mut tiles = Vec::new();
    collect(
        &serde_json::to_value(&layout.layout.root).unwrap(),
        &mut tiles,
    );
    assert_eq!(tiles, vec![tile("main", &["d1"], Some("d1"))]);
    assert!(panels.panels.iter().any(|panel| panel.id == "d1"));
}

#[test]
fn shutdown_writes_a_pending_placement() {
    let (mut rig, mut wire) = rig_and_wire(HOUR);
    assert!(
        !wire
            .tool("show_diagram", json!({"id": "d1", "source": MERMAID}))
            .is_error
    );
    wait_read(&rig);
    assert_eq!(rig.write_count(LAYOUT_FILE), 0);

    assert!(rig.server.shutdown().is_none());

    assert_eq!(rig.write_count(LAYOUT_FILE), 1);
    assert_eq!(stored(&rig), Some(vec![tile("main", &["d1"], Some("d1"))]));
}

#[test]
fn shutdown_never_writes_a_placement_over_an_invalid_file() {
    let (mut rig, mut wire) = rig_and_wire(HOUR);
    let invalid = b"{ not json".to_vec();
    rig.store
        .write_outside(&rig.space, LAYOUT_FILE, invalid.clone());
    assert!(
        !wire
            .tool("show_diagram", json!({"id": "d1", "source": MERMAID}))
            .is_error
    );
    wait_read(&rig);

    assert!(rig.server.shutdown().is_none());

    assert_eq!(rig.bytes(LAYOUT_FILE), Some(invalid));
    assert_eq!(rig.write_count(LAYOUT_FILE), 0);
}

/// Gives the permissions back so the scratch dir can be removed.
struct Restore(PathBuf);

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
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

#[test]
fn a_placement_in_an_unwritable_project_writes_the_fallback() {
    let fallback_dir = ShortTempDir::new();
    let fallback = fallback_dir.path().join("spaces");
    let served: Arc<dyn FileStore> = Arc::new(FsFileStore::with_fallback(&fallback));
    let (rig, mut wire) = wire_for(PanelRig::with_config(PanelServerConfig {
        served: Some(served),
        ..config(PROMPT)
    }));
    let work = PathBuf::from(&rig.space.0);
    let _restore = Restore(work.clone());
    fs::set_permissions(&work, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(work.join("probe"), b"x").is_ok() {
        eprintln!("skipped: the project is writable despite chmod 0o555 (running as root?)");
        return;
    }

    let shown = wire.tool("show_diagram", json!({"id": "d1", "source": MERMAID}));
    assert!(!shown.is_error, "{shown:?}");

    assert!(
        wait_until(WAIT, || files_named(&fallback, LAYOUT_FILE).len() == 1),
        "no fallback layout was written"
    );
    let written = fs::read(&files_named(&fallback, LAYOUT_FILE)[0]).unwrap();
    assert_eq!(tiles_of(&written), vec![tile("main", &["d1"], Some("d1"))]);
    assert!(!work.join(".workspace").exists());
}

/// Takes the placed tab out the way someone editing the file would, and waits until the daemon
/// has read the edit: the layout then holds no panel, and no placement is waiting. Returns the
/// layout write count, which nothing may change from here on.
fn settle_without_the_tab(rig: &PanelRig) -> u64 {
    let reads = rig.store.read_count(&rig.space, LAYOUT_FILE);
    rig.store
        .write_outside(&rig.space, LAYOUT_FILE, TWO_TILES.as_bytes().to_vec());
    assert!(
        wait_until(WAIT, || rig.store.read_count(&rig.space, LAYOUT_FILE)
            > reads),
        "the daemon never read the edited layout"
    );
    std::thread::sleep(QUIET);
    rig.write_count(LAYOUT_FILE)
}

fn the_two_empty_tiles() -> Option<Vec<Tile>> {
    Some(vec![tile("main", &[], None), tile("side", &[], None)])
}

#[test]
fn a_repeated_key_adds_no_second_tab() {
    let (rig, mut wire) = rig_and_wire(PROMPT);
    let mut args = markdown_args("k1");
    args["idempotency_key"] = json!("K");
    assert!(!wire.tool("create_panel", args.clone()).is_error);
    wait_stored(&rig, &[tile("main", &["k1"], Some("k1"))]);
    let writes = settle_without_the_tab(&rig);

    let again = wire.tool("create_panel", args);
    assert_eq!(again.body["panel"], "k1");
    std::thread::sleep(QUIET);

    assert_eq!(rig.write_count(LAYOUT_FILE), writes);
    assert_eq!(stored(&rig), the_two_empty_tiles());
}

#[test]
fn an_update_does_not_place_again() {
    let (rig, mut wire) = rig_and_wire(PROMPT);
    assert!(!wire.tool("create_panel", markdown_args("u")).is_error);
    wait_stored(&rig, &[tile("main", &["u"], Some("u"))]);
    let writes = settle_without_the_tab(&rig);

    let updated = wire.tool("update_panel", json!({"panel": "u", "content": "two"}));
    assert!(!updated.is_error, "{updated:?}");
    rig.wait_state("u", PanelState::Validated);
    std::thread::sleep(QUIET);

    assert_eq!(rig.write_count(LAYOUT_FILE), writes);
    assert_eq!(stored(&rig), the_two_empty_tiles());
}

#[test]
fn a_panel_already_in_the_tree_is_not_added_again() {
    let (rig, mut wire) = rig_and_wire(PROMPT);
    let held = TWO_TILES.replace(
        r#""id":"side","panels":[],"active":null"#,
        r#""id":"side","panels":["p"],"active":"p""#,
    );
    rig.store
        .write_outside(&rig.space, LAYOUT_FILE, held.clone().into_bytes());

    assert!(!wire.tool("create_panel", markdown_args("p")).is_error);
    wait_read(&rig);
    std::thread::sleep(QUIET);

    assert_eq!(rig.write_count(LAYOUT_FILE), 0);
    assert_eq!(rig.bytes(LAYOUT_FILE), Some(held.into_bytes()));
}

#[test]
fn a_folder_made_outside_is_not_placed() {
    let (rig, _wire) = rig_and_wire(PROMPT);
    rig.store.write_outside(
        &rig.space,
        "panels/x/panel.json",
        br#"{"version":1,"renderer":"markdown"}"#.to_vec(),
    );
    rig.store
        .write_outside(&rig.space, "panels/x/content.md", b"hello".to_vec());
    rig.wait_state("x", PanelState::Validated);
    std::thread::sleep(QUIET);

    assert_eq!(rig.write_count(LAYOUT_FILE), 0);
    assert_eq!(rig.bytes(LAYOUT_FILE), None);
}

#[test]
fn a_restart_places_nothing_again() {
    let (mut rig, mut wire) = rig_and_wire(PROMPT);
    assert!(!wire.tool("create_panel", markdown_args("r")).is_error);
    wait_stored(&rig, &[tile("main", &["r"], Some("r"))]);
    let writes = settle_without_the_tab(&rig);
    let written = rig.bytes(LAYOUT_FILE);
    drop(wire);

    rig.restart();
    rig.wait_state("r", PanelState::Validated);
    std::thread::sleep(QUIET);

    assert_eq!(rig.write_count(LAYOUT_FILE), writes);
    assert_eq!(rig.bytes(LAYOUT_FILE), written);
    assert_eq!(stored(&rig), the_two_empty_tiles());
}

#[test]
fn a_handle_close_takes_the_id_out() {
    let (rig, mut wire) = rig_and_wire(PROMPT);
    assert!(!wire.tool("create_panel", markdown_args("c")).is_error);
    wait_stored(&rig, &[tile("main", &["c"], Some("c"))]);

    let closed = rig.panels.close(&rig.space, "c").unwrap();
    assert_eq!(closed.state, PanelState::Removed);

    wait_stored(&rig, &[tile("main", &[], None)]);
}

/// A close in a run that never held the layout queues its `Watch` and `Read` inside the panel
/// part of a settle pass, which must run them. The panel timers are an hour long here.
#[test]
fn a_close_with_no_held_layout_reads_and_rewrites_it() {
    let timing = PanelTiming {
        retry_ms: 3_600_000,
        sweep_every_ms: 3_600_000,
        ..TEST_TIMING
    };
    let (mut rig, mut wire) = wire_for(PanelRig::with_config(PanelServerConfig {
        timing,
        ..config(PROMPT)
    }));
    assert!(!wire.tool("create_panel", markdown_args("c")).is_error);
    wait_stored(&rig, &[tile("main", &["c"], Some("c"))]);
    drop(wire);
    rig.restart();
    rig.wait_state("c", PanelState::Validated);

    let closed = rig.panels.close(&rig.space, "c").unwrap();
    assert_eq!(closed.state, PanelState::Removed);

    wait_stored(&rig, &[tile("main", &[], None)]);
}

#[test]
fn an_expired_scratch_panel_leaves_the_layout() {
    let (rig, mut wire) = rig_and_wire(PROMPT);
    assert!(
        !wire
            .tool("show_diagram", json!({"id": "d1", "source": MERMAID}))
            .is_error
    );
    wait_stored(&rig, &[tile("main", &["d1"], Some("d1"))]);

    rig.wall.advance(SCRATCH_TTL_MS + 1);

    wait_stored(&rig, &[tile("main", &[], None)]);
}
