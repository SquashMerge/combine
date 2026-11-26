#![cfg(feature = "test-support")]
//! A client's `close_panel` and `pin_panel` over the wire: a close takes the panel's tab out of the
//! layout and its files stay when it is persistent; a pin keeps the tab and moves a scratch
//! panel into the project, or says why it didn't. Over the running server with in-memory stores.

mod common;
mod daemon_support;
mod mcp_support;
mod panel_support;

use std::time::Duration;

use combine_protocol::{
    ClientMessage, ClosePanel, DaemonMessage, ErrorCode, ErrorMessage, Layout, PanelLifetime,
    PanelState, PinPanel, RequestId, WatchTerminals,
};
use combined::core::{LayoutTiming, LAYOUT_FILE, PIN_UNDONE, SCRATCH_TTL_MS};
use combined::model::Lifetime;
use combined::rpc::ServeOptions;
use daemon_support::{wait_until, WireClient};
use mcp_support::{cwd_caller, wire_options, McpWire};
use panel_support::{
    markdown, mermaid, named, scratch, watch_panels, PanelRig, PanelServerConfig, StoreOp, WAIT,
};
use serde_json::{json, Value};

/// How long a test waits to be sure nothing more is sent.
const QUIET: Duration = Duration::from_millis(300);
const MERMAID: &str = "graph TD; A-->B";

/// Writes soon after a layout change.
const PROMPT: LayoutTiming = LayoutTiming {
    debounce_ms: 20,
    max_wait_ms: 100,
    retry_ms: 3_600_000,
};

fn config() -> PanelServerConfig {
    PanelServerConfig {
        layout_timing: PROMPT,
        options: wire_options(),
        ..PanelServerConfig::default()
    }
}

/// A rig with a terminal in the project folder and its panels read.
fn ready(rig: PanelRig) -> PanelRig {
    rig.spawn_terminal();
    assert!(
        wait_until(WAIT, || rig.panels.list(&rig.space).is_ok()),
        "the space's panels were never read"
    );
    rig
}

fn rig() -> PanelRig {
    ready(PanelRig::with_config(config()))
}

/// A rig that never holds a flush back. The default holds one back when it comes less than 16 ms
/// after the last, which can move a close's layout push after its panels push and hide a settle
/// pass that returned without running the layout work.
fn unthrottled_rig() -> PanelRig {
    ready(PanelRig::with_config(PanelServerConfig {
        options: ServeOptions {
            flush_interval: Duration::ZERO,
            ..wire_options()
        },
        ..config()
    }))
}

/// The panel IDs the tiles of `layout` name, in tree order.
fn names(layout: &Layout) -> Vec<String> {
    fn walk(node: &Value, into: &mut Vec<String>) {
        if node["type"] == "tile" {
            into.extend(
                node["panels"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|panel| panel.as_str().unwrap().to_string()),
            );
        } else {
            walk(&node["a"], into);
            walk(&node["b"], into);
        }
    }
    let mut found = Vec::new();
    walk(
        &serde_json::to_value(&layout.layout.root).unwrap(),
        &mut found,
    );
    found
}

fn active_of(layout: &Layout) -> Vec<String> {
    fn walk(node: &Value, into: &mut Vec<String>) {
        if node["type"] == "tile" {
            into.extend(node["active"].as_str().map(str::to_string));
        } else {
            walk(&node["a"], into);
            walk(&node["b"], into);
        }
    }
    let mut found = Vec::new();
    walk(
        &serde_json::to_value(&layout.layout.root).unwrap(),
        &mut found,
    );
    found
}

/// The layout as `layout.json` stores it: the IDs its tiles name.
fn stored_names(rig: &PanelRig) -> Option<Vec<String>> {
    let bytes = rig.bytes(LAYOUT_FILE)?;
    let layout: Value = serde_json::from_slice(&bytes).unwrap();
    let mut found = Vec::new();
    fn walk(node: &Value, into: &mut Vec<String>) {
        if node["type"] == "tile" {
            into.extend(
                node["panels"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|panel| panel.as_str().unwrap().to_string()),
            );
        } else {
            walk(&node["a"], into);
            walk(&node["b"], into);
        }
    }
    walk(&layout["root"], &mut found);
    Some(found)
}

fn wait_stored(rig: &PanelRig, expected: &[&str]) {
    let expected: Vec<String> = expected.iter().map(|id| id.to_string()).collect();
    assert!(
        wait_until(WAIT, || stored_names(rig).as_ref() == Some(&expected)),
        "the stored layout never named {expected:?}; it names {:?}",
        stored_names(rig)
    );
}

