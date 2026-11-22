//! The event log: cursors, acks, resumes, coalescing and the bounds that keep it finite.

use combine_protocol::{
    AttachmentId, BackendGeneration, CloseReason, Cursor, DaemonMessage, Epoch, Fidelity, Seq,
    MAX_DATA_CHUNK,
};
use combined::core::{AckError, Appended, EventLog, LogEnd, LogLimits, ResumeError};
use combined::ports::AttachEvent;

mod common;
use common::display;

const ATT: AttachmentId = AttachmentId(7);

fn limits(max_bytes: usize, max_events: usize) -> LogLimits {
    LogLimits {
        max_bytes,
        max_events,
        credit_reserve: 512,
        credit_min_grant: 64,
    }
}

fn small() -> LogLimits {
    limits(4096, 64)
}

fn new_log(limits: LogLimits, byte_credit: bool) -> EventLog {
    EventLog::new(ATT, limits, byte_credit)
}

fn started(log: &mut EventLog, generation: u64, initial: u32) -> Appended {
    log.start_epoch(
        BackendGeneration(generation),
        80,
        24,
        Fidelity::Exact,
        true,
        initial,
    )
}

fn live(limits: LogLimits) -> EventLog {
    let mut log = new_log(limits, false);
    assert_eq!(started(&mut log, 1, 0), Appended::Yes);
    log
}

fn data(bytes: &[u8]) -> AttachEvent {
    AttachEvent::Data(bytes.to_vec())
}

fn resized() -> AttachEvent {
    AttachEvent::Resized {
        cols: 100,
        rows: 30,
    }
}

fn cursor(epoch: u64, seq: u64) -> Cursor {
    Cursor {
        attachment: ATT,
        epoch: Epoch(epoch),
        seq: Seq(seq),
    }
}

fn drain(log: &mut EventLog) -> Vec<DaemonMessage> {
    std::iter::from_fn(|| log.next_message()).collect()
}

fn payload(message: &DaemonMessage) -> &[u8] {
    match message {
        DaemonMessage::Data(data) => &data.bytes,
        other => panic!("not data: {other:?}"),
    }
}

fn is_gap(message: &DaemonMessage) -> bool {
    matches!(message, DaemonMessage::Gap(gap) if gap.request.is_none() && gap.attachment == ATT)
}

/// Appends and sends one-byte `Data` until the open entry is held back.
fn engage_hold_back(log: &mut EventLog) {
    for _ in 0..1000 {
        assert_eq!(log.append(&data(b"x")), Appended::Yes);
        if log.stats().held_back {
            return;
        }
        drain(log);
    }
    panic!("hold-back never engaged");
}

#[test]
fn cursors_run_from_started_at_seq_zero() {
    let mut log = live(small());
    assert_eq!(log.append(&data(b"ab")), Appended::Yes);
    assert_eq!(log.append(&resized()), Appended::Yes);
    let messages = drain(&mut log);
    let cursors: Vec<_> = messages.iter().map(|m| m.cursor().unwrap()).collect();
    assert_eq!(cursors, [cursor(1, 0), cursor(1, 1), cursor(1, 2)]);
    match &messages[0] {
        DaemonMessage::Started(started) => {
            assert_eq!(started.backend_generation, BackendGeneration(1));
            assert_eq!((started.cols, started.rows), (80, 24));
            assert_eq!(started.fidelity, Fidelity::Exact);
            assert!(started.ordered_resizes);
        }
        other => panic!("not started: {other:?}"),
    }
    assert_eq!(log.current_epoch(), Some(Epoch(1)));
}

#[test]
fn nothing_is_appended_before_the_first_started() {
    let mut log = new_log(small(), false);
    assert_eq!(log.current_epoch(), None);
    assert_eq!(log.append(&data(b"x")), Appended::Ignored);
    assert_eq!(log.append(&AttachEvent::Lost), Appended::Ignored);
    assert!(!log.has_sendable());
    assert_eq!(log.next_message(), None);
}

