#![cfg(feature = "test-support")]
//! `get_panel` through the running server: answered from the panel store in memory, with the
//! panel's current revision and source.

mod common;
mod daemon_support;
mod panel_support;

use combine_protocol::{
    ClientMessage, DaemonMessage, ErrorCode, GetPanel, Panel, PanelState, Renderer, RequestId,
};
use combined::core::PanelApiError;
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

fn get_panel(client: &mut WireClient, space: &str, panel: &str, request: u64) {
    client.send(&ClientMessage::GetPanel(GetPanel {
        request: RequestId(request),
        space: space.to_string(),
        panel: panel.to_string(),
    }));
}

fn expect_panel(client: &WireClient, request: u64) -> Panel {
    match client.recv() {
        DaemonMessage::Panel(panel) => {
            assert_eq!(panel.request, RequestId(request));
            panel
        }
        other => panic!("expected a panel, got {other:?}"),
    }
}

fn expect_error(client: &WireClient, request: u64, code: ErrorCode) -> String {
    match client.recv() {
        DaemonMessage::Error(error) => {
            assert_eq!(error.code, code, "{}", error.message);
            assert_eq!(error.request, Some(RequestId(request)));
            error.message
        }
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn get_panel_answers_the_current_revision_with_its_source() {
    let rig = loaded_rig();
    let made = rig.create(markdown("# hello")).unwrap();
    let (view, _) = rig.wait_state(&made.id, PanelState::Validated);
    let mut client = rig.client();

    get_panel(&mut client, &rig.space.0, &made.id, 5);
    let panel = expect_panel(&client, 5);

    assert_eq!(panel.space, rig.space.0);
    assert_eq!(panel.id, made.id);
    assert_eq!(panel.revision, view.revision);
    assert_eq!(panel.renderer, Renderer::Markdown);
    assert_eq!(panel.state, PanelState::Validated);
    assert_eq!(panel.source, "# hello");
    assert_eq!(panel.error, None);

    rig.panels.remove(&rig.space, &made.id).unwrap();
    get_panel(&mut client, &rig.space.0, &made.id, 6);
    let gone = expect_panel(&client, 6);
    assert_eq!(gone.state, PanelState::Removed);
    assert_eq!(gone.source, "");
}

#[test]
fn get_panel_refuses_what_it_cannot_answer() {
    let rig = loaded_rig();
    let mut client = rig.client();

    get_panel(&mut client, "/no/such/space", "p", 1);
    expect_error(&client, 1, ErrorCode::UnknownSpace);

    get_panel(&mut client, &rig.space.0, "nothing", 2);
    expect_error(&client, 2, ErrorCode::UnknownPanel);

    get_panel(&mut client, &rig.space.0, "a/b", 3);
    let text = expect_error(&client, 3, ErrorCode::InvalidRequest);
    assert!(text.starts_with("the panel ID isn't valid: "), "{text}");
}

#[test]
fn an_unsendable_source_is_answered_failed_with_its_request() {
    let rig = loaded_rig();
    let made = rig.create(markdown("small")).unwrap();
    rig.wait_state(&made.id, PanelState::Validated);
    let huge = "\u{1}".repeat(204_800);
    rig.store.write_outside(
        &rig.space,
        &format!("panels/{}/content.md", made.id),
        huge.into_bytes(),
    );
    rig.wait_state(&made.id, PanelState::Failed);
    let mut client = rig.client();

    get_panel(&mut client, &rig.space.0, &made.id, 9);
    let panel = expect_panel(&client, 9);

    assert_eq!(panel.state, PanelState::Failed);
    assert_eq!(panel.source, "");
    assert_eq!(
        panel.error.map(|error| error.code).as_deref(),
        Some("too_large")
    );
}

#[test]
fn a_create_with_an_unsendable_source_is_refused() {
    let rig = loaded_rig();

    let refused = rig.create(markdown(&"\u{1}".repeat(204_800)));

    assert!(
        matches!(refused, Err(PanelApiError::TooLarge { .. })),
        "{refused:?}"
    );
}
