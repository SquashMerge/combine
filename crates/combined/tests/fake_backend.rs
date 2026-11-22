#![cfg(feature = "test-support")]
//! The fake TerminalBackend: what it simulates and injects.

mod common;

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::Context;

use combine_protocol::{BackendGeneration, CloseReason, Fidelity, OwnershipGeneration, TermId};
use combined::model::{SpaceId, TermEvent};
use combined::ports::{
    AttachEvent, AttachEvents, AttachOptions, Attachment, BackendError, Capabilities, FileChange,
    FileStore, StoreError, StoreRevision, TerminalBackend, WriteOutcome,
};
use combined::testing::{
    FakeAttachmentRef, FakeBackend, FakeCall, FakeConfig, FakeControl, Half, MemoryFileStore,
    Mutation, ResyncMode, FAKE_PAINT_END, FAKE_PAINT_START,
};
use common::{display, request_options, spawn_spec};
use futures::executor::block_on;
use futures::task::{waker, ArcWake};
use futures::{FutureExt, StreamExt};

const OWN: OwnershipGeneration = OwnershipGeneration(1);

type Seen = Vec<(u64, AttachEvent)>;

fn fake(config: FakeConfig) -> (Arc<FakeBackend>, FakeControl) {
    FakeBackend::new(config)
}

fn spawn(backend: &FakeBackend) -> TermId {
    block_on(backend.spawn(spawn_spec("/work", 80, 24))).unwrap()
}

fn attach_sized(
    backend: &FakeBackend,
    term: &TermId,
    cols: u16,
    rows: u16,
    observe: bool,
) -> Box<dyn Attachment> {
    let options = AttachOptions::from(request_options(cols, rows, observe));
    block_on(backend.attach(term, options)).unwrap()
}

fn attach(backend: &FakeBackend, term: &TermId) -> Box<dyn Attachment> {
    attach_sized(backend, term, 100, 30, false)
}

fn drain(events: &mut AttachEvents) -> Seen {
    let mut seen = Vec::new();
    while let Some(Some((generation, event))) = events.next().now_or_never() {
        seen.push((generation.0, event));
    }
    seen
}

fn has_ended(events: &mut AttachEvents) -> bool {
    matches!(events.next().now_or_never(), Some(None))
}

fn is_paint(bytes: &[u8]) -> bool {
    bytes.starts_with(FAKE_PAINT_START) && bytes.ends_with(FAKE_PAINT_END)
}

fn paint_text(bytes: &[u8]) -> String {
    let inner = &bytes[FAKE_PAINT_START.len()..bytes.len() - FAKE_PAINT_END.len()];
    String::from_utf8(inner.to_vec()).unwrap()
}