#[test]
fn epochs_count_generations_and_restart_seq() {
    let mut log = live(small());
    log.append(&data(b"a"));
    assert_eq!(started(&mut log, 5, 0), Appended::Yes);
    log.append(&data(b"b"));
    assert_eq!(log.current_epoch(), Some(Epoch(2)));
    let cursors: Vec<_> = drain(&mut log)
        .iter()
        .map(|m| m.cursor().unwrap())
        .collect();
    assert_eq!(
        cursors,
        [cursor(1, 0), cursor(1, 1), cursor(2, 0), cursor(2, 1)]
    );
}

#[test]
fn ack_trims_through_the_applied_event() {
    let mut log = live(small());
    log.append(&data(b"a"));
    log.append(&resized());
    drain(&mut log);
    let before = log.stats();
    assert_eq!(before.events, 3);
    assert_eq!(log.ack(cursor(1, 1)), Ok(false));
    let after = log.stats();
    assert_eq!(after.events, 1);
    assert_eq!(after.retained_bytes, 64);
    assert!(after.retained_bytes < before.retained_bytes);
    log.append(&AttachEvent::Closed(CloseReason::Exited));
    drain(&mut log);
    assert_eq!(log.ack(cursor(1, 2)), Ok(false));
    assert_eq!(log.ack(cursor(1, 3)), Ok(true));
    assert_eq!(log.stats().events, 0);
}

#[test]
fn ack_of_an_unsent_event_is_rejected() {
    let mut log = live(small());
    log.append(&data(b"a"));
    log.append(&resized());
    assert!(log.next_message().is_some());
    assert_eq!(log.ack(cursor(1, 1)), Err(AckError::NotSent));
    assert_eq!(log.ack(cursor(1, 99)), Err(AckError::NotSent));
    assert_eq!(log.ack(cursor(3, 0)), Err(AckError::NotSent));
    let other = Cursor {
        attachment: AttachmentId(8),
        ..cursor(1, 0)
    };
    assert_eq!(log.ack(other), Err(AckError::NotSent));
    assert_eq!(log.stats().events, 3);
}

#[test]
fn late_or_duplicate_ack_is_ignored() {
    let mut log = live(small());
    log.append(&data(b"a"));
    log.append(&resized());
    drain(&mut log);
    assert_eq!(log.ack(cursor(1, 2)), Ok(false));
    assert_eq!(log.stats().events, 0);
    assert_eq!(log.ack(cursor(1, 2)), Ok(false));
    assert_eq!(log.ack(cursor(1, 1)), Ok(false));
    assert_eq!(log.ack(cursor(1, 0)), Ok(false));
    assert_eq!(log.stats().events, 0);
}

#[test]
fn resume_replays_after_the_cursor_in_original_places() {
    let mut log = live(small());
    log.append(&data(b"a"));
    log.append(&resized());
    log.append(&AttachEvent::Facts(display()));
    log.append(&data(b"b"));
    drain(&mut log);
    assert_eq!(log.resume(cursor(1, 1)), Ok(Some(cursor(1, 4))));
    let replay = drain(&mut log);
    let names: Vec<_> = replay.iter().map(|m| m.type_name()).collect();
    assert_eq!(names, ["resized", "facts", "data"]);
    let cursors: Vec<_> = replay.iter().map(|m| m.cursor().unwrap()).collect();
    assert_eq!(cursors, [cursor(1, 2), cursor(1, 3), cursor(1, 4)]);
    assert_eq!(payload(&replay[2]), b"b");
}

#[test]
fn resume_with_nothing_to_replay_has_no_replay_through() {
    let mut log = live(small());
    drain(&mut log);
    assert_eq!(log.ack(cursor(1, 0)), Ok(false));
    assert_eq!(log.resume(cursor(1, 0)), Ok(None));
    assert!(!log.has_sendable());
}

