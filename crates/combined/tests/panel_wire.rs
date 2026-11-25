#![cfg(feature = "test-support")]
//! The `panels` and `panel` messages, and the trim that keeps a list within one frame.

use combine_protocol::{
    CreatedBy, DaemonMessage, PanelError, PanelState, Panels, Renderer, RequestId, SkippedPanel,
    WireMessage,
};
use combined::core::{
    panel_reply, panel_summary, panels_loading, panels_message, PanelList, PanelView,
    PANELS_LOADING, SKIPPED_TRIMMED, TRIMMED_REASON,
};
use combined::model::{Lifetime, SpaceId, ViewPanelFile};

fn view(space: &SpaceId, id: &str, state: PanelState) -> PanelView {
    PanelView {
        space: space.clone(),
        id: id.to_owned(),
        lifetime: Lifetime::Persistent,
        file: ViewPanelFile {
            renderer: Renderer::Markdown,
            title: Some("Title".to_owned()),
            idempotency_key: None,
            created_by: Some(CreatedBy::User),
            placement: None,
        },
        revision: 3,
        state,
        error: Some(PanelError {
            code: "bad".to_owned(),
            message: "broken".to_owned(),
            line: None,
            column: None,
            snippet: None,
        }),
        file_error: None,
        dropped: None,
        read_only: false,
    }
}

fn fits(panels: &Panels) -> bool {
    DaemonMessage::Panels(Panels {
        request: Some(RequestId(u64::MAX)),
        ..panels.clone()
    })
    .encode()
    .is_ok()
}

/// A 255-byte name of `"` and `\`, the two characters that escape to two bytes; `n` picks which.
fn worst_name(n: usize) -> String {
    let head: String = (0..9)
        .map(|bit| if n >> (8 - bit) & 1 == 1 { '\\' } else { '"' })
        .collect();
    format!("{head}{}", "\"".repeat(246))
}

fn worst_view(space: &SpaceId, n: usize) -> PanelView {
    let quote = |len: usize| "\"".repeat(len);
    let mut v = view(space, &worst_name(2 * n), PanelState::Failed);
    v.file.title = Some(quote(1_024));
    v.file.created_by = Some(CreatedBy::Agent {
        slot: Some(quote(256)),
    });
    v.revision = u64::MAX;
    v.error = Some(PanelError {
        code: "unsupported_diagram".to_owned(),
        message: "\u{1}".repeat(1_024),
        line: Some(u32::MAX),
        column: Some(u32::MAX),
        snippet: Some("\u{1}".repeat(1_024)),
    });
    v
}

fn worst_case() -> (SpaceId, PanelList) {
    let space = SpaceId("\"".repeat(1_024));
    let panels: Vec<PanelView> = (0..100).map(|n| worst_view(&space, n)).collect();
    let skipped = (0..256)
        .map(|n| SkippedPanel {
            panel: worst_name(2 * n + 1),
            reason: "\u{1}".repeat(256),
        })
        .collect();
    (
        space,
        PanelList {
            panels,
            skipped,
            error: None,
        },
    )
}

#[test]
fn a_worst_case_list_is_trimmed_to_one_frame() {
    let (space, list) = worst_case();
    assert_eq!(list.panels[0].id.len(), 255);
    assert_eq!(list.skipped[0].panel.len(), 255);
    let untrimmed = Panels {
        request: Some(RequestId(u64::MAX)),
        space: space.0.clone(),
        panels: list.panels.iter().map(panel_summary).collect(),
        skipped: list.skipped.clone(),
        error: None,
    };
    assert!(!fits(&untrimmed), "the input must be too big for a frame");

    let message = panels_message(Some(RequestId(u64::MAX)), &space, list.clone());
    assert!(DaemonMessage::Panels(message.clone()).encode().is_ok());
    assert!(!message.panels.is_empty());
    assert!(message.panels.len() < 100);
    let kept = message.panels.len();
    for (shown, view) in message.panels.iter().zip(&list.panels) {
        assert_eq!(shown.id, view.id);
    }
    let trimmed: Vec<&SkippedPanel> = message
        .skipped
        .iter()
        .filter(|s| s.reason == TRIMMED_REASON)
        .collect();
    assert_eq!(trimmed.len(), 100 - kept);
    for (skipped, view) in trimmed.iter().zip(&list.panels[kept..]) {
        assert_eq!(skipped.panel, view.id);
    }
    assert!(message.skipped.windows(2).all(|w| w[0].panel <= w[1].panel));
    assert_eq!(message.skipped.len(), 256 + 100 - kept);
    assert_eq!(message.error, None);
    assert_eq!(message.request, Some(RequestId(u64::MAX)));
}

#[test]
fn trimming_keeps_the_largest_prefix() {
    let (space, list) = worst_case();
    let message = panels_message(None, &space, list.clone());
    let kept = message.panels.len();
    assert_eq!(message.request, None);
    // One more panel would not fit.
    let mut more = message.clone();
    more.panels.push(panel_summary(&list.panels[kept]));
    more.skipped.retain(|s| s.panel != list.panels[kept].id);
    assert!(!fits(&more));
}