/// The program's output: every `Data` that is not a paint, joined.
fn program_output(seen: &Seen) -> Vec<u8> {
    seen.iter()
        .filter_map(|(_, event)| match event {
            AttachEvent::Data(bytes) if !is_paint(bytes) => Some(bytes.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn paints(seen: &Seen) -> Vec<String> {
    seen.iter()
        .filter_map(|(_, event)| match event {
            AttachEvent::Data(bytes) if is_paint(bytes) => Some(paint_text(bytes)),
            _ => None,
        })
        .collect()
}

fn started_count(seen: &Seen) -> usize {
    seen.iter()
        .filter(|(_, event)| matches!(event, AttachEvent::Started { .. }))
        .count()
}

fn resized(cols: u16, rows: u16) -> AttachEvent {
    AttachEvent::Resized { cols, rows }
}

#[test]
fn attach_opens_display_then_input_and_starts_once() {
    let (backend, control) = fake(FakeConfig::default());
    assert_eq!(backend.id(), "fake");
    let term = spawn(&backend);
    assert_eq!(term, TermId("fake:t1".to_string()));
    let mut att = attach(&backend, &term);
    assert_eq!(control.open_halves(&term), 2);
    assert_eq!(control.attachments(&term), vec![FakeAttachmentRef(1)]);

    let mut events = att.events();
    let seen = drain(&mut events);
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[0],
        (
            1,
            AttachEvent::Started {
                cols: 80,
                rows: 24,
                fidelity: Fidelity::Exact,
                ordered_resizes: true,
            }
        )
    );
    assert!(matches!(&seen[1], (1, AttachEvent::Data(bytes)) if is_paint(bytes)));
    assert_eq!(paints(&seen), vec!["screen 80x24 exact after 0 bytes"]);

    control.emit(&term, b"hello");
    assert_eq!(started_count(&drain(&mut events)), 0);
    assert_eq!(control.pty_size(&term), Some((80, 24)));
    assert_eq!(att.fidelity(), Fidelity::Exact);
    assert!(att.ordered_resizes());
    assert_eq!(
        control.requested(FakeAttachmentRef(1)),
        ((100, 30), display())
    );
    assert!(matches!(
        control.calls().as_slice(),
        [
            FakeCall::Spawn { .. },
            FakeCall::Attach {
                att: FakeAttachmentRef(1),
                ..
            }
        ]
    ));
}

#[test]
fn watch_only_has_no_input_half() {
    let (backend, control) = fake(FakeConfig::default());
    let term = spawn(&backend);
    let mut att = attach_sized(&backend, &term, 100, 30, true);
    assert_eq!(control.open_halves(&term), 1);

    block_on(async {
        att.write(OWN, b"typed").await.unwrap();
        att.write_automatic(BackendGeneration(1), b"reply")
            .await
            .unwrap();
        att.resize(OWN, 120, 40).await.unwrap();
        att.focus(OWN).await.unwrap();
    });
    assert!(control.typed_input(&term).is_empty());
    assert!(control.automatic_input(&term).is_empty());
    assert_eq!(control.pty_size(&term), Some((80, 24)));
    assert_eq!(
        control.requested(FakeAttachmentRef(1)),
        ((100, 30), display())
    );
    assert_eq!(control.calls().len(), 6);

    let mut events = att.events();
    let first = drain(&mut events);
    assert_eq!(started_count(&first), 1);
    block_on(att.credit(10)).unwrap();
}

#[test]
fn remote_twin_is_filtered_from_list_watch_and_calls() {
    let (backend, control) = fake(FakeConfig::default());
    let mut watch = block_on(backend.watch()).unwrap();
    let term = spawn(&backend);
    let twin = control.remote_twin(&term);
    assert_eq!(twin, TermId("fake:work/t1".to_string()));
    control.set_cwd(&twin, "/elsewhere");
    control.set_cwd(&term, "/here");
    control.exit(&twin, Some(0));
    control.restart_backend();

    let listed = block_on(backend.list()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, term);
    assert_eq!(listed[0].cwd, "/here");

    let mut events = Vec::new();
    while let Some(Some(event)) = watch.next().now_or_never() {
        events.push(event);
    }
    assert_eq!(events.len(), 3);
    assert!(matches!(&events[0], TermEvent::Created(info) if info.id == term));
    assert!(matches!(&events[1], TermEvent::Changed(info) if info.cwd == "/here"));
    assert!(matches!(&events[2], TermEvent::Restarted { id } if *id == term));
    assert!(control.filtered_events() > 0);

    for remote in [&twin, &TermId("fake:work/t9".to_string())] {
        let options = AttachOptions::from(request_options(80, 24, false));
        assert!(matches!(
            block_on(backend.attach(remote, options)),
            Err(BackendError::RemoteTerminal(_))
        ));
        assert!(matches!(
            block_on(backend.kill(remote)),
            Err(BackendError::RemoteTerminal(_))
        ));
    }
    for unknown in ["fake:nope", "t1", "other:t1", "fake:"] {
        let id = TermId(unknown.to_string());
        let options = AttachOptions::from(request_options(80, 24, false));
        assert!(matches!(
            block_on(backend.attach(&id, options)),
            Err(BackendError::UnknownTerminal(_))
        ));
        assert!(matches!(
            block_on(backend.kill(&id)),
            Err(BackendError::UnknownTerminal(_))
        ));
    }
}

#[test]
fn delayed_output_and_boundary_release_in_order() {
    let (backend, control) = fake(FakeConfig::default());
    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    let mut events = att.events();
    drain(&mut events);

    control.delay_output(&term);
    control.emit(&term, b"old");
    block_on(att.focus(OWN)).unwrap();
    assert_eq!(control.pty_size(&term), Some((100, 30)));
    assert!(drain(&mut events).is_empty());
    assert!(control.pending_output(&term) > 0);

    control.release_output(&term);
    let seen = drain(&mut events);
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0], (1, AttachEvent::Data(b"old".to_vec())));
    assert_eq!(seen[1], (1, resized(100, 30)));
    assert!(matches!(&seen[2], (1, AttachEvent::Data(bytes)) if is_paint(bytes)));
    assert_eq!(paints(&seen), vec!["screen 100x30 exact after 3 bytes"]);
    assert_eq!(control.pending_output(&term), 0);
}

fn overflowing(resync: ResyncMode) -> FakeConfig {
    FakeConfig {
        resync,
        backend_queue_limit: 100,
        chunk_size: 50,
        ..FakeConfig::default()
    }
}

#[test]
fn resync_signal_mode_emits_resynced_then_paint() {
    let (backend, control) = fake(overflowing(ResyncMode::Signal));
    assert!(backend.capabilities().resync_signal);
    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    let mut events = att.events();
    drain(&mut events);

    control.delay_output(&term);
    control.emit(&term, &[b'x'; 300]);
    control.release_output(&term);
    let seen = drain(&mut events);
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0], (1, AttachEvent::Resynced));
    assert!(matches!(&seen[1], (1, AttachEvent::Data(bytes)) if is_paint(bytes)));
    assert!(program_output(&seen).is_empty());
    assert_eq!(paints(&seen), vec!["screen 80x24 exact after 300 bytes"]);

    control.force_resync(FakeAttachmentRef(1));
    let seen = drain(&mut events);
    assert_eq!(seen[0], (1, AttachEvent::Resynced));
    assert_eq!(paints(&seen).len(), 1);
}