#[test]
fn resume_from_a_trimmed_or_unknown_cursor_is_refused() {
    let mut log = live(small());
    log.append(&data(b"a"));
    log.append(&resized());
    assert!(log.next_message().is_some());
    assert!(log.next_message().is_some());
    assert_eq!(log.ack(cursor(1, 1)), Ok(false));
    assert_eq!(log.resume(cursor(1, 0)), Err(ResumeError::NotServable));
    assert_eq!(log.resume(cursor(9, 9)), Err(ResumeError::NotServable));
    assert_eq!(log.resume(cursor(1, 2)), Err(ResumeError::NotServable));
    let other = Cursor {
        attachment: AttachmentId(8),
        ..cursor(1, 1)
    };
    assert_eq!(log.resume(other), Err(ResumeError::NotServable));
    assert_eq!(log.resume(cursor(1, 1)), Ok(Some(cursor(1, 2))));
}

#[test]
fn resume_seals_the_open_entry() {
    let mut coalesced = live(small());
    coalesced.next_message();
    coalesced.append(&data(b"ab"));
    coalesced.append(&data(b"cd"));
    assert_eq!(coalesced.stats().events, 2);

    let mut log = live(small());
    assert!(log.next_message().is_some());
    log.append(&data(b"ab"));
    assert_eq!(log.resume(cursor(1, 0)), Ok(Some(cursor(1, 1))));
    log.append(&data(b"cd"));
    let replay = drain(&mut log);
    assert_eq!(replay.len(), 2);
    assert_eq!(payload(&replay[0]), b"ab");
    assert_eq!(payload(&replay[1]), b"cd");
    assert_eq!(replay[1].cursor(), Some(cursor(1, 2)));
}

#[test]
fn resume_near_the_bound_is_gap() {
    let mut log = live(small());
    assert_eq!(log.append(&data(&[0; 3800])), Appended::Yes);
    drain(&mut log);
    assert_eq!(log.resume(cursor(1, 0)), Err(ResumeError::NotServable));
}

#[test]
fn resume_of_an_ended_log_ignores_the_margin() {
    let mut log = live(small());
    assert_eq!(log.append(&data(&[0; 3800])), Appended::Yes);
    assert_eq!(
        log.append(&AttachEvent::Closed(CloseReason::Exited)),
        Appended::Yes
    );
    assert_eq!(log.ended(), Some(LogEnd::Final));
    drain(&mut log);
    let stats = log.stats();
    assert!(4096 - 64 - stats.retained_bytes < 192);
    assert_eq!(log.resume(cursor(1, 0)), Ok(Some(cursor(1, 2))));
    let replay = drain(&mut log);
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[1].type_name(), "closed");
}

#[test]
fn resume_after_the_gap_is_refused() {
    let mut log = live(small());
    drain(&mut log);
    assert_eq!(log.append(&data(&[0; 5000])), Appended::Overflowed);
    assert_eq!(log.ended(), Some(LogEnd::Gap));
    assert_eq!(log.resume(cursor(1, 0)), Err(ResumeError::NotServable));
}

#[test]
fn large_data_is_split_one_seq_per_frame() {
    let big = limits(4 * MAX_DATA_CHUNK, 64);
    let mut log = live(big);
    let bytes: Vec<u8> = (0..2 * MAX_DATA_CHUNK + 10).map(|i| i as u8).collect();
    assert_eq!(log.append(&AttachEvent::Data(bytes.clone())), Appended::Yes);
    let messages = drain(&mut log);
    assert_eq!(messages.len(), 4);
    let sizes: Vec<_> = messages[1..].iter().map(|m| payload(m).len()).collect();
    assert_eq!(sizes, [MAX_DATA_CHUNK, MAX_DATA_CHUNK, 10]);
    let cursors: Vec<_> = messages.iter().map(|m| m.cursor().unwrap()).collect();
    assert_eq!(
        cursors,
        [cursor(1, 0), cursor(1, 1), cursor(1, 2), cursor(1, 3)]
    );
    let joined: Vec<u8> = messages[1..]
        .iter()
        .flat_map(|m| payload(m).to_vec())
        .collect();
    assert_eq!(joined, bytes);
}

