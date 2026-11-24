#![cfg(feature = "test-support")]
//! The contract harness itself: the client simulator, the markers and payloads, the runner
//! and the fake target. The cases are tested by running them in `contract_fake_*`.

mod common;
#[macro_use]
mod contract;

use combine_protocol::{
    Attached, AttachmentId, BackendGeneration, ClientMessage, CloseReason, Closed, Cursor,
    DaemonMessage, Data, Epoch, Fidelity, Ownership, OwnershipGeneration, Resumed, Seq, Started,
    TermId,
};
use combined::core::{BackendCommand, Core};
use combined::model::Millis;
use combined::testing::{FakeConfig, Mutation};
use common::{display, request_options};
use contract::client::{AckPolicy, Applied, ReadPolicy, ReplyPolicy};
use contract::fake_target::FakeTarget;
use contract::generator::{osc_marker, printable_marker, Payload};
use contract::report::{record, Verdict, SPEC_REVISION};
use contract::{run_named, run_quietly, Case, ClientSim, Failure, Fault, Harness, Outcome, Target};
use futures::FutureExt;

fn cursor(attachment: u64, epoch: u64, seq: u64) -> Cursor {
    Cursor {
        attachment: AttachmentId(attachment),
        epoch: Epoch(epoch),
        seq: Seq(seq),
    }
}

fn started(attachment: u64, epoch: u64, seq: u64) -> DaemonMessage {
    DaemonMessage::Started(Started {
        cursor: cursor(attachment, epoch, seq),
        backend_generation: BackendGeneration(epoch),
        cols: 80,
        rows: 24,
        fidelity: Fidelity::Exact,
        ordered_resizes: true,
    })
}

fn data(attachment: u64, epoch: u64, seq: u64, bytes: &[u8]) -> DaemonMessage {
    DaemonMessage::Data(Data {
        cursor: cursor(attachment, epoch, seq),
        bytes: bytes.to_vec(),
    })
}

/// A client that has been told `attachment` exists on a terminal.
fn client_with(attachment: u64) -> ClientSim {
    let mut client = ClientSim::new();
    let request = client.next_request();
    client.expect_attach(request, TermId("fake:t1".to_string()));
    client.apply(&DaemonMessage::Attached(Attached {
        request,
        attachment: AttachmentId(attachment),
    }));
    client
}

fn ownership(attachment: u64, generation: u64, owner: Option<u64>) -> DaemonMessage {
    DaemonMessage::Ownership(Ownership {
        attachment: AttachmentId(attachment),
        ownership_generation: OwnershipGeneration(generation),
        owner: owner.map(AttachmentId),
    })
}

fn check(outcome: Outcome) {
    if let Err(failure) = outcome {
        panic!("{failure}");
    }
}

#[test]
fn attach_type_and_read_back() {
    let mut target = FakeTarget::new();
    check(FakeTarget::block_on(async {
        let mut h = Harness::new(&mut target, combined::core::CoreConfig::default())?;
        let term = h.spawn("p", 80, 24).await?;
        let mut c = h.client();
        let att = h
            .attach(&mut c, &term, request_options(80, 24, false))
            .await?;
        h.mark(&term, &mut [&mut c]).await?;
        h.type_input(&mut c, att, b"hi").await?;
        h.settle(&mut [&mut c]).await?;
        let view = c.view(att).expect("attached");
        ensure!("test", view.owns(), "typing should grant ownership");
        ensure!("test", view.bytes() == b"hi", "got {:?}", view.transcript);
        h.finish(&[&c])
    }));
}

