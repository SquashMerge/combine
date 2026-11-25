#![cfg(feature = "test-support")]
//! The per-connection render status store (D19).

use combine_protocol::{PanelError, RenderStatus};
use combined::core::PANEL_ERROR_TEXT_MAX;
use combined::core::{
    ClientRender, ConnectionId, RenderStatuses, RENDER_ERROR_CODE_MAX,
    RENDER_STATUS_MAX_PER_CONNECTION,
};
use combined::model::SpaceId;

fn space() -> SpaceId {
    SpaceId("/work/app".to_owned())
}

fn rendered(revision: u64) -> ClientRender {
    ClientRender {
        revision,
        status: RenderStatus::Rendered,
        error: None,
    }
}

fn failed(revision: u64, code: &str) -> ClientRender {
    ClientRender {
        revision,
        status: RenderStatus::RenderFailed,
        error: Some(PanelError {
            code: code.to_owned(),
            message: "bad".to_owned(),
            line: Some(2),
            column: Some(3),
            snippet: Some("  A -> B".to_owned()),
        }),
    }
}

#[test]
fn records_per_connection() {
    let mut statuses = RenderStatuses::new();
    let (a, b) = (ConnectionId(1), ConnectionId(2));
    assert!(statuses.record(b, &space(), "doc", failed(3, "x"), 3));
    assert!(statuses.record(a, &space(), "doc", rendered(3), 3));
    assert_eq!(
        statuses.render_status(&space(), "doc"),
        vec![rendered(3), failed(3, "x")]
    );
    assert!(statuses
        .render_status(&SpaceId("/other".to_owned()), "doc")
        .is_empty());
    assert!(statuses.render_status(&space(), "other").is_empty());
    assert_eq!((statuses.len_of(a), statuses.len_of(b)), (1, 1));
    assert_eq!(statuses.len_of(ConnectionId(9)), 0);
}

#[test]
fn an_older_revision_never_overwrites_a_newer_one() {
    let mut statuses = RenderStatuses::new();
    let conn = ConnectionId(1);
    assert!(statuses.record(conn, &space(), "doc", rendered(5), 6));
    assert!(!statuses.record(conn, &space(), "doc", failed(4, "late"), 6));
    assert_eq!(statuses.render_status(&space(), "doc"), vec![rendered(5)]);
    assert!(statuses.record(conn, &space(), "doc", failed(6, "new"), 6));
    assert_eq!(
        statuses.render_status(&space(), "doc"),
        vec![failed(6, "new")]
    );
}

#[test]
fn an_equal_revision_replaces() {
    let mut statuses = RenderStatuses::new();
    let conn = ConnectionId(1);
    assert!(statuses.record(conn, &space(), "doc", rendered(2), 2));
    assert!(statuses.record(conn, &space(), "doc", failed(2, "again"), 2));
    assert_eq!(
        statuses.render_status(&space(), "doc"),
        vec![failed(2, "again")]
    );
    assert_eq!(statuses.len_of(conn), 1);
}

#[test]
fn a_future_revision_is_ignored() {
    let mut statuses = RenderStatuses::new();
    let conn = ConnectionId(1);
    assert!(!statuses.record(conn, &space(), "doc", rendered(4), 3));
    assert!(statuses.render_status(&space(), "doc").is_empty());
    assert_eq!(statuses.len_of(conn), 0);
}

#[test]
fn the_1025th_panel_evicts_the_least_recent() {
    let mut statuses = RenderStatuses::new();
    let conn = ConnectionId(1);
    for n in 0..RENDER_STATUS_MAX_PER_CONNECTION {
        assert!(statuses.record(conn, &space(), &format!("p{n}"), rendered(1), 1));
    }
    assert_eq!(statuses.len_of(conn), 1_024);
    // Re-recording p0 makes p1 the least recent.
    assert!(statuses.record(conn, &space(), "p0", rendered(1), 1));
    assert_eq!(statuses.len_of(conn), 1_024);
    assert!(statuses.record(conn, &space(), "extra", rendered(1), 1));
    assert_eq!(statuses.len_of(conn), 1_024);
    assert!(statuses.render_status(&space(), "p1").is_empty());
    assert_eq!(statuses.render_status(&space(), "p0").len(), 1);
    assert_eq!(statuses.render_status(&space(), "extra").len(), 1);
    // Another connection is unaffected.
    let other = ConnectionId(2);
    assert!(statuses.record(other, &space(), "p1", rendered(1), 1));
    assert_eq!(statuses.render_status(&space(), "p1").len(), 1);
}

#[test]
fn disconnect_drops_only_that_connection() {
    let mut statuses = RenderStatuses::new();
    let (a, b) = (ConnectionId(1), ConnectionId(2));
    statuses.record(a, &space(), "doc", rendered(1), 1);
    statuses.record(b, &space(), "doc", failed(1, "x"), 1);
    statuses.disconnect(a);
    assert_eq!(statuses.len_of(a), 0);
    assert_eq!(
        statuses.render_status(&space(), "doc"),
        vec![failed(1, "x")]
    );
    statuses.disconnect(a);
    statuses.disconnect(b);
    assert!(statuses.render_status(&space(), "doc").is_empty());
}

#[test]
fn rendered_keeps_no_error_and_texts_are_clipped() {
    let mut statuses = RenderStatuses::new();
    let conn = ConnectionId(1);
    let with_error = ClientRender {
        error: failed(1, "x").error,
        ..rendered(1)
    };
    assert!(statuses.record(conn, &space(), "doc", with_error, 1));
    assert_eq!(statuses.render_status(&space(), "doc"), vec![rendered(1)]);

    let long = ClientRender {
        revision: 1,
        status: RenderStatus::RenderFailed,
        error: Some(PanelError {
            code: "é".repeat(RENDER_ERROR_CODE_MAX),
            message: "m".repeat(PANEL_ERROR_TEXT_MAX + 10),
            line: Some(1),
            column: None,
            snippet: Some("é".repeat(PANEL_ERROR_TEXT_MAX)),
        }),
    };
    assert!(statuses.record(conn, &space(), "doc", long, 1));
    let stored = statuses.render_status(&space(), "doc");
    let error = stored[0]
        .error
        .clone()
        .expect("a render_failed keeps its error");
    assert_eq!(error.code, "é".repeat(RENDER_ERROR_CODE_MAX / 2));
    assert_eq!(error.message.len(), PANEL_ERROR_TEXT_MAX);
    assert_eq!(error.snippet.expect("snippet").len(), PANEL_ERROR_TEXT_MAX);
    assert_eq!((error.line, error.column), (Some(1), None));
}