#[test]
fn coalescing_stops_at_the_chunk_limit() {
    let big = limits(4 * MAX_DATA_CHUNK, 64);
    let mut log = live(big);
    log.append(&AttachEvent::Data(vec![1; MAX_DATA_CHUNK - 1]));
    log.append(&AttachEvent::Data(vec![2; 5]));
    let messages = drain(&mut log);
    assert_eq!(messages.len(), 3);
    assert_eq!(payload(&messages[1]).len(), MAX_DATA_CHUNK);
    assert_eq!(payload(&messages[2]), [2; 4]);
    assert_eq!(messages[2].cursor(), Some(cursor(1, 2)));
}

#[test]
fn unsent_data_coalesces_sent_data_never_changes() {
    let mut log = live(small());
    log.append(&data(b"ab"));
    log.append(&data(b"cd"));
    let stats = log.stats();
    assert_eq!(stats.events, 2);
    assert_eq!(stats.retained_bytes, 64 + 64 + 4);

    assert!(log.next_message().is_some());
    let sent = log.next_message().unwrap();
    assert_eq!(payload(&sent), b"abcd");
    log.append(&data(b"ef"));
    assert_eq!(log.stats().events, 3);
    let next = log.next_message().unwrap();
    assert_eq!(payload(&next), b"ef");
    assert_eq!(next.cursor(), Some(cursor(1, 2)));

    log.append(&data(b"x"));
    log.append(&resized());
    log.append(&data(b"y"));
    let rest = drain(&mut log);
    let names: Vec<_> = rest.iter().map(|m| m.type_name()).collect();
    assert_eq!(names, ["data", "resized", "data"]);
}

#[test]
fn many_small_data_events_coalesce_without_overflow() {
    let mut log = live(limits(1 << 19, 64));
    for _ in 0..10_000 {
        assert_eq!(log.append(&data(b"z")), Appended::Yes);
    }
    let stats = log.stats();
    assert_eq!(stats.events, 2);
    assert_eq!(stats.retained_bytes, 64 + 64 + 10_000);
    assert_eq!(stats.ended, None);
    let messages = drain(&mut log);
    assert_eq!(payload(&messages[1]).len(), 10_000);
}

#[test]
fn pulled_but_unacked_small_data_is_held_back_not_gapped() {
    let mut log = live(small());
    for _ in 0..500 {
        assert_eq!(log.append(&data(b"x")), Appended::Yes);
        drain(&mut log);
    }
    let stats = log.stats();
    assert!(stats.held_back);
    assert_eq!(stats.ended, None);
    assert!(stats.events <= 40);
    assert!(!log.has_sendable());
    assert_eq!(log.next_message(), None);

    assert_eq!(log.ack(cursor(1, stats.events as u64 - 2)), Ok(false));
    let after = log.stats();
    assert!(!after.held_back);
    let released = drain(&mut log);
    assert_eq!(released.len(), 1);
    assert!(payload(&released[0]).len() > 1);
}

#[test]
fn ending_the_log_seals_and_lifts_hold_back() {
    let mut log = live(small());
    engage_hold_back(&mut log);
    assert!(!log.has_sendable());
    assert_eq!(
        log.append(&AttachEvent::Closed(CloseReason::Exited)),
        Appended::Yes
    );
    assert!(!log.stats().held_back);
    let messages = drain(&mut log);
    let tail: Vec<_> = messages
        .iter()
        .rev()
        .take(2)
        .map(|m| m.type_name())
        .collect();
    assert_eq!(tail, ["closed", "data"]);

    let mut log = live(small());
    engage_hold_back(&mut log);
    assert_eq!(log.append(&data(&[0; 4000])), Appended::Overflowed);
    let messages = drain(&mut log);
    assert!(is_gap(messages.last().unwrap()));
    assert_eq!(messages[messages.len() - 2].type_name(), "data");
    assert_eq!(log.next_message(), None);
}