#[test]
fn silent_mode_repaints_in_band() {
    let (backend, control) = fake(overflowing(ResyncMode::Silent));
    assert!(!backend.capabilities().resync_signal);
    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    let mut events = att.events();
    drain(&mut events);

    control.delay_output(&term);
    control.emit(&term, &[b'x'; 300]);
    control.release_output(&term);
    let seen = drain(&mut events);
    assert_eq!(seen.len(), 1);
    assert!(matches!(&seen[0], (1, AttachEvent::Data(bytes)) if is_paint(bytes)));

    control.force_resync(FakeAttachmentRef(1));
    let seen = drain(&mut events);
    assert!(!seen
        .iter()
        .any(|(_, event)| *event == AttachEvent::Resynced));
    assert_eq!(paints(&seen).len(), 1);
}

#[test]
fn credit_limits_output_and_initial_allowance() {
    assert_eq!(FakeConfig::default().capabilities.initial_credit, 262_144);
    let mut config = FakeConfig::default();
    config.capabilities.initial_credit = 1_000;
    let (backend, control) = fake(config);
    assert_eq!(backend.capabilities().initial_credit, 1_000);
    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    let mut events = att.events();
    let first = drain(&mut events);
    let paint_len: usize = first
        .iter()
        .map(|(_, event)| match event {
            AttachEvent::Data(bytes) => bytes.len(),
            _ => 0,
        })
        .sum();

    control.emit(&term, &[b'a'; 2_000]);
    let seen = drain(&mut events);
    assert_eq!(program_output(&seen).len(), 1_000 - paint_len);
    assert_eq!(control.pending_output(&term), 1_000 + paint_len);

    block_on(att.credit(500)).unwrap();
    assert_eq!(program_output(&drain(&mut events)).len(), 500);
    block_on(att.credit(5_000)).unwrap();
    assert_eq!(program_output(&drain(&mut events)).len(), 500 + paint_len);
    assert_eq!(control.pending_output(&term), 0);
    assert!(matches!(
        control.calls().last(),
        Some(FakeCall::Credit { bytes: 5_000, .. })
    ));
}

#[test]
fn half_failure_discards_undelivered_output() {
    for half in [Half::Display, Half::Input] {
        let (backend, control) = fake(FakeConfig::default());
        let term = spawn(&backend);
        let mut att = attach(&backend, &term);
        let mut events = att.events();

        control.delay_output(&term);
        control.emit(&term, b"undelivered");
        control.fail_half(FakeAttachmentRef(1), half);
        control.release_output(&term);
        assert_eq!(control.open_halves(&term), 0);

        let seen = drain(&mut events);
        assert_eq!(started_count(&seen), 1);
        assert!(program_output(&seen).is_empty());
        assert!(!has_ended(&mut events));
    }
}

#[test]
fn half_failures_rebuild_from_latest_requested_size() {
    let (backend, control) = fake(FakeConfig::default());
    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    let mut events = att.events();
    drain(&mut events);
    block_on(att.focus(OWN)).unwrap();
    assert_eq!(control.pty_size(&term), Some((100, 30)));
    drain(&mut events);

    control.fail_half(FakeAttachmentRef(1), Half::Input);
    assert_eq!(control.pty_size(&term), Some((200, 120)));
    block_on(async {
        att.resize(OWN, 120, 40).await.unwrap();
        att.set_display(OWN, display()).await.unwrap();
        att.focus(OWN).await.unwrap();
        att.write(OWN, b"ls").await.unwrap();
    });
    assert_eq!(control.pty_size(&term), Some((200, 120)));
    assert!(control.typed_input(&term).is_empty());

    assert!(control.step());
    assert_eq!(control.pty_size(&term), Some((120, 40)));
    assert_eq!(control.typed_input(&term), b"ls");
    assert_eq!(control.open_halves(&term), 2);
    assert_eq!(
        control.requested(FakeAttachmentRef(1)),
        ((120, 40), display())
    );
    assert!(!control.step());

    let seen = drain(&mut events);
    assert!(matches!(
        seen[0],
        (
            2,
            AttachEvent::Started {
                cols: 200,
                rows: 120,
                ..
            }
        )
    ));
    assert!(seen.iter().all(|(generation, _)| *generation == 2));
    assert!(seen.contains(&(2, AttachEvent::Facts(display()))));
    assert!(seen.contains(&(2, resized(120, 40))));
    assert_eq!(program_output(&seen), b"ls");
}