fn close(client: &mut WireClient, rig: &PanelRig, request: u64, panel: &str) {
    client.send(&ClientMessage::ClosePanel(ClosePanel {
        request: RequestId(request),
        space: rig.space.0.clone(),
        panel: panel.to_string(),
    }));
}

fn pin(client: &mut WireClient, rig: &PanelRig, request: u64, panel: &str) {
    client.send(&ClientMessage::PinPanel(PinPanel {
        request: RequestId(request),
        space: rig.space.0.clone(),
        panel: panel.to_string(),
    }));
}

fn answers(message: &DaemonMessage, request: u64) -> bool {
    match message {
        DaemonMessage::PanelClosed(closed) => closed.request == RequestId(request),
        DaemonMessage::PanelPinned(pinned) => pinned.request == RequestId(request),
        DaemonMessage::Error(error) => error.request == Some(RequestId(request)),
        _ => false,
    }
}

/// Every message up to and including the answer to `request`, then whatever follows within
/// [`QUIET`].
fn until_answered(client: &WireClient, request: u64) -> Vec<DaemonMessage> {
    let mut seen = Vec::new();
    loop {
        let message = client.recv();
        let done = answers(&message, request);
        seen.push(message);
        if done {
            break;
        }
    }
    while let Some(message) = client.recv_within(QUIET) {
        seen.push(message);
    }
    seen
}

fn answer_of(seen: &[DaemonMessage], request: u64) -> &DaemonMessage {
    seen.iter()
        .find(|message| answers(message, request))
        .expect("no answer")
}

fn refusal(seen: &[DaemonMessage], request: u64) -> &ErrorMessage {
    match answer_of(seen, request) {
        DaemonMessage::Error(error) => error,
        other => panic!("expected an error, got {other:?}"),
    }
}

fn kinds(seen: &[DaemonMessage]) -> Vec<&'static str> {
    seen.iter().map(DaemonMessage::type_name).collect()
}

/// A client watching the layout, with the pushes of the watch answered.
fn watcher(rig: &PanelRig) -> (WireClient, Layout) {
    let mut client = rig.client();
    let (layout, _) = watch_panels(&mut client, &rig.space, 1);
    (client, layout)
}

#[test]
fn pin_keeps_the_tab_and_the_panel_listed_throughout() {
    let rig = rig();
    rig.create(named("s", scratch(markdown("one")))).unwrap();
    wait_stored(&rig, &["s"]);
    let (mut client, before) = watcher(&rig);
    assert_eq!(names(&before), ["s"]);
    let writes = rig.write_count(LAYOUT_FILE);

    pin(&mut client, &rig, 2, "s");
    let seen = until_answered(&client, 2);

    assert!(
        matches!(answer_of(&seen, 2), DaemonMessage::PanelPinned(_)),
        "{seen:?}"
    );
    for message in &seen {
        match message {
            DaemonMessage::Layout(_) => panic!("a pin pushed a layout: {seen:?}"),
            DaemonMessage::Panels(panels) => {
                let listed = panels.panels.iter().find(|panel| panel.id == "s");
                assert!(
                    listed.is_some_and(|panel| panel.state != PanelState::Removed),
                    "the panel was missing from a push: {panels:?}"
                );
            }
            _ => {}
        }
    }
    let (after, panels) = watch_panels(&mut client, &rig.space, 3);
    assert_eq!(names(&after), ["s"]);
    let listed = panels.panels.iter().find(|panel| panel.id == "s").unwrap();
    assert_eq!(listed.lifetime, Some(PanelLifetime::Persistent));
    assert_eq!(rig.write_count(LAYOUT_FILE), writes);
    assert_eq!(stored_names(&rig), Some(vec!["s".to_string()]));
}

#[test]
fn closing_a_scratch_panel_deletes_it_and_its_layout_id() {
    let rig = rig();
    rig.create(named("s", scratch(markdown("one")))).unwrap();
    wait_stored(&rig, &["s"]);
    let (mut closer, _) = watcher(&rig);
    let (second, _) = watcher(&rig);

    close(&mut closer, &rig, 2, "s");
    let seen = until_answered(&closer, 2);

    assert!(
        matches!(answer_of(&seen, 2), DaemonMessage::PanelClosed(closed) if closed.panel == "s"),
        "{seen:?}"
    );
    let mut pushed = Vec::new();
    while let Some(message) = second.recv_within(QUIET) {
        pushed.push(message);
    }
    let layouts: Vec<&Layout> = pushed
        .iter()
        .filter_map(|message| match message {
            DaemonMessage::Layout(layout) => Some(layout),
            _ => None,
        })
        .collect();
    assert_eq!(layouts.len(), 1, "{:?}", kinds(&pushed));
    assert!(names(layouts[0]).is_empty());
    let panels = pushed.iter().rev().find_map(|message| match message {
        DaemonMessage::Panels(panels) => Some(panels),
        _ => None,
    });
    assert!(
        panels.is_some_and(|panels| panels.panels.iter().all(|panel| panel.id != "s")),
        "{pushed:?}"
    );
    assert!(rig.scratch.dirs().is_empty());
    wait_stored(&rig, &[]);
}