fn assert_gap_follows(log: &mut EventLog) {
    assert_eq!(log.ended(), Some(LogEnd::Gap));
    let messages = drain(log);
    assert!(is_gap(messages.last().unwrap()));
    assert_eq!(messages.iter().filter(|m| is_gap(m)).count(), 1);
    assert_eq!(log.stats().outstanding_credit, 0);
}

#[test]
fn every_kind_of_append_is_bounded() {
    const MARGIN_BYTES: i64 = 192;
    const MARGIN_EVENTS: i64 = 3;
    let margin_kinds: [(&str, AttachEvent); 3] = [
        ("resized", resized()),
        ("facts", AttachEvent::Facts(display())),
        ("resynced", AttachEvent::Resynced),
    ];

    for (name, kind) in &margin_kinds {
        // On bytes: events are plentiful, so the margin on bytes ends the run.
        let mut log = live(limits(4096, 1000));
        let mut last_free = 0i64;
        loop {
            let before = log.stats();
            match log.append(kind) {
                Appended::Yes => {
                    last_free = 4096 - 64 - log.stats().retained_bytes as i64;
                    assert!(last_free >= MARGIN_BYTES, "{name}");
                }
                Appended::Overflowed => {
                    let cost = log.stats().retained_bytes - before.retained_bytes;
                    assert_eq!(cost, 0, "{name}");
                    assert!(last_free < MARGIN_BYTES + 192, "{name}");
                    break;
                }
                Appended::Ignored => panic!("{name} ignored"),
            }
        }
        assert_gap_follows(&mut log);

        // On events: bytes are plentiful.
        let mut log = live(limits(1 << 19, 20));
        loop {
            match log.append(kind) {
                Appended::Yes => {
                    let free = 20 - 1 - log.stats().events as i64;
                    assert!(free >= MARGIN_EVENTS, "{name}");
                }
                Appended::Overflowed => {
                    assert_eq!(19 - log.stats().events as i64, MARGIN_EVENTS, "{name}");
                    break;
                }
                Appended::Ignored => panic!("{name} ignored"),
            }
        }
        assert_gap_follows(&mut log);
    }

    // Started: the margin is checked beside the new allowance.
    let mut log = new_log(limits(4096, 1000), true);
    let mut generation = 0;
    loop {
        generation += 1;
        let before = log.stats().retained_bytes;
        match started(&mut log, generation, 1000) {
            Appended::Yes => {
                let free = 4096 - 64 - log.stats().retained_bytes as i64 - 1000;
                assert!(free >= MARGIN_BYTES);
            }
            Appended::Overflowed => {
                assert!(4096 - 64 - (before as i64 + 64) - 1000 < MARGIN_BYTES);
                break;
            }
            Appended::Ignored => panic!("ignored"),
        }
    }
    assert_gap_follows(&mut log);
    let mut log = new_log(limits(1 << 19, 20), false);
    let mut generation = 0;
    loop {
        generation += 1;
        match started(&mut log, generation, 0) {
            Appended::Yes => assert!(19 - log.stats().events as i64 >= MARGIN_EVENTS),
            Appended::Overflowed => {
                assert_eq!(19 - log.stats().events as i64, MARGIN_EVENTS);
                break;
            }
            Appended::Ignored => panic!("ignored"),
        }
    }
    assert_gap_follows(&mut log);

    // Data: only the bound itself, not the margin, ends the run, and it coalesces.
    let mut log = live(small());
    assert_eq!(log.append(&data(&[0; 3500])), Appended::Yes);
    assert_eq!(log.append(&data(&[0; 300])), Appended::Yes);
    assert!(4096 - 64 - (log.stats().retained_bytes as i64) < MARGIN_BYTES);
    assert_eq!(log.stats().events, 2);
    assert_eq!(log.append(&data(&[0; 200])), Appended::Overflowed);
    assert_gap_follows(&mut log);
    // Events never bound Data: the margin the other kinds keep is enough for the entries one
    // Data event can create, so Data still fits where the others have just been refused.
    let mut log = live(limits(1 << 19, 20));
    while 19 - log.stats().events as i64 > MARGIN_EVENTS {
        assert_eq!(log.append(&resized()), Appended::Yes);
    }
    assert_eq!(log.append(&data(b"x")), Appended::Yes);
    assert_eq!(log.ended(), None);

    // Closed and Lost: the final event needs only room for itself.
    for kind in [AttachEvent::Closed(CloseReason::Exited), AttachEvent::Lost] {
        let mut log = live(small());
        log.append(&data(&[0; 3841]));
        assert_eq!(log.append(&kind), Appended::Overflowed);
        assert_gap_follows(&mut log);

        let mut log = live(small());
        log.append(&data(&[0; 3840]));
        assert_eq!(log.append(&kind), Appended::Yes);
        assert_eq!(log.ended(), Some(LogEnd::Final));
        assert_eq!(log.stats().accounted_bytes, 4096);

        let mut log = live(limits(1 << 19, 20));
        while 19 - log.stats().events as i64 > MARGIN_EVENTS {
            assert_eq!(log.append(&resized()), Appended::Yes);
        }
        assert_eq!(log.append(&kind), Appended::Yes);
        assert_eq!(log.ended(), Some(LogEnd::Final));
    }
}

