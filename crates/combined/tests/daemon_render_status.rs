#![cfg(feature = "test-support")]
//! `report_render` through the running server: what each connection reports is kept per
//! connection, read through the panel API, and forgotten when the connection ends.

mod common;
mod daemon_support;
mod panel_support;

use combine_protocol::{
    ClientMessage, DaemonMessage, ErrorCode, GetPanel, PanelError, PanelState, RenderStatus,
    ReportRender, RequestId,
};
use combined::core::{ClientRender, PanelUpdate};
use daemon_support::{wait_until, WireClient};
use panel_support::{markdown, PanelRig, WAIT};

fn loaded_rig() -> PanelRig {
    let rig = PanelRig::new();
    rig.spawn_terminal();
    assert!(
        wait_until(WAIT, || rig.panels.list(&rig.space).is_ok()),
        "the space's panels were never read"
    );
    rig
}

fn report(
    client: &mut WireClient,
    rig: &PanelRig,
    id: &str,
    revision: u64,
    status: RenderStatus,
    error: Option<PanelError>,
) {
    client.send(&ClientMessage::ReportRender(ReportRender {
        space: rig.space.0.clone(),
        panel: id.to_string(),
        revision,
        status,
        error,
    }));
}

/// Returns once the daemon has handled everything the client sent before: its refusal of an
/// invalid ID is the next frame, and a report never gets one.
fn sync(client: &mut WireClient, rig: &PanelRig) {
    client.send(&ClientMessage::GetPanel(GetPanel {
        request: RequestId(900),
        space: rig.space.0.clone(),
        panel: "a/b".to_string(),
    }));
    match client.recv() {
        DaemonMessage::Error(error) => {
            assert_eq!(error.code, ErrorCode::InvalidRequest);
            assert_eq!(error.request, Some(RequestId(900)), "a report was answered");
        }
        other => panic!("expected the refusal that ends the sync, got {other:?}"),
    }
}

fn rendered(revision: u64) -> ClientRender {
    ClientRender {
        revision,
        status: RenderStatus::Rendered,
        error: None,
    }
}

fn statuses(rig: &PanelRig, id: &str) -> Vec<ClientRender> {
    rig.panels.render_status(&rig.space, id).unwrap()
}

fn validated_panel(rig: &PanelRig) -> (String, u64) {
    let made = rig.create(markdown("one")).unwrap();
    let (view, _) = rig.wait_state(&made.id, PanelState::Validated);
    (made.id, view.revision)
}

fn broken(code: &str) -> PanelError {
    PanelError {
        code: code.to_string(),
        message: "the diagram has a problem".to_string(),
        line: Some(2),
        column: Some(4),
        snippet: Some("  A -> B".to_string()),
    }
}

#[test]
fn report_render_gets_no_reply() {
    let rig = loaded_rig();
    let (id, revision) = validated_panel(&rig);
    let mut client = rig.client();

    report(
        &mut client,
        &rig,
        &id,
        revision,
        RenderStatus::Rendered,
        None,
    );
    sync(&mut client, &rig);

    assert_eq!(statuses(&rig, &id), vec![rendered(revision)]);
}

#[test]
fn status_is_recorded_per_connection() {
    let rig = loaded_rig();
    let (id, revision) = validated_panel(&rig);
    let mut first = rig.client();
    let mut second = rig.client();

    report(
        &mut first,
        &rig,
        &id,
        revision,
        RenderStatus::Rendered,
        None,
    );
    sync(&mut first, &rig);
    report(
        &mut second,
        &rig,
        &id,
        revision,
        RenderStatus::RenderFailed,
        Some(broken("syntax")),
    );
    sync(&mut second, &rig);

    let all = statuses(&rig, &id);
    assert_eq!(all.len(), 2);
    assert_eq!(all[0], rendered(revision));
    assert_eq!(all[1].status, RenderStatus::RenderFailed);
    assert_eq!(
        all[1].error.as_ref().map(|error| error.code.as_str()),
        Some("syntax")
    );
}

