#![cfg(feature = "test-support")]
//! The terminal list: terminals and spaces as `terminals` snapshots report them.

mod common;

use combine_protocol::{
    ClientMessage, DaemonMessage, ErrorCode, RequestId, SpaceInfo, SpaceKind, TermId,
    TerminalState, WatchTerminals,
};
use combined::core::{Core, CoreConfig, TerminalList};
use combined::model::{Millis, SpaceId, TermEvent, TermInfo, TermState};
use combined::ports::Capabilities;
use common::FakeGitProbe;

fn id(text: &str) -> TermId {
    TermId(text.to_string())
}

fn info(term: &str, cwd: &str) -> TermInfo {
    TermInfo {
        id: id(term),
        cwd: cwd.to_string(),
        argv: vec!["sh".to_string()],
        state: TermState::Running,
        labels: Default::default(),
        cols: 80,
        rows: 24,
    }
}

fn list() -> TerminalList {
    TerminalList::new(Box::new(
        FakeGitProbe::default().with_repository("/work/repo"),
    ))
}

fn ids(list: &TerminalList) -> Vec<String> {
    list.snapshot(None)
        .terminals
        .into_iter()
        .map(|terminal| terminal.id.0)
        .collect()
}

#[test]
fn list_then_snapshot_reports_terminals_and_spaces_in_list_order() {
    let mut list = list();
    list.apply_list(&[
        info("fake:b", "/work/repo/src"),
        info("fake:a", "/work/repo"),
        info("fake:c", "/tmp/scratch"),
    ]);
    let snapshot = list.snapshot(Some(RequestId(7)));
    assert_eq!(snapshot.request, Some(RequestId(7)));
    assert_eq!(ids(&list), ["fake:b", "fake:a", "fake:c"]);
    assert_eq!(
        snapshot.spaces,
        [
            SpaceInfo {
                id: "/tmp/scratch".to_string(),
                kind: SpaceKind::Folder,
                terminals: vec![id("fake:c")],
            },
            SpaceInfo {
                id: "/work/repo".to_string(),
                kind: SpaceKind::Repository,
                terminals: vec![id("fake:b"), id("fake:a")],
            },
        ]
    );
    assert_eq!(snapshot.terminals[0].state, TerminalState::Running);
    assert_eq!(snapshot.terminals[0].exit_code, None);
}

#[test]
fn order_is_not_id_order() {
    let mut list = list();
    list.apply_list(&[
        info("fake:t2", "/work/repo"),
        info("fake:t10", "/work/repo"),
        info("fake:t1", "/work/repo"),
    ]);
    assert_eq!(ids(&list), ["fake:t2", "fake:t10", "fake:t1"]);
    assert_eq!(
        list.snapshot(None).spaces[0].terminals,
        [id("fake:t2"), id("fake:t10"), id("fake:t1")]
    );
}

#[test]
fn created_for_known_id_replaces_in_place() {
    let mut list = list();
    list.apply_list(&[
        info("fake:b", "/work/repo"),
        info("fake:a", "/work/repo"),
        info("fake:c", "/work/repo"),
    ]);
    let mut again = info("fake:a", "/work/repo");
    again.cols = 100;
    list.apply_event(&TermEvent::Created(again));
    let snapshot = list.snapshot(None);
    assert_eq!(ids(&list), ["fake:b", "fake:a", "fake:c"]);
    assert_eq!(snapshot.terminals[1].cols, 100);
    assert_eq!(
        snapshot.spaces[0].terminals,
        [id("fake:b"), id("fake:a"), id("fake:c")]
    );
}

#[test]
fn changed_keeps_place_and_unknown_changed_is_appended() {
    let mut list = list();
    list.apply_list(&[info("fake:b", "/work/repo"), info("fake:a", "/work/repo")]);
    let mut resized = info("fake:b", "/work/repo");
    resized.cols = 90;
    list.apply_event(&TermEvent::Changed(resized));
    list.apply_event(&TermEvent::Changed(info("fake:0", "/work/repo")));
    let snapshot = list.snapshot(None);
    assert_eq!(ids(&list), ["fake:b", "fake:a", "fake:0"]);
    assert_eq!(snapshot.terminals[0].cols, 90);
    assert_eq!(
        snapshot.spaces[0].terminals,
        [id("fake:b"), id("fake:a"), id("fake:0")]
    );
}

#[test]
fn exited_keeps_place_lost_removes_restarted_changes_nothing() {
    let mut list = list();
    list.apply_list(&[
        info("fake:c", "/work/repo"),
        info("fake:a", "/work/repo"),
        info("fake:b", "/work/repo"),
    ]);
    list.take_changed();
    list.apply_event(&TermEvent::Exited {
        id: id("fake:a"),
        exit_code: Some(1),
    });
    assert_eq!(ids(&list), ["fake:c", "fake:a", "fake:b"]);
    assert_eq!(
        list.snapshot(None).terminals[1].state,
        TerminalState::Exited
    );
    assert!(list.take_changed());
    list.apply_event(&TermEvent::Restarted { id: id("fake:c") });
    assert!(!list.take_changed());
    assert_eq!(ids(&list), ["fake:c", "fake:a", "fake:b"]);
    list.apply_event(&TermEvent::Lost { id: id("fake:a") });
    assert_eq!(ids(&list), ["fake:c", "fake:b"]);
    assert_eq!(
        list.snapshot(None).spaces[0].terminals,
        [id("fake:c"), id("fake:b")]
    );
}