#[test]
fn opening_failure_halfway_tears_down() {
    let (backend, control) = fake(FakeConfig::default());
    let term = spawn(&backend);
    control.fail_next_opens(&term, 1);
    let options = AttachOptions::from(request_options(80, 24, false));
    assert!(matches!(
        block_on(backend.attach(&term, options)),
        Err(BackendError::Failed(_))
    ));
    assert_eq!(control.open_halves(&term), 0);
    assert!(control.attachments(&term).is_empty());
    assert!(matches!(
        control.calls().last(),
        Some(FakeCall::Attach { .. })
    ));
    let mut att = attach(&backend, &term);
    assert_eq!(control.open_halves(&term), 2);

    let mut events = att.events();
    drain(&mut events);
    control.fail_half(FakeAttachmentRef(2), Half::Display);
    control.fail_next_opens(&term, 3);
    assert!(control.step());
    assert!(control.step());
    assert_eq!(control.open_halves(&term), 0);
    assert!(drain(&mut events).is_empty());
    assert!(control.step());
    assert_eq!(drain(&mut events), vec![(1, AttachEvent::Lost)]);
    assert!(has_ended(&mut events));
    assert_eq!(block_on(att.write(OWN, b"x")), Err(BackendError::Closed));
    assert!(!control.step());
}

#[test]
fn restart_and_crash_bump_generation() {
    let (backend, control) = fake(FakeConfig::default());
    let term = spawn(&backend);
    let mut watch = block_on(backend.watch()).unwrap();
    let mut att = attach(&backend, &term);
    let mut events = att.events();
    drain(&mut events);

    control.restart_backend();
    assert_eq!(control.open_halves(&term), 0);
    assert!(matches!(
        watch.next().now_or_never(),
        Some(Some(TermEvent::Restarted { .. }))
    ));
    assert!(control.step());
    let seen = drain(&mut events);
    assert!(matches!(
        seen[0],
        (
            2,
            AttachEvent::Started {
                fidelity: Fidelity::Exact,
                ..
            }
        )
    ));
    assert_eq!(att.fidelity(), Fidelity::Exact);

    control.crash_backend();
    assert!(control.step());
    let seen = drain(&mut events);
    assert!(matches!(
        seen[0],
        (
            3,
            AttachEvent::Started {
                fidelity: Fidelity::Inexact,
                ..
            }
        )
    ));
    assert_eq!(att.fidelity(), Fidelity::Inexact);
    assert!(paints(&seen)[0].contains("inexact"));
    control.set_fidelity(&term, Fidelity::Exact);
    control.fail_half(FakeAttachmentRef(1), Half::Input);
    control.step();
    assert_eq!(att.fidelity(), Fidelity::Inexact);

    let other = spawn(&backend);
    let mut second = attach(&backend, &other);
    assert_eq!(second.fidelity(), Fidelity::Inexact);
    control.set_fidelity(&term, Fidelity::Exact);
    drop(second.events());
}

#[test]
fn late_old_generation_events_follow_the_new_started() {
    let (backend, control) = fake(FakeConfig::default());
    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    let mut events = att.events();
    drain(&mut events);

    control.set_late_old_generation_events(true);
    control.fail_half(FakeAttachmentRef(1), Half::Display);
    control.step();
    let seen = drain(&mut events);
    assert_eq!(seen.len(), 4);
    assert!(matches!(seen[0], (2, AttachEvent::Started { .. })));
    assert!(matches!(&seen[1], (2, AttachEvent::Data(bytes)) if is_paint(bytes)));
    assert_eq!(seen[2], (1, AttachEvent::Data(b"<late>".to_vec())));
    assert_eq!(seen[3], (1, resized(80, 24)));
}