/// The layout push comes before the panels push, which comes before the reply, in one flush. A
/// settle pass that returned without running the layout work its close queued would send the
/// panels and the reply first and the layout after them.
#[test]
fn a_close_flushes_layout_then_panels_then_its_reply() {
    let rig = unthrottled_rig();
    rig.create(named("p", markdown("one"))).unwrap();
    wait_stored(&rig, &["p"]);
    let (mut client, _) = watcher(&rig);

    close(&mut client, &rig, 2, "p");
    let seen = until_answered(&client, 2);

    assert_eq!(
        kinds(&seen),
        ["layout", "panels", "panel_closed"],
        "{seen:?}"
    );
}

#[test]
fn one_close_makes_one_layout_push_and_settle_returns() {
    let rig = unthrottled_rig();
    rig.create(named("p", markdown("one"))).unwrap();
    wait_stored(&rig, &["p"]);
    let (mut client, before) = watcher(&rig);

    close(&mut client, &rig, 2, "p");
    let seen = until_answered(&client, 2);

    assert_eq!(
        kinds(&seen),
        ["layout", "panels", "panel_closed"],
        "{seen:?}"
    );
    match &seen[0] {
        DaemonMessage::Layout(layout) => {
            assert_eq!(
                layout.layout.layout_revision,
                before.layout.layout_revision + 1
            );
            assert!(names(layout).is_empty());
        }
        other => panic!("{other:?}"),
    }
    client.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(3),
    }));
    let answered = wait_until(Duration::from_secs(1), || {
        matches!(
            client.recv_within(Duration::from_millis(50)),
            Some(DaemonMessage::Terminals(_))
        )
    });
    assert!(answered, "a later request was never answered");
}

#[test]
fn closing_a_persistent_panel_removes_its_tab_and_keeps_its_files() {
    let rig = rig();
    rig.create(named("p", markdown("one"))).unwrap();
    wait_stored(&rig, &["p"]);
    let (mut client, _) = watcher(&rig);

    close(&mut client, &rig, 2, "p");
    let seen = until_answered(&client, 2);

    assert!(
        matches!(answer_of(&seen, 2), DaemonMessage::PanelClosed(_)),
        "{seen:?}"
    );
    wait_stored(&rig, &[]);
    assert_eq!(rig.bytes("panels/p/content.md").unwrap(), b"one");
    assert!(rig.bytes("panels/p/panel.json").is_some());
    assert_eq!(rig.remove_count("panels/p/content.md"), 0);
    assert_eq!(rig.remove_count("panels/p/panel.json"), 0);
}

#[test]
fn after_a_restart_a_closed_persistent_panel_is_listed_and_not_placed() {
    let mut rig = rig();
    rig.create(named("p", markdown("one"))).unwrap();
    wait_stored(&rig, &["p"]);
    {
        let (mut client, _) = watcher(&rig);
        close(&mut client, &rig, 2, "p");
        until_answered(&client, 2);
    }
    wait_stored(&rig, &[]);
    let writes = rig.write_count(LAYOUT_FILE);

    rig.restart();
    rig.wait_state("p", PanelState::Validated);
    std::thread::sleep(QUIET);

    let mut client = rig.client();
    let (layout, panels) = watch_panels(&mut client, &rig.space, 1);
    assert!(names(&layout).is_empty());
    assert!(panels.panels.iter().any(|panel| panel.id == "p"));
    assert_eq!(rig.write_count(LAYOUT_FILE), writes);
    assert_eq!(stored_names(&rig), Some(Vec::new()));
}