#[test]
fn client_records_seq_gaps_and_epoch_jumps() {
    let mut clean = client_with(1);
    for message in [started(1, 1, 0), data(1, 1, 1, b"a"), data(1, 1, 2, b"b")] {
        clean.apply(&message);
    }
    assert!(clean.violations.is_empty(), "{:?}", clean.violations);

    let mut gap = client_with(1);
    for message in [started(1, 1, 0), data(1, 1, 2, b"a")] {
        gap.apply(&message);
    }
    assert_eq!(gap.violations.len(), 1, "{:?}", gap.violations);

    let mut jump = client_with(1);
    for message in [started(1, 1, 0), started(1, 3, 0)] {
        jump.apply(&message);
    }
    assert_eq!(jump.violations.len(), 1, "{:?}", jump.violations);

    let mut late_start = client_with(1);
    late_start.apply(&started(1, 1, 3));
    assert_eq!(
        late_start.violations.len(),
        1,
        "{:?}",
        late_start.violations
    );

    let mut headless = client_with(1);
    headless.apply(&data(1, 1, 0, b"a"));
    assert_eq!(headless.violations.len(), 1, "{:?}", headless.violations);

    let mut after_end = client_with(1);
    after_end.apply(&started(1, 1, 0));
    after_end.apply(&DaemonMessage::Closed(Closed {
        cursor: cursor(1, 1, 1),
        reason: CloseReason::Exited,
    }));
    assert!(after_end.violations.is_empty());
    after_end.apply(&data(1, 1, 2, b"late"));
    assert_eq!(after_end.violations.len(), 1, "{:?}", after_end.violations);
}

#[test]
fn measurement_starts_after_the_marker_even_when_split() {
    for marker in [osc_marker("n-1"), printable_marker("n-1")] {
        for cut in 1..marker.len() {
            let mut client = client_with(1);
            client.expect_marker(marker.clone());
            client.apply(&started(1, 1, 0));
            let mut first = b"paint".to_vec();
            first.extend_from_slice(&marker[..cut]);
            let mut second = marker[cut..].to_vec();
            second.extend_from_slice(b"after");
            client.apply(&data(1, 1, 1, &first));
            assert!(!client.view(AttachmentId(1)).unwrap().measuring());
            client.apply(&data(1, 1, 2, &second));
            client.apply(&data(1, 1, 3, b"!"));
            let view = client.view(AttachmentId(1)).unwrap();
            assert!(view.measuring() && view.marker_seen);
            assert_eq!(view.measured_len, 6, "cut at {cut}");
            assert_eq!(view.transcript, [Applied::Bytes(b"after!".to_vec())]);
        }

        let mut spread = client_with(1);
        spread.expect_marker(marker.clone());
        spread.apply(&started(1, 1, 0));
        for (seq, byte) in marker.iter().enumerate() {
            spread.apply(&data(1, 1, seq as u64 + 1, &[*byte]));
        }
        let view = spread.view(AttachmentId(1)).unwrap();
        assert!(view.measuring());
        assert_eq!(view.measured_len, 0, "a marker one byte at a time");
    }
}

#[test]
fn a_new_epoch_stops_measuring_until_the_marker_is_seen_again() {
    let marker = osc_marker("n-2");
    let mut client = client_with(1);
    client.expect_marker(marker.clone());
    client.apply(&started(1, 1, 0));
    client.apply(&data(1, 1, 1, &marker));
    client.apply(&data(1, 1, 2, b"abc"));
    assert_eq!(client.view(AttachmentId(1)).unwrap().measured_len, 3);
    client.apply(&started(1, 2, 0));
    let view = client.view(AttachmentId(1)).unwrap();
    assert!(!view.measuring());
    assert_eq!(view.measured_len, 0);
    assert!(view.transcript.is_empty());
    assert_eq!(view.started.len(), 2);
}

#[test]
fn settle_fails_after_max_rounds_with_a_reason() {
    let mut target = FakeTarget::new();
    target.quiet = 5;
    target.max = 1;
    let failure = FakeTarget::block_on(async {
        let mut h = Harness::new(&mut target, combined::core::CoreConfig::default())?;
        h.settle(&mut []).await
    })
    .expect_err("one round can't be five quiet ones");
    assert_eq!(failure.check, "settle");
    assert!(failure.message.contains("1 rounds"), "{}", failure.message);

    let mut target = FakeTarget::new();
    let failure = FakeTarget::block_on(async {
        let mut h = Harness::new(&mut target, combined::core::CoreConfig::default())?;
        h.settle_until(&mut [], "the impossible", |_| false).await
    })
    .expect_err("nothing will make the predicate true");
    assert_eq!(failure.check, "settle");
    assert!(
        failure.message.contains("waited for the impossible"),
        "{}",
        failure.message
    );
}

