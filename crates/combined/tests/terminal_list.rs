#![cfg(feature = "test-support")]
//! The terminal list: terminals and spaces as `terminals` snapshots report them.

mod common;

use combine_protocol::{
    ClientMessage, DaemonMessage, ErrorCode, RequestId, SpaceInfo, SpaceKind, TermId,
    TerminalState, WatchTerminals,
};
use combined::core::{Core, CoreConfig, TerminalList};
use combined::model::{Millis, TermEvent, TermInfo, TermState};
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
fn list_then_snapshot_reports_terminals_and_spaces_sorted() {
    let mut list = list();
    list.apply_list(&[
        info("fake:b", "/work/repo/src"),
        info("fake:a", "/work/repo"),
        info("fake:c", "/tmp/scratch"),
    ]);
    let snapshot = list.snapshot(Some(RequestId(7)));
    assert_eq!(snapshot.request, Some(RequestId(7)));
    assert_eq!(ids(&list), ["fake:a", "fake:b", "fake:c"]);
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
                terminals: vec![id("fake:a"), id("fake:b")],
            },
        ]
    );
    assert_eq!(snapshot.terminals[0].state, TerminalState::Running);
    assert_eq!(snapshot.terminals[0].exit_code, None);
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
            assert_eq!(error.request, Some(RequestId(5)));
            assert_eq!(error.attachment, None);
        }
        other => panic!("not an error: {other:?}"),
    }
}
