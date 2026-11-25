#![cfg(feature = "test-support")]
//! The six MCP tools over a real channel: what each writes, what it audits, and how it answers
//! when an outside edit races it.

mod common;
mod daemon_support;
mod mcp_support;
mod panel_support;

use std::time::Duration;

use combine_protocol::PanelState;
use combined::core::{LAYOUT_FILE, SCRATCH_TTL_MS};
use combined::ports::ScratchDir;
use daemon_support::wait_until;
use mcp_support::{cwd_caller, wire_options, McpWire};
use panel_support::{settled_read_count, PanelRig, Race};
use serde_json::{json, Value};

const MERMAID: &str = "graph TD; A-->B";
const THEIRS_JSON: &[u8] = br#"{"version":1,"renderer":"markdown","title":"theirs"}"#;
const REMOVE_CONFLICT: &str = "the panel's files changed outside Combine, so it wasn't removed";

fn file_path(id: &str) -> String {
    format!("panels/{id}/panel.json")
}

fn content_path(id: &str) -> String {
    format!("panels/{id}/content.md")
}

/// A rig with a terminal in the project folder and an open channel on its space.
fn rig_and_wire() -> (PanelRig, McpWire) {
    let rig = PanelRig::with_options(wire_options());
    let wire = wire_of(&rig);
    (rig, wire)
}

fn wire_of(rig: &PanelRig) -> McpWire {
    rig.spawn_terminal();
    let mut wire = McpWire::open(rig, cwd_caller(rig));
    wire.wait_loaded();
    wire
}

fn markdown_args(id: &str, content: &str) -> Value {
    json!({"id": id, "renderer": "markdown", "content": content})
}