#[test]
fn reopen_queue_is_bounded() {
    let config = FakeConfig {
        reopen_queue_limit: 4,
        reopen_input_limit: 8,
        ..FakeConfig::default()
    };
    let (backend, control) = fake(config);
    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    control.fail_half(FakeAttachmentRef(1), Half::Display);

    block_on(async {
        att.write(OWN, b"abcde").await.unwrap();
        assert_eq!(
            att.write(OWN, b"fghij").await,
            Err(BackendError::Failed("reopen queue full".to_string()))
        );
        att.write(OWN, b"fgh").await.unwrap();
        att.write_automatic(BackendGeneration(1), b"dropped")
            .await
            .unwrap();
        att.credit(10).await.unwrap();
        att.resize(OWN, 90, 20).await.unwrap();
        att.focus(OWN).await.unwrap();
        assert_eq!(
            att.set_display(OWN, display()).await,
            Err(BackendError::Failed("reopen queue full".to_string()))
        );
    });
    assert!(control.step());
    assert_eq!(control.typed_input(&term), b"abcdefgh");
    assert!(control.automatic_input(&term).is_empty());
    assert_eq!(control.pty_size(&term), Some((90, 20)));
}

fn bytes_of(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8 + 1).collect()
}

fn with_mutation(mutation: Mutation) -> (Arc<FakeBackend>, FakeControl) {
    fake(FakeConfig {
        mutation: Some(mutation),
        ..FakeConfig::default()
    })
}

#[test]
fn mutations_change_the_stream() {
    // DropResized: only the first Resized is lost.
    let (backend, control) = with_mutation(Mutation::DropResized);
    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    let mut events = att.events();
    drain(&mut events);
    block_on(att.focus(OWN)).unwrap();
    assert_eq!(control.pty_size(&term), Some((100, 30)));
    let seen = drain(&mut events);
    assert!(!seen
        .iter()
        .any(|(_, e)| matches!(e, AttachEvent::Resized { .. })));
    assert_eq!(paints(&seen).len(), 1);
    block_on(att.resize(OWN, 90, 20)).unwrap();
    assert!(drain(&mut events).contains(&(1, resized(90, 20))));

    // MoveResized: the first Resized overtakes the latest Data still waiting.
    let (backend, control) = with_mutation(Mutation::MoveResized);
    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    let mut events = att.events();
    drain(&mut events);
    control.delay_output(&term);
    control.emit(&term, b"old");
    block_on(att.focus(OWN)).unwrap();
    control.release_output(&term);
    let seen = drain(&mut events);
    assert_eq!(seen[0], (1, resized(100, 30)));
    assert_eq!(seen[1], (1, AttachEvent::Data(b"old".to_vec())));
    block_on(att.resize(OWN, 90, 20)).unwrap();
    assert_eq!(drain(&mut events)[0], (1, resized(90, 20)));

    // MoveResized with nothing waiting is sent in place, and stays armed.
    let (backend, control) = with_mutation(Mutation::MoveResized);
    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    let mut events = att.events();
    drain(&mut events);
    block_on(att.focus(OWN)).unwrap();
    assert_eq!(drain(&mut events)[0], (1, resized(100, 30)));
    control.delay_output(&term);
    control.emit(&term, b"later");
    block_on(att.resize(OWN, 90, 20)).unwrap();
    control.release_output(&term);
    assert_eq!(drain(&mut events)[0], (1, resized(90, 20)));

    // DropByte: the middle byte of the first program-output Data of 1,024 bytes or more.
    let (backend, control) = with_mutation(Mutation::DropByte);
    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    let mut events = att.events();
    let first = drain(&mut events);
    assert_eq!(paints(&first), vec!["screen 80x24 exact after 0 bytes"]);
    control.emit(&term, &bytes_of(1_023));
    control.emit(&term, b"marker");
    assert_eq!(
        program_output(&drain(&mut events)),
        [bytes_of(1_023), b"marker".to_vec()].concat()
    );
    let mut expected = bytes_of(1_024);
    expected.remove(512);
    control.emit(&term, &bytes_of(1_024));
    assert_eq!(program_output(&drain(&mut events)), expected);
    control.emit(&term, &bytes_of(2_000));
    assert_eq!(program_output(&drain(&mut events)), bytes_of(2_000));

    // Once per attachment: a second one is mutated in turn.
    let mut other = attach(&backend, &term);
    let mut other_events = other.events();
    drain(&mut other_events);
    control.emit(&term, &bytes_of(1_500));
    let mut expected = bytes_of(1_500);
    expected.remove(750);
    assert_eq!(program_output(&drain(&mut other_events)), expected);
    assert_eq!(program_output(&drain(&mut events)), bytes_of(1_500));
}

#[test]
fn events_can_be_taken_once() {
    let (backend, _control) = fake(FakeConfig::default());
    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    let mut first = att.events();
    let mut second = att.events();
    assert!(has_ended(&mut second));
    assert_eq!(started_count(&drain(&mut first)), 1);
}

