#![cfg(feature = "test-support")]
//! The panels a client that watches a space is sent: with the layout that answers its watch, and
//! pushed when the space's panels change. In-memory stores and a fake diagram check.

mod common;
mod daemon_support;
mod panel_support;

use std::time::Duration;

use combine_protocol::{
    ClientMessage, DaemonMessage, PanelState, PanelSummary, Panels, RequestId, UnwatchLayout,
    WatchTerminals,
};
use combined::core::{PanelUpdate, PANELS_LOADING};
use daemon_support::{wait_until, WireClient};
use panel_support::{markdown, next_panels, watch_panels, PanelRig, WAIT};

/// How long a test waits to be sure nothing is sent.
const QUIET: Duration = Duration::from_millis(300);

fn content_path(id: &str) -> String {
    format!("panels/{id}/content.md")
}

/// A rig whose project is a space with its panels read.
fn loaded_rig() -> PanelRig {
    let rig = PanelRig::new();
    rig.spawn_terminal();
    assert!(
        wait_until(WAIT, || rig.panels.list(&rig.space).is_ok()),
        "the space's panels were never read"
    );
    rig
}

fn listed<'a>(panels: &'a Panels, id: &str) -> Option<&'a PanelSummary> {
    panels.panels.iter().find(|panel| panel.id == id)
}

/// The first push that `wanted` accepts. Every push before it must be one too: no request.
fn pushed_until(client: &WireClient, what: &str, wanted: impl Fn(&Panels) -> bool) -> Panels {
    loop {
        let Some(panels) = next_panels(client, WAIT) else {
            panic!("no panels push for: {what}");
        };
        assert_eq!(panels.request, None, "a push answers no request");
        if wanted(&panels) {
            return panels;
        }
    }
}

fn is_validated(panels: &Panels, id: &str) -> bool {
    listed(panels, id).is_some_and(|panel| panel.state == PanelState::Validated)
}

#[test]
fn the_answer_follows_the_layout_with_its_request() {
    let rig = loaded_rig();
    let made = rig.create(markdown("# one")).unwrap();
    rig.wait_state(&made.id, PanelState::Validated);

    let mut client = rig.client();
    let (layout, panels) = watch_panels(&mut client, &rig.space, 7);

    assert_eq!(layout.request, Some(RequestId(7)));
    assert_eq!(panels.request, Some(RequestId(7)));
    assert_eq!(panels.space, rig.space.0);
    assert_eq!(panels.error, None);
    assert!(is_validated(&panels, &made.id), "{panels:?}");
}

#[test]
fn a_panel_made_with_no_request_reaches_a_watcher() {
    let rig = loaded_rig();
    let mut client = rig.client();
    let (_, first) = watch_panels(&mut client, &rig.space, 1);
    assert!(first.panels.is_empty());

    let made = rig.create(markdown("# one")).unwrap();

    let push = pushed_until(&client, "the new panel", |panels| {
        is_validated(panels, &made.id)
    });
    assert_eq!(push.space, rig.space.0);
}

#[test]
fn updates_and_removals_are_pushed() {
    let rig = loaded_rig();
    let made = rig.create(markdown("one")).unwrap();
    let (before, _) = rig.wait_state(&made.id, PanelState::Validated);
    let mut client = rig.client();
    watch_panels(&mut client, &rig.space, 1);

    rig.panels
        .update(
            &rig.space,
            &made.id,
            PanelUpdate {
                content: Some("two".to_string()),
                title: None,
            },
        )
        .unwrap();
    let push = pushed_until(&client, "the update", |panels| {
        listed(panels, &made.id).is_some_and(|panel| {
            panel.revision > before.revision && panel.state == PanelState::Validated
        })
    });
    assert_eq!(push.request, None);

    rig.panels.remove(&rig.space, &made.id).unwrap();
    pushed_until(&client, "the removal", |panels| {
        listed(panels, &made.id).is_none()
    });
}

#[test]
fn an_outside_edit_is_pushed() {
    let rig = loaded_rig();
    let made = rig.create(markdown("one")).unwrap();
    let (before, _) = rig.wait_state(&made.id, PanelState::Validated);
    let mut client = rig.client();
    watch_panels(&mut client, &rig.space, 1);

    rig.store
        .write_outside(&rig.space, &content_path(&made.id), b"two".to_vec());

    pushed_until(&client, "the outside edit", |panels| {
        listed(panels, &made.id).is_some_and(|panel| {
            panel.revision > before.revision && panel.state == PanelState::Validated
        })
    });
}

#[test]
fn events_with_no_watcher_send_nothing() {
    let rig = loaded_rig();
    let never = rig.client();
    let mut unwatched = rig.client();
    watch_panels(&mut unwatched, &rig.space, 1);
    unwatched.send(&ClientMessage::UnwatchLayout(UnwatchLayout {}));
    // The answer to this comes after the unwatch has been handled.
    unwatched.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(2),
    }));
    loop {
        if let DaemonMessage::Terminals(_) = unwatched.recv() {
            break;
        }
    }

    let made = rig.create(markdown("one")).unwrap();
    rig.wait_state(&made.id, PanelState::Validated);
    rig.store
        .write_outside(&rig.space, &content_path(&made.id), b"two".to_vec());
    rig.wait_state(&made.id, PanelState::Validated);

    assert!(
        never.recv_within(QUIET).is_none(),
        "a client that never watched was sent a frame"
    );
    assert!(
        next_panels(&unwatched, QUIET).is_none(),
        "a client that stopped watching was sent panels"
    );
}