#[test]
fn trimming_is_deterministic() {
    let (space, list) = worst_case();
    let first = panels_message(Some(RequestId(1)), &space, list.clone());
    let again = panels_message(Some(RequestId(1)), &space, list.clone());
    assert_eq!(first, again);
    let other = panels_message(Some(RequestId(9)), &space, list);
    assert_eq!(
        Panels {
            request: Some(RequestId(1)),
            ..other
        },
        first
    );
}

#[test]
fn skipped_entries_are_dropped_only_when_needed() {
    let space = SpaceId("/work/app".to_owned());
    let skipped = |n: usize| SkippedPanel {
        panel: format!("{n:03}{}", "a".repeat(250)),
        reason: "\u{1}".repeat(256),
    };
    // 256 skipped entries and a few small panels fit one frame untouched.
    let small = PanelList {
        panels: vec![view(&space, "b", PanelState::Validated)],
        skipped: (0..256).map(skipped).collect(),
        error: Some("some folders were unreadable".to_owned()),
    };
    let message = panels_message(None, &space, small.clone());
    assert_eq!(message.skipped, small.skipped);
    assert_eq!(message.panels.len(), 1);
    assert_eq!(message.error, small.error);

    // Skipped entries alone that overflow are cut to the largest prefix, with the error saying so.
    let huge: Vec<SkippedPanel> = (0..2_000)
        .map(|n| SkippedPanel {
            panel: format!("{n:04}{}", "a".repeat(250)),
            reason: "\u{1}".repeat(256),
        })
        .collect();
    let list = PanelList {
        panels: Vec::new(),
        skipped: huge.clone(),
        error: Some("first".to_owned()),
    };
    let message = panels_message(None, &space, list);
    assert!(fits(&message));
    let m = message.skipped.len();
    assert!(m > 0 && m < huge.len());
    assert_eq!(message.skipped, huge[..m]);
    assert_eq!(message.error, Some(format!("first; {SKIPPED_TRIMMED}")));
    let mut one_more = message.clone();
    one_more.skipped.push(huge[m].clone());
    assert!(!fits(&one_more));

    let list = PanelList {
        panels: Vec::new(),
        skipped: huge,
        error: None,
    };
    assert_eq!(
        panels_message(None, &space, list).error,
        Some(SKIPPED_TRIMMED.to_owned())
    );
}

#[test]
fn a_trimmed_panel_sorts_before_a_folder_with_its_id() {
    let (space, mut list) = worst_case();
    let id = list.panels[99].id.clone();
    list.skipped.push(SkippedPanel {
        panel: id.clone(),
        reason: "no panel.json".to_owned(),
    });
    list.skipped.sort_by(|a, b| a.panel.cmp(&b.panel));
    let message = panels_message(None, &space, list);
    let same: Vec<&str> = message
        .skipped
        .iter()
        .filter(|s| s.panel == id)
        .map(|s| s.reason.as_str())
        .collect();
    assert_eq!(same, vec![TRIMMED_REASON, "no panel.json"]);
}

#[test]
fn removed_panels_are_not_listed_and_the_rest_are_sorted() {
    let space = SpaceId("/work/app".to_owned());
    let list = PanelList {
        panels: vec![
            view(&space, "b", PanelState::Validated),
            view(&space, "gone", PanelState::Removed),
            view(&space, "a", PanelState::Failed),
        ],
        skipped: Vec::new(),
        error: None,
    };
    let message = panels_message(Some(RequestId(7)), &space, list);
    let ids: Vec<&str> = message.panels.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "b"]);
    assert_eq!(message.request, Some(RequestId(7)));
    assert!(message.panels[0].error.is_some());
    assert!(message.panels[1].error.is_none());
}

#[test]
fn error_is_set_only_while_failed() {
    let space = SpaceId("/work/app".to_owned());
    for state in [
        PanelState::Validating,
        PanelState::Fetching,
        PanelState::Validated,
        PanelState::ValidatedParserOnly,
        PanelState::Untrusted,
    ] {
        let v = view(&space, "p", state);
        assert_eq!(panel_summary(&v).error, None);
        assert_eq!(panel_reply(RequestId(1), &v, "src").error, None);
    }
    let failed = view(&space, "p", PanelState::Failed);
    assert_eq!(panel_summary(&failed).error, failed.error);
    let reply = panel_reply(RequestId(4), &failed, "src");
    assert_eq!(reply.error, failed.error);
    assert_eq!(
        (
            reply.request,
            reply.space.as_str(),
            reply.id.as_str(),
            reply.revision
        ),
        (RequestId(4), "/work/app", "p", 3)
    );
    assert_eq!(
        (reply.renderer, reply.source.as_str()),
        (Renderer::Markdown, "src")
    );
}

#[test]
fn the_loading_message_has_no_panels() {
    let space = SpaceId("/work/app".to_owned());
    let message = panels_loading(Some(RequestId(2)), &space);
    assert_eq!(message.error.as_deref(), Some(PANELS_LOADING));
    assert!(message.panels.is_empty() && message.skipped.is_empty());
    assert_eq!(message.request, Some(RequestId(2)));
}