#[test]
fn last_active_leaving_returns_pty_to_headless_size() {
    let (backend, control) = fake(FakeConfig::default());
    let term = spawn(&backend);
    let mut a = attach_sized(&backend, &term, 100, 30, false);
    let mut b = attach_sized(&backend, &term, 90, 20, false);
    let mut a_events = a.events();
    let mut b_events = b.events();
    drain(&mut a_events);
    drain(&mut b_events);
    assert_eq!(control.pty_size(&term), Some((80, 24)));

    block_on(a.focus(OWN)).unwrap();
    assert_eq!(control.pty_size(&term), Some((100, 30)));
    assert!(drain(&mut b_events).contains(&(1, resized(100, 30))));
    block_on(b.resize(OWN, 70, 10)).unwrap();
    assert_eq!(control.pty_size(&term), Some((100, 30)));

    block_on(b.detach()).unwrap();
    assert_eq!(control.pty_size(&term), Some((100, 30)));
    assert_eq!(
        drain(&mut b_events).last(),
        Some(&(1, AttachEvent::Closed(CloseReason::Detached)))
    );
    assert!(has_ended(&mut b_events));

    drop(a);
    assert_eq!(control.pty_size(&term), Some((200, 120)));
    let seen = drain(&mut a_events);
    assert!(!seen
        .iter()
        .any(|(_, event)| matches!(event, AttachEvent::Closed(_))));
    assert!(has_ended(&mut a_events));
    assert_eq!(
        control.attachments(&term),
        vec![FakeAttachmentRef(1), FakeAttachmentRef(2)]
    );
    assert_eq!(control.open_halves(&term), 0);
}

#[test]
fn spawn_kill_and_calls_follow_the_rules() {
    let (backend, control) = fake(FakeConfig::default());
    assert!(matches!(
        block_on(backend.spawn(spawn_spec("/work", 0, 24))),
        Err(BackendError::InvalidRequest(_))
    ));
    assert!(matches!(
        block_on(backend.spawn(spawn_spec("/work", 80, 0))),
        Err(BackendError::InvalidRequest(_))
    ));
    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    let mut events = att.events();
    drain(&mut events);

    block_on(async {
        att.write(OWN, b"echo me").await.unwrap();
        att.write_automatic(BackendGeneration(1), b"\x1b[0n")
            .await
            .unwrap();
        att.set_display(OWN, display()).await.unwrap();
    });
    assert_eq!(control.typed_input(&term), b"echo me");
    assert_eq!(control.automatic_input(&term), vec![b"\x1b[0n".to_vec()]);
    let seen = drain(&mut events);
    assert_eq!(program_output(&seen), b"echo me");
    assert!(seen.contains(&(1, AttachEvent::Facts(display()))));
    assert_eq!(control.pty_size(&term), Some((100, 30)));

    block_on(backend.kill(&term)).unwrap();
    assert_eq!(
        drain(&mut events),
        vec![(1, AttachEvent::Closed(CloseReason::Exited))]
    );
    assert!(has_ended(&mut events));
    assert_eq!(block_on(att.focus(OWN)), Err(BackendError::Closed));
    assert!(block_on(backend.list()).unwrap().is_empty());
    assert!(matches!(
        block_on(backend.kill(&term)),
        Err(BackendError::UnknownTerminal(_))
    ));
    assert!(control
        .calls()
        .contains(&FakeCall::Kill { term: term.clone() }));
    assert!(control.calls().contains(&FakeCall::Focus {
        att: FakeAttachmentRef(1)
    }));
}

#[test]
fn capabilities_gate_facts_and_automatic_replies() {
    let mut config = FakeConfig::default();
    config.capabilities = Capabilities {
        display_queries: false,
        automatic_replies: false,
        byte_credit: false,
        ..config.capabilities
    };
    let (backend, control) = fake(config);
    let defaults = FakeConfig::default().capabilities;
    assert!(defaults.ordered_attach && defaults.byte_credit && defaults.display_queries);
    assert!(defaults.automatic_replies && defaults.resync_signal && !defaults.agent_state);

    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    let mut events = att.events();
    drain(&mut events);
    block_on(async {
        att.set_display(OWN, display()).await.unwrap();
        att.write_automatic(BackendGeneration(1), b"reply")
            .await
            .unwrap();
        att.credit(1).await.unwrap();
    });
    control.emit(&term, &[b'z'; 600_000]);
    let seen = drain(&mut events);
    assert!(!seen.iter().any(|(_, e)| matches!(e, AttachEvent::Facts(_))));
    assert!(control.automatic_input(&term).is_empty());
    assert_eq!(program_output(&seen).len(), 600_000);
    assert!(control.calls().contains(&FakeCall::WriteAutomatic {
        att: FakeAttachmentRef(1),
        bytes: b"reply".to_vec()
    }));
}