fn panel_ids(listed: &Value) -> Vec<&str> {
    listed["panels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|panel| panel["panel"].as_str().unwrap())
        .collect()
}

fn wait_validated(rig: &PanelRig, id: &str) {
    rig.wait_state(id, PanelState::Validated);
}

// AC4

#[test]
fn a_repeated_key_makes_a_new_panel_after_a_scratch_expiry() {
    let (rig, mut wire) = rig_and_wire();
    let args = json!({"renderer": "markdown", "content": "one", "lifetime": "scratch", "idempotency_key": "K"});
    let first = wire.tool("create_panel", args.clone());
    assert!(!first.is_error, "{first:?}");
    let id = first.body["panel"].as_str().unwrap().to_string();
    let again = wire.tool("create_panel", args.clone());
    assert_eq!(again.body["panel"], id.as_str());

    rig.wall.advance(SCRATCH_TTL_MS + 1);
    assert!(
        wait_until(Duration::from_secs(5), || {
            matches!(rig.panels.status(&rig.space, &id), Ok(view) if view.state == PanelState::Removed)
        }),
        "the scratch panel never expired"
    );
    let renewed = wire.tool("create_panel", args);
    assert!(!renewed.is_error, "{renewed:?}");
    assert_ne!(renewed.body["panel"], id.as_str());
}

// AC11

#[test]
fn each_mutating_call_writes_one_audit_line_without_the_source() {
    const MARKER: &str = "SECRET-MARKER-4711";
    let (rig, mut wire) = rig_and_wire();
    let space = format!("{:?}", rig.space.0);
    let prefix =
        |tool: &str, panel: &str| format!("mcp {tool} space={space} slot=null panel={panel} ");

    wire.tool("create_panel", markdown_args("c1", MARKER));
    wire.tool("update_panel", json!({"panel": "c1", "content": MARKER}));
    wire.tool(
        "show_diagram",
        json!({"id": "s1", "source": format!("graph TD; {MARKER}-->B"), "title": MARKER}),
    );
    wire.tool("remove_panel", json!({"panel": "c1"}));
    let refused = wire.tool(
        "create_panel",
        json!({"renderer": "terminal", "content": MARKER}),
    );
    assert_eq!(refused.code(), "not_supported");

    let lines = rig.audit.lines();
    assert_eq!(lines.len(), 5, "{lines:?}");
    for (line, (tool, panel)) in lines.iter().zip([
        ("create_panel", r#""c1""#),
        ("update_panel", r#""c1""#),
        ("show_diagram", r#""s1""#),
        ("remove_panel", r#""c1""#),
    ]) {
        assert!(line.starts_with(&prefix(tool, panel)), "{line}");
        assert!(line.contains("revision="), "{line}");
    }
    assert!(lines[3].ends_with("state=removed"), "{}", lines[3]);
    assert_eq!(
        lines[4],
        format!("{}error=not_supported", prefix("create_panel", "null"))
    );
    assert!(lines.iter().all(|line| !line.contains(MARKER)), "{lines:?}");
}

#[test]
fn reads_and_protocol_methods_write_no_audit_line() {
    let (rig, mut wire) = rig_and_wire();
    wire.tool("create_panel", markdown_args("r1", "x"));
    let before = rig.audit.lines().len();

    wire.call("initialize", json!({"protocolVersion": "2025-06-18"}));
    wire.call("ping", json!({}));
    wire.call("tools/list", json!({}));
    wire.tool("panel_status", json!({"panel": "r1"}));
    wire.tool("list_panels", json!({}));
    wire.tool("panel_status", json!({"panel": "nope"}));

    assert_eq!(rig.audit.lines().len(), before);
}

// AC13

#[test]
fn create_refuses_later_milestone_fields_naming_the_milestone() {
    let (rig, mut wire) = rig_and_wire();
    for (field, milestone) in [
        ("kind", "M4"),
        ("group", "M4"),
        ("loader", "M3"),
        ("sources", "M3"),
        ("refresh", "M3"),
        ("params", "M3"),
    ] {
        let mut args = markdown_args("later", "x");
        args[field] = json!("anything");
        let result = wire.tool("create_panel", args);
        assert_eq!(result.code(), "not_supported", "{field}");
        let message = result.body["error"]["message"].as_str().unwrap();
        assert!(message.contains(milestone), "{field}: {message}");
    }
    assert!(rig.list_ids().is_empty());
    assert_eq!(rig.write_count(&file_path("later")), 0);
}

#[test]
fn create_refuses_terminals() {
    let (rig, mut wire) = rig_and_wire();
    let result = wire.tool(
        "create_panel",
        json!({"id": "t", "renderer": "terminal", "content": "x"}),
    );
    assert_eq!(result.code(), "not_supported");
    assert!(result.body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("terminals can't be created through MCP"));
    assert!(rig.list_ids().is_empty());
}

#[test]
fn show_diagram_makes_a_scratch_mermaid_panel() {
    let (rig, mut wire) = rig_and_wire();
    let shown = wire.tool("show_diagram", json!({"id": "d1", "source": MERMAID}));
    assert!(!shown.is_error, "{shown:?}");
    assert_eq!(shown.body["lifetime"], "scratch");
    assert_eq!(shown.body["renderer"], "mermaid");
    let dir = ScratchDir {
        key: rig.space.sha256_hex(),
        panel: "d1".to_string(),
    };
    assert_eq!(rig.scratch.dirs(), vec![dir]);
    assert_eq!(rig.bytes(&file_path("d1")), None);
}

// AC14

#[test]
fn placement_is_stored_and_no_layout_is_written() {
    let (rig, mut wire) = rig_and_wire();
    let mut args = markdown_args("pl", "x");
    args["placement"] = json!("main");
    let created = wire.tool("create_panel", args);
    assert!(!created.is_error, "{created:?}");
    assert_eq!(created.body["placement"], "main");
    wait_validated(&rig, "pl");

    let stored: Value = serde_json::from_slice(&rig.bytes(&file_path("pl")).unwrap()).unwrap();
    assert_eq!(stored["placement"], "main");
    assert_eq!(rig.write_count(LAYOUT_FILE), 0);
}

#[test]
fn an_invalid_placement_is_invalid_field() {
    let (rig, mut wire) = rig_and_wire();
    for placement in ["p".repeat(257), "a\u{7}b".to_string()] {
        let mut args = markdown_args("bad", "x");
        args["placement"] = json!(placement);
        let result = wire.tool("create_panel", args);
        assert_eq!(result.code(), "invalid_field");
        assert_eq!(result.body["error"]["field"], "placement");
    }
    assert!(rig.list_ids().is_empty());
}

// AC16

#[test]
fn a_future_revision_is_unknown_revision() {
    let (rig, mut wire) = rig_and_wire();
    let created = wire.tool("create_panel", markdown_args("r", "one"));
    let r1 = created.body["revision"].as_u64().unwrap();
    wait_validated(&rig, "r");
    let updated = wire.tool("update_panel", json!({"panel": "r", "content": "two"}));
    let r2 = updated.body["revision"].as_u64().unwrap();
    assert!(r2 > r1, "{r1} {r2}");

    let future = wire.tool("panel_status", json!({"panel": "r", "revision": r2 + 5}));
    assert_eq!(future.code(), "unknown_revision");
    let past = wire.tool("panel_status", json!({"panel": "r", "revision": r1}));
    assert!(!past.is_error, "{past:?}");
    assert_eq!(past.body["superseded"], true);
    let current = wire.tool("panel_status", json!({"panel": "r", "revision": r2}));
    assert_eq!(current.body["superseded"], false);
}

// AC17

#[test]
fn list_panels_lists_only_the_callers_space() {
    let (rig, mut in_a) = rig_and_wire();
    let other = rig.dir.path().join("other");
    std::fs::create_dir_all(other.join(".git")).unwrap();
    rig.spawn_terminal_in(&other);
    let mut in_b = McpWire::open(
        &rig,
        combine_protocol::McpCaller {
            terminal_id: None,
            terminal_socket: None,
            cwd: Some(other.to_str().unwrap().to_string()),
        },
    );
    in_b.wait_loaded();
    in_a.tool("create_panel", markdown_args("a1", "a"));
    in_b.tool("create_panel", markdown_args("b1", "b"));

    let listed_a = in_a.tool("list_panels", json!({}));
    assert_eq!(listed_a.body["space"], rig.space.0.as_str());
    assert_eq!(panel_ids(&listed_a.body), ["a1"]);
    let listed_b = in_b.tool("list_panels", json!({}));
    assert_eq!(listed_b.body["space"], other.to_str().unwrap());
    assert_eq!(panel_ids(&listed_b.body), ["b1"]);
}

#[test]
fn remove_panel_removes_persistent_and_scratch_panels() {
    let (rig, mut wire) = rig_and_wire();
    wire.tool("create_panel", markdown_args("p", "x"));
    wire.tool(
        "create_panel",
        json!({"id": "s", "renderer": "markdown", "content": "y", "lifetime": "scratch"}),
    );
    wait_validated(&rig, "p");

    let removed = wire.tool("remove_panel", json!({"panel": "p"}));
    assert!(!removed.is_error, "{removed:?}");
    assert_eq!(removed.body["state"], "removed");
    assert_eq!(rig.bytes(&file_path("p")), None);
    assert_eq!(rig.remove_count(&file_path("p")), 1);

    let removed = wire.tool("remove_panel", json!({"panel": "s"}));
    assert!(!removed.is_error, "{removed:?}");
    assert_eq!(removed.body["state"], "removed");
    assert!(rig.scratch.dirs().is_empty());
}

#[test]
fn a_remove_that_conflicts_with_an_outside_edit_is_remove_conflict_and_the_panel_stays() {
    let (rig, racing) = PanelRig::racing_with(wire_options());
    let mut wire = wire_of(&rig);
    let created = wire.tool("create_panel", markdown_args("p", "one"));
    let r = created.body["revision"].as_u64().unwrap();
    wait_validated(&rig, "p");
    settled_read_count(&rig.store, &rig.space, &file_path("p"), 1);

    racing.arm(
        &rig.space,
        &file_path("p"),
        Race::BeforeCompare,
        THEIRS_JSON.to_vec(),
    );
    let removed = wire.tool("remove_panel", json!({"panel": "p"}));

    assert!(!racing.armed(), "the remove never read the file");
    assert!(removed.is_error, "{removed:?}");
    assert_eq!(removed.code(), "remove_conflict");
    assert_eq!(removed.body["error"]["message"], REMOVE_CONFLICT);
    assert_eq!(removed.body["panel"]["id"], "p");
    assert_ne!(removed.body["panel"]["state"], "removed");
    assert!(removed.body["panel"]["revision"].as_u64().unwrap() >= r);
    let status = wire.tool("panel_status", json!({"panel": "p"}));
    assert!(!status.is_error, "{status:?}");
    assert_ne!(status.body["state"], "removed");
    assert_eq!(rig.remove_count(&file_path("p")), 0);
    assert!(rig
        .audit
        .lines()
        .last()
        .unwrap()
        .ends_with("error=remove_conflict"));
}

// AC20

#[test]
fn a_255_byte_id_is_accepted_and_256_refused_with_nothing_written() {
    for lifetime in ["persistent", "scratch"] {
        let (rig, mut wire) = rig_and_wire();
        let long = "a".repeat(256);
        let args = |id: &str| json!({"id": id, "renderer": "markdown", "content": "x", "lifetime": lifetime});
        let refused = wire.tool("create_panel", args(&long));
        assert_eq!(refused.code(), "invalid_id", "{lifetime}");
        assert_eq!(
            refused.body["error"]["message"],
            "the panel ID isn't valid: a panel ID can be at most 255 bytes"
        );
        assert_eq!(rig.write_count(&file_path(&long)), 0);
        assert_eq!(rig.write_count(&content_path(&long)), 0);
        assert!(rig.scratch.dirs().is_empty());

        let accepted = wire.tool("create_panel", args(&long[1..]));
        assert!(!accepted.is_error, "{lifetime}: {accepted:?}");
        assert_eq!(accepted.body["panel"].as_str().unwrap().len(), 255);
    }
}

// AC21

#[test]
fn a_title_update_while_panel_json_is_invalid_is_panel_file_invalid() {
    let (rig, mut wire) = rig_and_wire();
    wire.tool("create_panel", markdown_args("p", "one"));
    wait_validated(&rig, "p");

    rig.store
        .write_outside(&rig.space, &file_path("p"), b"{not json".to_vec());
    assert!(
        wait_until(Duration::from_secs(5), || {
            matches!(rig.panels.status(&rig.space, "p"), Ok(view) if view.file_error.is_some())
        }),
        "the invalid panel.json was never seen"
    );

    let updated = wire.tool("update_panel", json!({"panel": "p", "title": "new"}));
    assert_eq!(updated.code(), "panel_file_invalid");
}

// AC22

#[test]
fn a_create_that_gives_way_answers_the_folders_panel_superseded() {
    let (rig, racing) = PanelRig::racing_with(wire_options());
    let mut wire = wire_of(&rig);
    racing.arm(
        &rig.space,
        &file_path("x"),
        Race::BeforeCompare,
        THEIRS_JSON.to_vec(),
    );

    let created = wire.tool("create_panel", markdown_args("x", "ours"));

    assert!(!racing.armed(), "the create never read the file");
    assert!(!created.is_error, "{created:?}");
    assert_eq!(created.body["superseded"], true);
    assert_eq!(created.body["title"], "theirs");
    let status = wire.tool("panel_status", json!({"panel": "x"}));
    assert_eq!(status.body["title"], "theirs");
    assert_eq!(rig.write_count(&content_path("x")), 0);
}

#[test]
fn panel_errors_reach_the_tools_as_their_own_codes() {
    let (rig, mut wire) = rig_and_wire();
    let created = wire.tool("create_panel", markdown_args("taken", "one"));
    assert!(!created.is_error, "{created:?}");

    // IdTaken: a second create of the same ID, with no idempotency key.
    let again = wire.tool("create_panel", markdown_args("taken", "two"));
    assert_eq!(again.code(), "id_taken");

    // UnknownPanel: an update and a remove of a panel the space doesn't have.
    let missing = wire.tool("update_panel", json!({"panel": "nope", "content": "x"}));
    assert_eq!(missing.code(), "unknown_panel");
    let missing = wire.tool("remove_panel", json!({"panel": "nope"}));
    assert_eq!(missing.code(), "unknown_panel");

    // Removed: an update of a panel that was removed.
    wait_validated(&rig, "taken");
    let removed = wire.tool("remove_panel", json!({"panel": "taken"}));
    assert!(!removed.is_error, "{removed:?}");
    let gone = wire.tool("update_panel", json!({"panel": "taken", "content": "x"}));
    assert_eq!(gone.code(), "removed");

    // InvalidField: an update that changes nothing.
    let other = wire.tool("create_panel", markdown_args("other", "one"));
    assert!(!other.is_error, "{other:?}");
    let nothing = wire.tool("update_panel", json!({"panel": "other"}));
    assert_eq!(nothing.code(), "invalid_field");

    // Each refusal wrote nothing for the panel it named.
    assert_eq!(rig.write_count(&file_path("nope")), 0);
}

#[test]
fn the_101st_panel_is_panel_limit() {
    let (_rig, mut wire) = rig_and_wire();
    for n in 0..combined::core::MAX_PANELS {
        let made = wire.tool("create_panel", markdown_args(&format!("p{n}"), "x"));
        assert!(!made.is_error, "{n}: {made:?}");
    }
    let over = wire.tool("create_panel", markdown_args("one-too-many", "x"));
    assert_eq!(over.code(), "panel_limit");
}