#[test]
fn ending_the_log_ignores_further_appends() {
    let mut log = live(small());
    assert_eq!(log.append(&AttachEvent::Lost), Appended::Yes);
    assert_eq!(log.append(&data(b"x")), Appended::Ignored);
    assert_eq!(started(&mut log, 2, 0), Appended::Ignored);
    log.end_with_gap();
    assert_eq!(log.ended(), Some(LogEnd::Final));
}

#[test]
fn overflow_ends_the_stream_with_one_gap_in_order() {
    let mut log = live(small());
    log.append(&data(b"a"));
    log.append(&resized());
    assert_eq!(log.append(&data(&[0; 4500])), Appended::Overflowed);
    assert_eq!(log.append(&data(b"late")), Appended::Ignored);
    assert!(!log.gap_sent());
    let messages = drain(&mut log);
    let names: Vec<_> = messages.iter().map(|m| m.type_name()).collect();
    assert_eq!(names, ["started", "data", "resized", "gap"]);
    assert!(is_gap(&messages[3]));
    assert!(log.gap_sent());
    assert_eq!(log.next_message(), None);
    assert!(!log.has_sendable());
}

#[test]
fn the_final_gap_always_fits() {
    let mut log = live(small());
    while log.append(&resized()) == Appended::Yes {}
    let stats = log.stats();
    assert_eq!(stats.ended, Some(LogEnd::Gap));
    assert!(stats.accounted_bytes <= 4096);
    assert!(stats.peak_accounted_bytes <= 4096);
    assert!(stats.peak_events < 64);
    assert_eq!(log.next_cost(), Some(64));
    let messages = drain(&mut log);
    assert!(is_gap(messages.last().unwrap()));

    let mut log = live(limits(1 << 19, 8));
    while log.append(&resized()) == Appended::Yes {}
    let stats = log.stats();
    assert!(stats.peak_events < 8);
}

#[test]
fn credit_counts_granted_and_in_flight_bytes() {
    let mut log = new_log(limits(8192, 64), true);
    assert_eq!(started(&mut log, 1, 1000), Appended::Yes);
    assert_eq!(log.stats().outstanding_credit, 1000);
    assert_eq!(log.stats().accounted_bytes, 64 + 1000 + 64);

    assert_eq!(log.take_credit(), Some(8192 - 64 - 512 - 64 - 1000));
    assert_eq!(log.stats().outstanding_credit, 7552);
    let accounted = log.stats().accounted_bytes;

    assert_eq!(log.append(&data(&[0; 500])), Appended::Yes);
    let stats = log.stats();
    assert_eq!(stats.outstanding_credit, 7052);
    assert_eq!(stats.accounted_bytes, accounted + 64);
    assert!(stats.peak_accounted_bytes >= stats.accounted_bytes);
}