#[test]
fn a_loading_space_gets_an_empty_list_then_a_push() {
    let (rig, store) = PanelRig::stalling();
    let held = store.stall_list(&rig.space, "panels");
    rig.spawn_terminal();
    let mut client = rig.client();

    let (_, loading) = watch_panels(&mut client, &rig.space, 3);
    assert!(
        wait_until(WAIT, || store.entered()),
        "the listing never started"
    );
    assert_eq!(loading.request, Some(RequestId(3)));
    assert!(loading.panels.is_empty());
    assert_eq!(loading.error.as_deref(), Some(PANELS_LOADING));

    held.release();
    let push = pushed_until(&client, "the loaded list", |panels| panels.error.is_none());
    assert!(push.panels.is_empty());
}

/// 256 folders with 255-byte names, 100 of them panels with the longest titles, in a space whose
/// first listing finds them all.
#[test]
fn a_hundred_worst_case_panels_fit_one_frame() {
    let rig = PanelRig::new();
    let title = "\"".repeat(1_024);
    let mut ids: Vec<String> = (0..256).map(|index| format!("{index:a>255}")).collect();
    ids.sort();
    for (index, id) in ids.iter().enumerate() {
        // The store reads the first 100 folders by name: those are the panels.
        let file = if index < 100 {
            format!(
                r#"{{"version":1,"renderer":"markdown","title":"{}"}}"#,
                title.replace('"', "\\\"")
            )
        } else {
            r#"{"version":1,"renderer":"nope"}"#.to_string()
        };
        rig.store.write_outside(
            &rig.space,
            &format!("panels/{id}/panel.json"),
            file.into_bytes(),
        );
        if index < 100 {
            rig.store
                .write_outside(&rig.space, &content_path(id), b"x".to_vec());
        }
    }
    rig.spawn_terminal();
    assert!(
        wait_until(WAIT, || rig.panels.list(&rig.space).is_ok()),
        "the space's panels were never read"
    );
    let mut client = rig.client();

    let (_, panels) = watch_panels(&mut client, &rig.space, 1);

    assert!(panels
        .error
        .as_deref()
        .is_some_and(|error| error.contains("more than 100")));
    assert_eq!(panels.panels.len(), 100);
    assert!(panels
        .panels
        .iter()
        .all(|panel| panel.title.as_deref().map(str::len) == Some(1_024)));
}

/// AC11 on the wire: a list the daemon really has to trim. The longest titles, failed with the
/// longest control-character errors, make 100 panels far larger than one frame. The `panels`
/// message the client is sent is the trimmed prefix, and the rest is in `skipped`.
#[test]
fn a_list_larger_than_one_frame_is_trimmed_on_the_wire() {
    use combine_protocol::SkippedPanel;
    use combined::core::TRIMMED_REASON;
    use combined::ports::{DiagramFailure, DiagramVerdict, FailureCode};

    let rig = PanelRig::new();
    rig.check
        .answer_all(Ok(DiagramVerdict::Failed(DiagramFailure {
            code: FailureCode::Syntax,
            message: "\u{1}".repeat(1_024),
            line: Some(u32::MAX),
            column: Some(u32::MAX),
            snippet: Some("\u{1}".repeat(1_024)),
        })));
    let title = "\\\"".repeat(1_024);
    let mut ids: Vec<String> = (0..100).map(|index| format!("{index:a>255}")).collect();
    ids.sort();
    for id in &ids {
        let file = format!(r#"{{"version":1,"renderer":"mermaid","title":"{title}"}}"#);
        rig.store.write_outside(
            &rig.space,
            &format!("panels/{id}/panel.json"),
            file.into_bytes(),
        );
        rig.store
            .write_outside(&rig.space, &content_path(id), b"x".to_vec());
    }
    rig.spawn_terminal();
    assert!(
        wait_until(WAIT, || {
            rig.panels.list(&rig.space).is_ok_and(|list| {
                list.panels.len() == 100
                    && list.panels.iter().all(|p| p.state == PanelState::Failed)
            })
        }),
        "the 100 panels never all failed"
    );
    let mut client = rig.client();

    let (_, panels) = watch_panels(&mut client, &rig.space, 1);

    let kept = panels.panels.len();
    assert!(kept > 0 && kept < 100, "{kept} panels kept; a trim was due");
    for (shown, id) in panels.panels.iter().zip(&ids) {
        assert_eq!(&shown.id, id, "the kept panels are the ID-order prefix");
    }
    let trimmed: Vec<&SkippedPanel> = panels
        .skipped
        .iter()
        .filter(|skipped| skipped.reason == TRIMMED_REASON)
        .collect();
    assert_eq!(trimmed.len(), 100 - kept);
    for (skipped, id) in trimmed.iter().zip(&ids[kept..]) {
        assert_eq!(&skipped.panel, id);
    }
    // It was encoded and sent: the client has the frame.
    assert_eq!(panels.request, Some(RequestId(1)));
}