#[test]
fn recreated_after_lost_is_appended() {
    let mut list = list();
    list.apply_list(&[info("fake:b", "/work/repo"), info("fake:a", "/work/repo")]);
    list.apply_event(&TermEvent::Lost { id: id("fake:b") });
    list.apply_event(&TermEvent::Created(info("fake:b", "/work/repo")));
    assert_eq!(ids(&list), ["fake:a", "fake:b"]);
    assert_eq!(
        list.snapshot(None).spaces[0].terminals,
        [id("fake:a"), id("fake:b")]
    );
}

#[test]
fn duplicate_ids_in_a_list_keep_the_first() {
    let mut list = list();
    let mut second = info("fake:a", "/tmp/scratch");
    second.cols = 111;
    list.apply_list(&[
        info("fake:a", "/work/repo"),
        info("fake:b", "/work/repo"),
        second,
    ]);
    let snapshot = list.snapshot(None);
    assert_eq!(ids(&list), ["fake:a", "fake:b"]);
    assert_eq!(snapshot.terminals[0].cols, 80);
    assert_eq!(snapshot.spaces.len(), 1);
    assert_eq!(snapshot.spaces[0].id, "/work/repo");
    assert_eq!(snapshot.spaces[0].terminals, [id("fake:a"), id("fake:b")]);
}

#[test]
fn space_listed_tracks_placements() {
    let mut list = list();
    let repo = SpaceId("/work/repo".to_string());
    assert!(!list.space_listed(&repo));
    list.apply_list(&[info("fake:1", "/work/repo/src")]);
    assert!(list.space_listed(&repo));
    assert!(!list.space_listed(&SpaceId("/elsewhere".to_string())));
    list.apply_event(&TermEvent::Lost { id: id("fake:1") });
    assert!(!list.space_listed(&repo));
    list.apply_event(&TermEvent::Created(info("fake:2", "/work/repo")));
    assert!(list.space_listed(&repo));
}

#[test]
fn created_and_changed_events_update_the_terminal() {
    let mut list = list();
    list.apply_event(&TermEvent::Created(info("fake:1", "/work/repo")));
    assert_eq!(ids(&list), ["fake:1"]);
    let mut resized = info("fake:1", "/work/repo");
    resized.cols = 120;
    list.apply_event(&TermEvent::Changed(resized));
    let snapshot = list.snapshot(None);
    assert_eq!(snapshot.terminals.len(), 1);
    assert_eq!(snapshot.terminals[0].cols, 120);
}

#[test]
fn exited_event_keeps_the_terminal_with_its_exit_code() {
    let mut list = list();
    list.apply_list(&[info("fake:1", "/work/repo")]);
    list.apply_event(&TermEvent::Exited {
        id: id("fake:1"),
        exit_code: Some(3),
    });
    let snapshot = list.snapshot(None);
    assert_eq!(snapshot.terminals[0].state, TerminalState::Exited);
    assert_eq!(snapshot.terminals[0].exit_code, Some(3));
    assert_eq!(snapshot.spaces[0].terminals, [id("fake:1")]);
}

#[test]
fn lost_event_removes_the_terminal_and_empties_its_space() {
    let mut list = list();
    list.apply_list(&[info("fake:1", "/work/repo")]);
    list.apply_event(&TermEvent::Lost { id: id("fake:1") });
    let snapshot = list.snapshot(None);
    assert!(snapshot.terminals.is_empty());
    assert!(snapshot.spaces.is_empty());
}

#[test]
fn restarted_event_changes_nothing() {
    let mut list = list();
    list.apply_list(&[info("fake:1", "/work/repo")]);
    assert!(list.take_changed());
    let before = list.snapshot(None);
    list.apply_event(&TermEvent::Restarted { id: id("fake:1") });
    assert!(!list.take_changed());
    assert_eq!(list.snapshot(None), before);
}

#[test]
fn take_changed_reports_each_change_once() {
    let mut list = list();
    assert!(!list.take_changed());
    list.apply_list(&[]);
    assert!(list.take_changed());
    assert!(!list.take_changed());
    list.apply_event(&TermEvent::Created(info("fake:1", "/work/repo")));
    list.apply_event(&TermEvent::Lost { id: id("fake:1") });
    assert!(list.take_changed());
    assert!(!list.take_changed());
}

#[test]
fn core_answers_watch_terminals_with_invalid_request() {
    let capabilities = Capabilities {
        ordered_attach: true,
        byte_credit: false,
        initial_credit: 0,
        display_queries: true,
        automatic_replies: true,
        resync_signal: true,
        agent_state: false,
    };
    let mut core = Core::new(CoreConfig::default(), capabilities).unwrap();
    let connection = core.connect();
    core.client_message(
        connection,
        ClientMessage::WatchTerminals(WatchTerminals {
            request: RequestId(5),
        }),
        Millis(0),
    );
    let messages = core.pull(connection, usize::MAX, Millis(0));
    assert_eq!(messages.len(), 1);
    match &messages[0] {
        DaemonMessage::Error(error) => {
            assert_eq!(error.code, ErrorCode::InvalidRequest);
            assert_eq!(error.message, "unexpected watch_terminals");
            assert_eq!(error.request, Some(RequestId(5)));
            assert_eq!(error.attachment, None);
        }
        other => panic!("not an error: {other:?}"),
    }
}