#[test]
fn record_appends_a_revision_line() {
    let pass = record("case_one", "fake", &Verdict::Pass);
    assert!(pass.starts_with("contract case=case_one target=fake "));
    assert!(pass.contains(&format!("spec_rev={SPEC_REVISION}")));
    assert!(pass.contains("source_rev="));
    assert!(pass.ends_with("result=pass"));
    assert_eq!(SPEC_REVISION, 23);

    let fail = record(
        "case_two",
        "fake",
        &Verdict::Fail(Failure::new("payload-length", "first line\nsecond line")),
    );
    assert!(fail.ends_with("result=fail check=payload-length reason=first line"));
    let skip = record("case_three", "fake", &Verdict::Skip("no twin".to_string()));
    assert!(skip.ends_with("result=skip reason=no twin"));

    let log = std::fs::read_to_string(concat!(
        env!("CARGO_TARGET_TMPDIR"),
        "/contract-results.log"
    ))
    .expect("the results log");
    for line in [&pass, &fail, &skip] {
        assert!(
            log.lines().any(|logged| logged == line),
            "{line} is not logged"
        );
    }
}

#[test]
fn payload_is_deterministic_and_never_contains_a_marker() {
    let payload = Payload::new(42, 100_000);
    assert_eq!(payload.len(), 100_000);
    assert_eq!(payload.bytes(), Payload::new(42, 100_000).bytes());
    assert_ne!(payload.bytes(), Payload::new(43, 100_000).bytes());
    assert!(payload
        .bytes()
        .iter()
        .all(|byte| (0x20..=0x7e).contains(byte) && *byte != b'<'));
    for marker in [osc_marker("n-3"), printable_marker("n-3")] {
        assert!(!payload
            .bytes()
            .windows(marker.len())
            .any(|window| window == marker));
    }
    assert!(!payload
        .bytes()
        .windows(5)
        .any(|window| window == b"\x1b[?6n"));
    assert_eq!(payload.sha256(), payload.sha256_of(0..payload.len()));
    assert_ne!(payload.sha256(), payload.sha256_of(0..payload.len() - 1));
    let pieces: Vec<&[u8]> = payload.pieces(30_000).collect();
    assert_eq!(pieces.len(), 4);
    assert_eq!(pieces.concat(), payload.bytes());
    let printable = printable_marker("n-3");
    assert!(printable.starts_with(b"<<combine-mark:") && printable.ends_with(b">>"));
    let osc = osc_marker("n-3");
    assert!(osc.starts_with(b"\x1b]9999;combine-mark;") && osc.ends_with(b"\x07"));
}

#[test]
fn mutated_target_describes_and_applies_its_mutation() {
    assert_eq!(FakeTarget::new().describe(), "fake");
    for mutation in [
        Mutation::DropResized,
        Mutation::MoveResized,
        Mutation::DropByte,
    ] {
        assert_eq!(
            FakeTarget::mutated(mutation).describe(),
            format!("fake (mutation {mutation:?})")
        );
    }

    let mut dropping = FakeTarget::mutated(Mutation::DropByte);
    let measured = FakeTarget::block_on(async {
        let mut h = Harness::new(&mut dropping, combined::core::CoreConfig::default())?;
        let term = h.spawn("p", 80, 24).await?;
        let mut c = h.client();
        let att = h
            .attach(&mut c, &term, request_options(80, 24, false))
            .await?;
        h.mark(&term, &mut [&mut c]).await?;
        h.emit(&term, Payload::new(1, 4_096).bytes()).await?;
        h.settle(&mut [&mut c]).await?;
        Ok::<u64, Failure>(c.view(att).expect("attached").measured_len)
    })
    .expect("scenario");
    assert_eq!(measured, 4_095, "DropByte should lose one byte");

    let mut hiding = FakeTarget::mutated(Mutation::DropResized);
    let resizes = FakeTarget::block_on(async {
        let mut h = Harness::new(&mut hiding, combined::core::CoreConfig::default())?;
        let term = h.spawn("p", 80, 24).await?;
        let mut c = h.client();
        let att = h
            .attach(&mut c, &term, request_options(80, 24, false))
            .await?;
        h.type_input(&mut c, att, b"x").await?;
        h.settle(&mut [&mut c]).await?;
        h.mark(&term, &mut [&mut c]).await?;
        h.resize(&mut c, att, 100, 30).await?;
        h.settle(&mut [&mut c]).await?;
        Ok::<usize, Failure>(c.view(att).expect("attached").resizes().len())
    })
    .expect("scenario");
    assert_eq!(resizes, 0, "DropResized should hide the first Resized");
}