#[test]
fn close_answers_unknown_panel_invalid_request_and_unknown_space() {
    let rig = rig();
    let (mut client, _) = watcher(&rig);

    close(&mut client, &rig, 2, "nobody");
    let seen = until_answered(&client, 2);
    assert_eq!(refusal(&seen, 2).code, ErrorCode::UnknownPanel);

    pin(&mut client, &rig, 3, "nobody");
    let seen = until_answered(&client, 3);
    assert_eq!(refusal(&seen, 3).code, ErrorCode::UnknownPanel);

    close(&mut client, &rig, 4, "../up");
    let seen = until_answered(&client, 4);
    let error = refusal(&seen, 4);
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert!(error.message.starts_with("the panel ID isn't valid"));

    client.send(&ClientMessage::ClosePanel(ClosePanel {
        request: RequestId(5),
        space: "/no/such/space".to_string(),
        panel: "p".to_string(),
    }));
    let seen = until_answered(&client, 5);
    assert_eq!(refusal(&seen, 5).code, ErrorCode::UnknownSpace);
}

#[test]
fn a_closed_panel_leaves_the_next_panels_list() {
    let rig = rig();
    rig.create(named("p", markdown("one"))).unwrap();
    rig.create(named("q", markdown("two"))).unwrap();
    wait_stored(&rig, &["p", "q"]);
    let (mut client, _) = watcher(&rig);

    close(&mut client, &rig, 2, "p");
    until_answered(&client, 2);

    let (layout, panels) = watch_panels(&mut client, &rig.space, 3);
    let listed: Vec<&str> = panels
        .panels
        .iter()
        .map(|panel| panel.id.as_str())
        .collect();
    assert_eq!(listed, ["q"]);
    assert_eq!(names(&layout), ["q"]);
}

#[test]
fn close_during_validation_then_recreate_runs_one_check_per_id() {
    let rig = rig();
    let mut wire = McpWire::open(&rig, cwd_caller(&rig));
    wire.wait_loaded();
    let held = "graph TD; H-->I";
    let hold = rig.check.hold(held);
    let first = wire.send_call("show_diagram", json!({"id": "d", "source": held}));
    assert!(
        wait_until(WAIT, || rig.check.in_flight() == 1),
        "the first check never started"
    );
    let (mut client, _) = watcher(&rig);

    close(&mut client, &rig, 2, "d");
    until_answered(&client, 2);
    let ended = wire.recv();
    assert_eq!(ended["id"], first);
    let again = wire.tool("show_diagram", json!({"id": "d", "source": MERMAID}));
    assert!(!again.is_error, "{again:?}");
    hold.release();
    rig.wait_state("d", PanelState::Validated);

    assert_eq!(rig.check.peak_in_flight(), 1);
    let (_fresh, layout) = watcher(&rig);
    assert_eq!(names(&layout), ["d"]);
    assert_eq!(active_of(&layout), ["d"]);
    wait_stored(&rig, &["d"]);
    let bytes = rig.bytes(LAYOUT_FILE).unwrap();
    let stored: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(stored["root"]["active"], "d");
}

fn content_path(id: &str) -> String {
    format!("panels/{id}/content.md")
}

fn file_path(id: &str) -> String {
    format!("panels/{id}/panel.json")
}

const THEIRS_JSON: &[u8] = br#"{"version":1,"renderer":"markdown","title":"theirs"}"#;

#[test]
fn pin_moves_the_folder_content_first_and_survives_restart_and_expiry() {
    let (rig, store) = PanelRig::stalling();
    let mut rig = ready(rig);
    rig.create(named("s", scratch(mermaid(MERMAID)))).unwrap();
    rig.wait_state("s", PanelState::Validated);
    wait_stored(&rig, &["s"]);
    let (mut client, _) = watcher(&rig);

    pin(&mut client, &rig, 2, "s");
    let seen = until_answered(&client, 2);

    assert!(
        matches!(answer_of(&seen, 2), DaemonMessage::PanelPinned(pinned) if pinned.panel == "s"),
        "{seen:?}"
    );
    let writes: Vec<String> = store
        .log()
        .into_iter()
        .filter_map(|op| match op {
            StoreOp::Write(path, _) if path.starts_with("panels/s/") => Some(path),
            _ => None,
        })
        .collect();
    let content = writes.iter().position(|path| path.ends_with("diagram.mmd"));
    let file = writes.iter().position(|path| path.ends_with("panel.json"));
    assert!(
        content.is_some() && content < file,
        "the folder's content wasn't written before its panel.json: {writes:?}"
    );
    assert!(rig.scratch.dirs().is_empty());
    assert_eq!(rig.get("s").unwrap().0.lifetime, Lifetime::Persistent);

    drop(client);
    rig.restart();
    rig.wait_state("s", PanelState::Validated);
    rig.wall.advance(SCRATCH_TTL_MS + 1);
    std::thread::sleep(QUIET);
    let (view, _) = rig.get("s").unwrap();
    assert_eq!(
        (view.lifetime, view.state),
        (Lifetime::Persistent, PanelState::Validated)
    );
    let mut client = rig.client();
    let (layout, _) = watch_panels(&mut client, &rig.space, 1);
    assert_eq!(names(&layout), ["s"]);
}