#[test]
fn credit_is_granted_only_with_room() {
    let mut log = new_log(limits(700, 64), true);
    assert_eq!(started(&mut log, 1, 0), Appended::Yes);
    assert_eq!(log.take_credit(), None);

    let mut log = new_log(limits(1000, 64), true);
    assert_eq!(started(&mut log, 1, 0), Appended::Yes);
    assert_eq!(log.take_credit(), Some(360));
    assert_eq!(log.take_credit(), None);

    let mut log = new_log(limits(700, 64), true);
    started(&mut log, 1, 0);
    drain(&mut log);
    assert_eq!(log.ack(cursor(1, 0)), Ok(false));
    assert_eq!(log.take_credit(), Some(124));

    let mut log = new_log(limits(1000, 64), true);
    started(&mut log, 1, 0);
    log.append(&AttachEvent::Lost);
    assert_eq!(log.take_credit(), None);
}

#[test]
fn no_credit_without_byte_credit() {
    let mut log = new_log(limits(8192, 64), false);
    assert_eq!(started(&mut log, 1, 1000), Appended::Yes);
    assert_eq!(log.stats().outstanding_credit, 0);
    assert_eq!(log.take_credit(), None);
}

#[test]
fn rebuild_with_retained_entries_checks_started_against_the_new_allowance() {
    let build = || {
        let mut log = new_log(limits(4096, 64), true);
        assert_eq!(started(&mut log, 1, 1000), Appended::Yes);
        assert_eq!(log.append(&data(&[0; 1500])), Appended::Yes);
        assert_eq!(log.append(&AttachEvent::Resynced), Appended::Yes);
        assert_eq!(log.append(&data(&[0; 1400])), Appended::Yes);
        log
    };
    let mut roomy = build();
    assert_eq!(started(&mut roomy, 2, 0), Appended::Yes);
    assert_eq!(roomy.stats().outstanding_credit, 0);

    let mut tight = build();
    assert_eq!(started(&mut tight, 2, 1000), Appended::Overflowed);
    assert_eq!(tight.stats().outstanding_credit, 0);
    let messages = drain(&mut tight);
    let names: Vec<_> = messages.iter().map(|m| m.type_name()).collect();
    assert_eq!(names, ["started", "data", "resynced", "data", "gap"]);
}

#[test]
fn credit_honouring_backend_never_overflows() {
    let mut log = new_log(
        LogLimits {
            max_bytes: 6000,
            max_events: 64,
            credit_reserve: 1024,
            credit_min_grant: 256,
        },
        true,
    );
    assert_eq!(started(&mut log, 1, 512), Appended::Yes);
    let mut state = 12345u64;
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as usize
    };
    let mut last_sent: Option<Cursor> = None;
    let mut sent_data = 0usize;
    for _ in 0..20_000 {
        match next() % 4 {
            0 | 1 => {
                let room = log.stats().outstanding_credit as usize;
                let n = (next() % 300 + 1).min(room);
                if n > 0 {
                    assert_eq!(log.append(&data(&vec![7; n])), Appended::Yes);
                    sent_data += n;
                }
            }
            2 => {
                while let Some(message) = log.next_message() {
                    last_sent = message.cursor().or(last_sent);
                }
            }
            _ => {
                if let Some(cursor) = last_sent {
                    log.ack(cursor).unwrap();
                    while log.take_credit().is_some() {}
                }
            }
        }
        let stats = log.stats();
        assert!(stats.accounted_bytes <= 6000);
        assert!(stats.events < 64);
        assert_eq!(stats.ended, None);
    }
    assert!(sent_data > 10_000);
    let stats = log.stats();
    assert!(stats.peak_accounted_bytes <= 6000);
    assert!(stats.peak_events < 64);
}