#[test]
fn an_exited_terminal_with_credit_blocked_output_still_finishes() {
    let (backend, control) = fake(FakeConfig::default());
    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    let mut events = att.events();
    drain(&mut events);
    let allowance = FakeConfig::default().capabilities.initial_credit as usize;

    control.emit(&term, &vec![b'q'; allowance + 1_000]);
    let before = drain(&mut events);
    let waiting = control.pending_output(&term);
    assert!(waiting > 0);
    assert_eq!(program_output(&before).len() + waiting, allowance + 1_000);

    control.exit(&term, Some(0));
    let after_exit = drain(&mut events);
    assert!(after_exit.is_empty());
    assert!(!has_ended(&mut events));
    assert_eq!(
        block_on(att.write(OWN, b"late")),
        Err(BackendError::Closed),
        "a program that ended takes no input"
    );

    let half = u32::try_from(waiting / 2).unwrap();
    block_on(att.credit(half)).unwrap();
    let first = drain(&mut events);
    assert_eq!(program_output(&first).len(), waiting / 2);
    assert!(!first
        .iter()
        .any(|(_, e)| matches!(e, AttachEvent::Closed(_))));

    block_on(att.credit(half + 10)).unwrap();
    let rest = drain(&mut events);
    assert_eq!(program_output(&rest).len(), waiting - waiting / 2);
    let closed: Vec<_> = rest
        .iter()
        .filter(|(_, e)| matches!(e, AttachEvent::Closed(_)))
        .collect();
    assert_eq!(closed.len(), 1);
    assert_eq!(
        rest.last(),
        Some(&(1, AttachEvent::Closed(CloseReason::Exited)))
    );
    assert!(has_ended(&mut events));
    assert_eq!(block_on(att.credit(1)), Err(BackendError::Closed));
}

#[test]
fn an_automatic_write_for_another_generation_is_dropped() {
    let (backend, control) = fake(FakeConfig::default());
    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    control.fail_half(FakeAttachmentRef(1), Half::Input);
    assert!(control.step());

    block_on(async {
        att.write_automatic(BackendGeneration(1), b"old")
            .await
            .unwrap();
        att.write_automatic(BackendGeneration(2), b"new")
            .await
            .unwrap();
    });
    assert_eq!(control.automatic_input(&term), vec![b"new".to_vec()]);
}

#[test]
fn memory_file_store_counts_revisions_and_refuses_bad_paths() {
    let store = MemoryFileStore::new();
    let space = SpaceId("/repo".to_string());
    let other = SpaceId("/other".to_string());
    block_on(async {
        assert_eq!(store.revision(&space).await, Ok(StoreRevision(0)));
        assert_eq!(store.read(&space, "a.txt").await, Ok(None));
        let revision = |outcome: Result<WriteOutcome, StoreError>| outcome.unwrap().revision;
        assert_eq!(
            revision(store.write(&space, "a.txt", b"one".to_vec()).await),
            StoreRevision(1)
        );
        assert_eq!(
            revision(store.write(&space, "dir/b.txt", b"two".to_vec()).await),
            StoreRevision(2)
        );
        assert_eq!(
            revision(store.write(&space, "a.txt", b"three".to_vec()).await),
            StoreRevision(3)
        );
        assert_eq!(store.revision(&other).await, Ok(StoreRevision(0)));
        let file = store.read(&space, "a.txt").await.unwrap().unwrap();
        assert_eq!(file.bytes, b"three");
        assert_eq!(file.revision, StoreRevision(3));
        assert_eq!(store.read(&other, "a.txt").await, Ok(None));

        for path in ["", "/abs", "a/../b", "./a", "a//b", "a/", "..", "a/./b"] {
            assert_eq!(
                store.write(&space, path, Vec::new()).await,
                Err(StoreError::InvalidPath),
                "{path:?}"
            );
            assert_eq!(
                store.read(&space, path).await,
                Err(StoreError::InvalidPath),
                "{path:?}"
            );
        }
        assert_eq!(store.revision(&space).await, Ok(StoreRevision(3)));
    });
}

#[test]
fn memory_write_enqueues_change_before_returning() {
    let store = MemoryFileStore::new();
    let space = SpaceId("/repo".to_string());
    let mut changes = store.changes();
    block_on(async {
        store.watch(&space).await.unwrap();
        store
            .write(&space, "layout.json", b"{}".to_vec())
            .await
            .unwrap();
        assert_eq!(
            changes.next().now_or_never(),
            Some(Some(FileChange {
                space: space.clone(),
                path: "layout.json".to_string(),
            }))
        );
        assert_eq!(changes.next().now_or_never(), None);
        let unwatched = SpaceId("/other".to_string());
        store
            .write(&unwatched, "layout.json", b"{}".to_vec())
            .await
            .unwrap();
        assert_eq!(changes.next().now_or_never(), None);
        assert!(store.changes().next().await.is_none());
    });
}