#[test]
fn pin_into_a_taken_folder_is_refused_and_stays_scratch() {
    let rig = rig();
    rig.create(named("x", scratch(markdown("mine")))).unwrap();
    rig.store
        .write_outside(&rig.space, &file_path("x"), THEIRS_JSON.to_vec());
    rig.store
        .write_outside(&rig.space, &content_path("x"), b"theirs".to_vec());
    assert!(
        wait_until(WAIT, || {
            rig.panels
                .list(&rig.space)
                .is_ok_and(|list| list.skipped.iter().any(|skip| skip.panel == "x"))
        }),
        "the daemon never saw the folder"
    );
    let (mut client, _) = watcher(&rig);

    pin(&mut client, &rig, 2, "x");
    let seen = until_answered(&client, 2);

    let error = refusal(&seen, 2);
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    let (view, source) = rig.get("x").unwrap();
    assert_eq!(error.message, view.file_error.clone().unwrap());
    assert!(error.message.starts_with("Couldn't pin:"), "{error:?}");
    assert_eq!(
        (view.lifetime, source.as_str()),
        (Lifetime::Scratch, "mine")
    );
    assert_eq!(rig.bytes(&file_path("x")).unwrap(), THEIRS_JSON);
    assert_eq!(rig.bytes(&content_path("x")).unwrap(), b"theirs");
    assert_eq!(rig.write_count(&file_path("x")), 0);
    assert_eq!(rig.write_count(&content_path("x")), 0);
    assert!(!rig.scratch.dirs().is_empty());
}

#[test]
fn a_panel_json_that_appears_during_the_pin_undoes_it() {
    let rig = rig();
    rig.create(named("x", scratch(markdown("mine")))).unwrap();
    rig.store
        .write_quiet(&rig.space, &file_path("x"), THEIRS_JSON.to_vec());
    let (mut client, _) = watcher(&rig);

    pin(&mut client, &rig, 2, "x");
    let seen = until_answered(&client, 2);

    let error = refusal(&seen, 2);
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert_eq!(
        error.message,
        "Couldn't pin: .workspace/panels/x/panel.json changed while it was written"
    );
    let (view, source) = rig.get("x").unwrap();
    assert_eq!(
        (view.lifetime, source.as_str()),
        (Lifetime::Scratch, "mine")
    );
    assert_eq!(rig.bytes(&file_path("x")).unwrap(), THEIRS_JSON);
    assert_eq!(rig.bytes(&content_path("x")), None);
    assert_eq!(rig.write_count(&file_path("x")), 0);
}

#[test]
fn pin_into_a_read_only_project_is_undone_with_its_reason() {
    let store = std::sync::Arc::new(combined::testing::MemoryFileStore::new().without_fallback());
    let served: std::sync::Arc<dyn combined::ports::FileStore> = store.clone();
    let mut rig = PanelRig::with_config(PanelServerConfig {
        served: Some(served),
        ..config()
    });
    rig.store = store.clone();
    let rig = ready(rig);
    rig.create(named("s", scratch(markdown("mine")))).unwrap();
    store.set_read_only(&rig.space, true);
    let (mut client, _) = watcher(&rig);

    pin(&mut client, &rig, 2, "s");
    let seen = until_answered(&client, 2);

    let error = refusal(&seen, 2);
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    let (view, _) = rig.get("s").unwrap();
    assert_eq!(view.lifetime, Lifetime::Scratch);
    assert_eq!(Some(error.message.clone()), view.file_error);
    assert!(error.message.contains("read-only"), "{error:?}");
    assert_ne!(error.message, PIN_UNDONE);
    assert!(!rig.scratch.dirs().is_empty());
}

#[test]
fn pinning_a_persistent_panel_is_invalid_request() {
    let rig = rig();
    rig.create(named("p", markdown("one"))).unwrap();
    wait_stored(&rig, &["p"]);
    let (mut client, _) = watcher(&rig);

    pin(&mut client, &rig, 2, "p");
    let seen = until_answered(&client, 2);

    let error = refusal(&seen, 2);
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert_eq!(
        error.message,
        combined::core::PanelApiError::NotScratch {
            id: "p".to_string()
        }
        .to_string()
    );
    assert_eq!(rig.get("p").unwrap().0.lifetime, Lifetime::Persistent);
}