fn explode(message: &str) -> Outcome {
    panic!("{message}")
}

fn case_that(
    needs: &'static [Fault],
    run: fn(&mut FakeTarget) -> futures::future::LocalBoxFuture<'_, Outcome>,
) -> Case<FakeTarget> {
    Case {
        name: "probe",
        needs,
        heavy: false,
        run,
    }
}

#[test]
fn run_quietly_turns_a_panic_into_fail_and_an_unsupported_need_into_skip() {
    let panics = case_that(&[], |_| async { explode("boom") }.boxed_local());
    match run_quietly(FakeTarget::new, &panics) {
        Verdict::Fail(failure) => {
            assert_eq!(failure.check, "panic");
            assert!(failure.message.contains("boom"), "{}", failure.message);
        }
        other => panic!("expected a failure, got {other:?}"),
    }

    let passes = case_that(&[], |_| async { Ok(()) }.boxed_local());
    assert!(matches!(
        run_quietly(FakeTarget::new, &passes),
        Verdict::Pass
    ));

    let fails = case_that(&[], |_| {
        async { Err(Failure::new("probe-check", "no")) }.boxed_local()
    });
    assert!(matches!(
        run_quietly(FakeTarget::new, &fails),
        Verdict::Fail(failure) if failure.check == "probe-check"
    ));

    let needs_twin = case_that(&[Fault::RemoteTwin], |_| {
        async { explode("must not run") }.boxed_local()
    });
    let lacking = || {
        let mut target = FakeTarget::new();
        target.unsupported.push(Fault::RemoteTwin);
        target
    };
    assert!(matches!(
        run_quietly(lacking, &needs_twin),
        Verdict::Skip(reason) if reason.contains("RemoteTwin")
    ));
    assert!(FakeTarget::new().supports(Fault::RemoteTwin));

    let named = [case_that(&[], |_| {
        async { Err(Failure::new("probe-check", "no")) }.boxed_local()
    })];
    let escaped = std::panic::catch_unwind(|| run_named(FakeTarget::new, &named, "probe"));
    assert!(escaped.is_err(), "run_named should panic on a failure");
}

#[test]
fn core_accepts_the_small_caps() {
    let capabilities = FakeTarget::new().backend().capabilities();
    let config = Harness::<FakeTarget>::small_caps();
    assert_eq!(config.log_max_bytes, 524_288);
    assert_eq!(config.log_max_events, 512);
    assert_eq!(config.credit_reserve, 16_384);
    assert_eq!(config.credit_min_grant, 16_384);
    Core::new(config, capabilities).expect("small caps must be a valid configuration");
}