#[test]
fn an_older_report_never_overwrites_the_current_one() {
    let rig = loaded_rig();
    let (id, first) = validated_panel(&rig);
    rig.panels
        .update(
            &rig.space,
            &id,
            PanelUpdate {
                content: Some("two".to_string()),
                title: None,
            },
        )
        .unwrap();
    let mut client = rig.client();
    assert!(wait_until(WAIT, || {
        matches!(rig.get(&id), Ok((view, _)) if view.revision > first && view.state == PanelState::Validated)
    }));
    let (view, _) = rig.get(&id).unwrap();

    report(
        &mut client,
        &rig,
        &id,
        view.revision,
        RenderStatus::Rendered,
        None,
    );
    report(
        &mut client,
        &rig,
        &id,
        first,
        RenderStatus::RenderFailed,
        Some(broken("old")),
    );
    sync(&mut client, &rig);

    assert_eq!(statuses(&rig, &id), vec![rendered(view.revision)]);
}

#[test]
fn status_is_dropped_when_the_connection_closes() {
    let rig = loaded_rig();
    let (id, revision) = validated_panel(&rig);
    let mut client = rig.client();
    report(
        &mut client,
        &rig,
        &id,
        revision,
        RenderStatus::Rendered,
        None,
    );
    sync(&mut client, &rig);
    assert_eq!(statuses(&rig, &id).len(), 1);

    drop(client);

    assert!(
        wait_until(WAIT, || statuses(&rig, &id).is_empty()),
        "the report of a closed connection is still kept"
    );
}

#[test]
fn status_is_dropped_after_a_malformed_frame() {
    let rig = loaded_rig();
    let (id, revision) = validated_panel(&rig);
    let mut client = rig.client();
    report(
        &mut client,
        &rig,
        &id,
        revision,
        RenderStatus::Rendered,
        None,
    );
    sync(&mut client, &rig);
    assert_eq!(statuses(&rig, &id).len(), 1);

    let mut garbage = 9u32.to_be_bytes().to_vec();
    garbage.push(1);
    garbage.extend_from_slice(b"not json!");
    client.send_bytes(&garbage);

    assert!(
        wait_until(WAIT, || statuses(&rig, &id).is_empty()),
        "the report of a connection that sent a malformed frame is still kept"
    );
}

#[test]
fn reports_for_unknown_removed_or_future_revisions_are_ignored() {
    let rig = loaded_rig();
    let (id, revision) = validated_panel(&rig);
    let (removed, removed_revision) = validated_panel(&rig);
    rig.panels.remove(&rig.space, &removed).unwrap();
    let mut client = rig.client();

    report(
        &mut client,
        &rig,
        "nothing",
        1,
        RenderStatus::Rendered,
        None,
    );
    report(
        &mut client,
        &rig,
        &removed,
        removed_revision,
        RenderStatus::Rendered,
        None,
    );
    report(
        &mut client,
        &rig,
        &id,
        revision + 5,
        RenderStatus::Rendered,
        None,
    );
    client.send(&ClientMessage::ReportRender(ReportRender {
        space: "/no/such/space".to_string(),
        panel: id.clone(),
        revision,
        status: RenderStatus::Rendered,
        error: None,
    }));
    sync(&mut client, &rig);

    assert_eq!(statuses(&rig, &id), Vec::new());
    assert_eq!(statuses(&rig, "nothing"), Vec::new());
    assert_eq!(statuses(&rig, &removed), Vec::new());
}

#[test]
fn render_failed_is_visible_to_panel_status_s_reader() {
    let rig = loaded_rig();
    let (id, revision) = validated_panel(&rig);
    let mut client = rig.client();

    report(
        &mut client,
        &rig,
        &id,
        revision,
        RenderStatus::RenderFailed,
        Some(broken("render_error")),
    );
    sync(&mut client, &rig);

    let all = statuses(&rig, &id);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].revision, revision);
    assert_eq!(all[0].status, RenderStatus::RenderFailed);
    assert_eq!(all[0].error, Some(broken("render_error")));
}