#[test]
fn memory_outside_edits_and_faults() {
    let store = MemoryFileStore::new();
    let space = SpaceId("/repo".to_string());
    let mut changes = store.changes();
    block_on(async {
        store.watch(&space).await.unwrap();
        store.write_outside(&space, "layout.json", b"one".to_vec());
        assert!(changes.next().now_or_never().flatten().is_some());
        assert_eq!(store.bytes(&space, "layout.json"), Some(b"one".to_vec()));
        assert_eq!(store.revision(&space).await, Ok(StoreRevision(0)));
        let read = store.read(&space, "layout.json").await.unwrap().unwrap();
        assert!(!read.read_only);
        assert_eq!(store.read_count(&space, "layout.json"), 1);
        store.remove_outside(&space, "layout.json");
        assert!(changes.next().now_or_never().flatten().is_some());
        assert_eq!(store.read(&space, "layout.json").await, Ok(None));
        assert_eq!(store.read_count(&space, "layout.json"), 2);

        store.fail_next_write(&space, StoreError::Unavailable("boom".into()));
        assert_eq!(
            store.write(&space, "layout.json", Vec::new()).await,
            Err(StoreError::Unavailable("boom".into()))
        );
        assert!(store.write(&space, "layout.json", Vec::new()).await.is_ok());
        assert_eq!(store.write_count(&space, "layout.json"), 1);

        store.set_read_only(&space, true);
        let outcome = store
            .write(&space, "layout.json", b"two".to_vec())
            .await
            .unwrap();
        assert!(outcome.read_only);
        assert_eq!(
            store.fallback_bytes(&space, "layout.json"),
            Some(b"two".to_vec())
        );
        assert_eq!(store.bytes(&space, "layout.json"), Some(Vec::new()));
        assert!(
            store
                .read(&space, "layout.json")
                .await
                .unwrap()
                .unwrap()
                .read_only
        );
    });
    let bare = MemoryFileStore::new().without_fallback();
    bare.set_read_only(&space, true);
    assert!(matches!(
        block_on(bare.write(&space, "layout.json", Vec::new())),
        Err(StoreError::ReadOnly(_))
    ));
    assert_eq!(bare.bytes(&space, "layout.json"), None);
}

/// Counts how often it is woken.
#[derive(Default)]
struct CountingWaker(AtomicUsize);

impl ArcWake for CountingWaker {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn emitted_output_wakes_a_pending_attachment_stream() {
    let (backend, control) = fake(FakeConfig::default());
    let term = spawn(&backend);
    let mut att = attach(&backend, &term);
    let mut events = att.events();
    drain(&mut events);

    let counter = Arc::new(CountingWaker::default());
    let waker = waker(Arc::clone(&counter));
    let mut cx = Context::from_waker(&waker);
    assert!(events.poll_next_unpin(&mut cx).is_pending());
    assert_eq!(counter.0.load(Ordering::SeqCst), 0);

    control.emit(&term, b"hello");
    assert!(counter.0.load(Ordering::SeqCst) >= 1);
    assert!(matches!(
        events.poll_next_unpin(&mut cx),
        std::task::Poll::Ready(Some((_, AttachEvent::Data(bytes)))) if bytes == b"hello"
    ));
}

#[test]
fn spawn_wakes_a_pending_watch_stream() {
    let (backend, _control) = fake(FakeConfig::default());
    let mut watch = block_on(backend.watch()).unwrap();

    let counter = Arc::new(CountingWaker::default());
    let waker = waker(Arc::clone(&counter));
    let mut cx = Context::from_waker(&waker);
    assert!(watch.poll_next_unpin(&mut cx).is_pending());
    assert_eq!(counter.0.load(Ordering::SeqCst), 0);

    let term = spawn(&backend);
    assert!(counter.0.load(Ordering::SeqCst) >= 1);
    assert!(matches!(
        watch.poll_next_unpin(&mut cx),
        std::task::Poll::Ready(Some(TermEvent::Created(info))) if info.id == term
    ));
}

#[test]
fn testing_files_start_with_the_feature_gate() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/testing");
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "control.rs",
            "fake_attachment.rs",
            "fake_backend.rs",
            "file_store.rs",
            "mod.rs",
            "world.rs"
        ]
    );
    for name in names {
        let text = fs::read_to_string(dir.join(&name)).unwrap();
        assert_eq!(
            text.lines().next(),
            Some("#![cfg(feature = \"test-support\")]"),
            "{name}"
        );
        assert!(!text.contains("crate::core"), "{name} uses core");
    }
}