#[test]
fn reply_policies_follow_the_spec_on_crafted_messages() {
    let query = b"x\x1b[?6ny\x1b[?6n";
    let replies = |policy: ReplyPolicy, owner: Option<u64>| {
        let mut client = client_with(1);
        client.replies = policy;
        client.apply(&ownership(1, 7, owner));
        client.apply(&started(1, 1, 0));
        client.apply(&data(1, 1, 1, query));
        client.outgoing
    };

    let expected = vec![
        ClientMessage::AutomaticReply(combine_protocol::AutomaticReply {
            attachment: AttachmentId(1),
            epoch: Epoch(1),
            ownership_generation: OwnershipGeneration(7),
            bytes: b"\x1b[?1;1R".to_vec(),
        }),
        ClientMessage::AutomaticReply(combine_protocol::AutomaticReply {
            attachment: AttachmentId(1),
            epoch: Epoch(1),
            ownership_generation: OwnershipGeneration(7),
            bytes: b"\x1b[?1;2R".to_vec(),
        }),
    ];
    assert_eq!(replies(ReplyPolicy::AsSpec, Some(1)), expected);
    assert_eq!(replies(ReplyPolicy::Always, Some(1)), expected);
    assert_eq!(replies(ReplyPolicy::Always, Some(2)), expected);
    assert!(replies(ReplyPolicy::AsSpec, Some(2)).is_empty());
    assert!(replies(ReplyPolicy::AsSpec, None).is_empty());
    assert!(replies(ReplyPolicy::Never, Some(1)).is_empty());

    let mut resumed = client_with(1);
    resumed.apply(&ownership(1, 7, Some(1)));
    resumed.apply(&started(1, 1, 0));
    resumed.apply(&data(1, 1, 1, b"plain"));
    resumed.apply(&DaemonMessage::Resumed(Resumed {
        request: combine_protocol::RequestId(9),
        attachment: AttachmentId(1),
        replay_through: Some(cursor(1, 1, 3)),
    }));
    resumed.apply(&data(1, 1, 2, query));
    resumed.apply(&data(1, 1, 3, query));
    assert!(resumed.outgoing.is_empty(), "replayed queries get no reply");
    resumed.apply(&data(1, 1, 4, query));
    assert_eq!(resumed.outgoing.len(), 2, "a live query gets its reply");

    let mut always = client_with(1);
    always.replies = ReplyPolicy::Always;
    always.apply(&started(1, 1, 0));
    always.apply(&DaemonMessage::Resumed(Resumed {
        request: combine_protocol::RequestId(9),
        attachment: AttachmentId(1),
        replay_through: Some(cursor(1, 1, 2)),
    }));
    always.apply(&data(1, 1, 1, query));
    assert_eq!(
        always.outgoing.len(),
        2,
        "Always answers replayed queries too"
    );
}

#[test]
fn the_never_ack_policy_keeps_its_acks_until_the_policy_changes() {
    let mut client = client_with(1);
    client.apply(&started(1, 1, 0));
    client.apply(&data(1, 1, 1, b"a"));
    client.apply(&data(1, 1, 2, b"b"));
    client.ack = AckPolicy::Never;
    assert!(client.take_acks().is_empty());
    assert!(client.take_acks().is_empty());
    client.ack = AckPolicy::AfterEachRead;
    let acks = client.take_acks();
    assert_eq!(acks.len(), 1, "one ack for the last applied event");
    assert!(matches!(&acks[0], ClientMessage::Ack(ack) if ack.cursor == cursor(1, 1, 2)));
    assert!(client.take_acks().is_empty(), "an ack is sent once");
}

#[test]
fn a_paused_client_reads_only_on_request() {
    let mut target = FakeTarget::new();
    check(FakeTarget::block_on(async {
        let mut h = Harness::new(&mut target, combined::core::CoreConfig::default())?;
        let term = h.spawn("p", 80, 24).await?;
        let mut c = h.client();
        let att = h
            .attach(&mut c, &term, request_options(80, 24, false))
            .await?;
        h.mark(&term, &mut [&mut c]).await?;
        c.read = ReadPolicy::Paused;
        let before = c.view(att).and_then(|view| view.last_applied);
        h.emit(&term, b"abc").await?;
        h.settle(&mut [&mut c]).await?;
        ensure!(
            "test",
            c.view(att).and_then(|view| view.last_applied) == before,
            "a paused client applied something during settle"
        );
        h.read_until(&mut c, |message| matches!(message, DaemonMessage::Data(_)))
            .await?;
        let view = c.view(att).expect("attached");
        ensure!(
            "test",
            view.transcript == [Applied::Bytes(b"abc".to_vec())],
            "got {:?}",
            view.transcript
        );
        h.finish(&[&c])
    }));
}

#[test]
fn the_entry_points_the_wave_four_cases_use_work() {
    let mut target = FakeTarget::with_config(FakeConfig {
        chunk_size: 4_096,
        ..FakeConfig::default()
    });
    check(FakeTarget::block_on(async {
        let mut h = Harness::new(&mut target, combined::core::CoreConfig::default())?;
        let term = h.spawn("p", 80, 24).await?;
        let mut c = h.client();
        let att = h
            .attach(&mut c, &term, request_options(80, 24, false))
            .await?;

        let journal = h.journal();
        ensure!(
            "test",
            matches!(journal.first(), Some(BackendCommand::Attach { .. })),
            "the journal should start with the attach; got {journal:?}"
        );
        ensure!(
            "test",
            h.journal().is_empty(),
            "the journal is taken, not copied"
        );
        ensure!("test", h.stats(att).is_some(), "no stats");

        h.request_ownership(&mut c, att).await?;
        h.focus(&mut c, att).await?;
        h.set_display(&mut c, att, display()).await?;
        h.settle(&mut [&mut c]).await?;
        ensure!(
            "test",
            c.view(att).is_some_and(|view| view.owns()),
            "not the owner"
        );
        let commands = h.journal();
        ensure!(
            "test",
            commands.contains(&BackendCommand::Focus {
                attachment: att,
                ownership: OwnershipGeneration(0)
            }),
            "no focus in {commands:?}"
        );

        h.mark(&term, &mut [&mut c]).await?;
        let payload = Payload::new(5, 40_000);
        h.emit_paced(&term, &payload, 10_000, &mut [&mut c]).await?;
        let view = c.view(att).expect("attached");
        ensure!(
            "test",
            view.measured_len == 40_000 && view.measured_sha256() == payload.sha256(),
            "paced output was not measured exactly"
        );

        let closed = h.advance(1_500).await?;
        ensure!(
            "test",
            closed.is_empty() && h.now == Millis(1_500),
            "advance moved wrongly"
        );
        h.settle_until(&mut [&mut c], "nothing", |_| true).await?;

        h.detach(&mut c, att).await?;
        h.read_until(&mut c, |message| {
            matches!(message, DaemonMessage::Closed(_))
        })
        .await?;
        h.settle(&mut [&mut c]).await?;
        ensure!(
            "test",
            c.view(att).is_some_and(|view| !view.live()),
            "the detached view is still live"
        );
        h.finish(&[&c])
    }));

    let mut target = FakeTarget::new();
    target
        .disable_byte_credit()
        .expect("before the first backend()");
    assert!(!target.backend().capabilities().byte_credit);
    assert!(
        target.disable_byte_credit().is_err(),
        "after backend() it is too late"
    );
}

#[test]
fn a_restarted_daemon_numbers_attachments_above_the_old_ones() {
    let mut target = FakeTarget::new();
    check(FakeTarget::block_on(async {
        let mut h = Harness::new(&mut target, combined::core::CoreConfig::default())?;
        let term = h.spawn("p", 80, 24).await?;
        let mut c = h.client();
        let old = h
            .attach(&mut c, &term, request_options(80, 24, false))
            .await?;
        h.restart_daemon(&mut [&mut c]).await?;
        ensure!(
            "test",
            c.connection.is_none(),
            "the client kept its connection"
        );
        ensure!(
            "test",
            h.config.id_base > old.0,
            "id_base {} is not above {}",
            h.config.id_base,
            old.0
        );
        c.connection = Some(h.driver.core_mut().connect());
        let fresh = h
            .attach(&mut c, &term, request_options(80, 24, false))
            .await?;
        ensure!("test", fresh > old, "{fresh:?} should be above {old:?}");
        h.finish(&[&c])
    }));
}
