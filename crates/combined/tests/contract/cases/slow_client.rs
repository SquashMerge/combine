//! Cases 22-28: a client that reads slowly, or not at all, never makes the daemon's memory
//! grow past its bounds, and a stream that does overflow ends cleanly.

use combine_protocol::{AttachmentId, ClientMessage, Resume, TermId};
use combined::core::{BackendCommand, CoreConfig, LogEnd, LogStats};
use futures::FutureExt;

use crate::common::request_options;
use crate::contract::client::{AckPolicy, Applied, Ending, ReadPolicy, View};
use crate::contract::generator::Payload;
use crate::contract::{Case, ClientSim, Failure, Fault, Harness, Outcome, Target};

const PIECE: usize = 65_536;
const SMALL_FRAMES: usize = 20_000;
const FRAME: usize = 16;
const FLOOD: usize = 2_000;

pub fn cases<T: Target>() -> Vec<Case<T>> {
    vec![
        Case {
            name: "slow_client_output_stays_bounded",
            needs: &[],
            heavy: false,
            run: |t| slow_client_output_stays_bounded(t).boxed_local(),
        },
        Case {
            name: "many_small_frames_to_a_paused_client_never_gap",
            needs: &[],
            heavy: false,
            run: |t| many_small_frames_to_a_paused_client_never_gap(t).boxed_local(),
        },
        Case {
            name: "client_reading_without_acks_is_held_back_not_gapped",
            needs: &[],
            heavy: false,
            run: |t| client_reading_without_acks_is_held_back_not_gapped(t).boxed_local(),
        },
        Case {
            name: "credit_is_granted_only_while_the_log_has_room",
            needs: &[],
            heavy: false,
            run: |t| credit_is_granted_only_while_the_log_has_room(t).boxed_local(),
        },
        Case {
            name: "event_flood_ends_with_a_final_gap_once_drained",
            needs: &[Fault::NoReplacementPaint, Fault::Observation],
            heavy: false,
            run: |t| event_flood_ends_with_a_final_gap_once_drained(t).boxed_local(),
        },
        Case {
            name: "blocked_client_is_disconnected_and_resume_gets_gap",
            needs: &[Fault::NoReplacementPaint],
            heavy: false,
            run: |t| blocked_client_is_disconnected_and_resume_gets_gap(t).boxed_local(),
        },
        Case {
            name: "backend_without_byte_credit_is_still_bounded",
            needs: &[Fault::NoByteCredit],
            heavy: false,
            run: |t| backend_without_byte_credit_is_still_bounded(t).boxed_local(),
        },
    ]
}

fn view_of(c: &ClientSim, attachment: AttachmentId) -> Result<&View, Failure> {
    c.view(attachment)
        .ok_or_else(|| Failure::new("harness", "the client has no view of the attachment"))
}

fn stats_of<T: Target>(h: &Harness<'_, T>, attachment: AttachmentId) -> Result<LogStats, Failure> {
    h.stats(attachment)
        .ok_or_else(|| Failure::new("harness", "the daemon lost the attachment"))
}

/// The log's peaks stay inside the configured bounds, counting the slot and bytes it keeps
/// for a final gap.
fn within_bounds(stats: &LogStats, config: &CoreConfig) -> Outcome {
    ensure!(
        "bounded-bytes",
        stats.peak_accounted_bytes <= config.log_max_bytes,
        "peak accounted bytes {} exceed {}",
        stats.peak_accounted_bytes,
        config.log_max_bytes
    );
    ensure!(
        "bounded-events",
        stats.peak_events < config.log_max_events,
        "peak events {} reach {}",
        stats.peak_events,
        config.log_max_events
    );
    Ok(())
}

fn not_gapped(stats: &LogStats) -> Outcome {
    ensure!(
        "no-gap",
        stats.ended != Some(LogEnd::Gap),
        "the log ended with a gap"
    );
    Ok(())
}

/// Attaches a client and has it measure from a marker, then leaves it reading automatically.
async fn marked_client<T: Target>(
    h: &mut Harness<'_, T>,
    term: &TermId,
) -> Result<(ClientSim, AttachmentId), Failure> {
    let mut c = h.client();
    let att = h
        .attach(&mut c, term, request_options(80, 24, false))
        .await?;
    h.mark(term, &mut [&mut c]).await?;
    Ok((c, att))
}

fn credits(journal: &[BackendCommand]) -> usize {
    journal
        .iter()
        .filter(|command| matches!(command, BackendCommand::Credit { .. }))
        .count()
}

/// Emits `payload` in frames of `frame` bytes.
async fn emit_frames<T: Target>(
    h: &mut Harness<'_, T>,
    term: &TermId,
    payload: &Payload,
    frame: usize,
) -> Outcome {
    for chunk in payload.pieces(frame) {
        h.emit(term, chunk).await?;
    }
    Ok(())
}

/// The client reads again and acks.
fn resume_reading(c: &mut ClientSim) {
    c.read = ReadPolicy::Auto;
    c.ack = AckPolicy::AfterEachRead;
}

async fn settle_through<T: Target>(
    h: &mut Harness<'_, T>,
    c: &mut ClientSim,
    att: AttachmentId,
    total: u64,
) -> Outcome {
    h.settle_until(&mut [&mut *c], "all the output", |clients| {
        clients[0]
            .view(att)
            .is_some_and(|view| view.measured_len == total)
    })
    .await
}

fn exact(view: &View, payload: &Payload) -> Outcome {
    ensure!(
        "payload-length",
        view.measured_len == payload.len() as u64,
        "measured {} bytes, expected {}",
        view.measured_len,
        payload.len()
    );
    ensure!(
        "payload-sha256",
        view.measured_sha256() == payload.sha256(),
        "the measured bytes are not the payload"
    );
    Ok(())
}

/// Case 22.
pub async fn slow_client_output_stays_bounded<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, Harness::<T>::small_caps())?;
    let term = h.spawn("p", 80, 24).await?;
    let signals = h.driver.backend().capabilities().resync_signal;
    let (mut c, att) = marked_client(&mut h, &term).await?;
    c.read = ReadPolicy::Paused;
    c.ack = AckPolicy::Never;

    let payload = Payload::new(22, 8 * 1_048_576);
    for chunk in payload.pieces(PIECE) {
        h.emit(&term, chunk).await?;
        h.settle(&mut [&mut c]).await?;
        let stats = stats_of(&h, att)?;
        within_bounds(&stats, &h.config)?;
        not_gapped(&stats)?;
    }

    resume_reading(&mut c);
    h.settle(&mut [&mut c]).await?;
    let view = view_of(&c, att)?;
    ensure!(
        "no-gap",
        view.ending.is_none(),
        "the stream ended with {:?}",
        view.ending
    );
    let transcript = &view.transcript;
    let resynced = transcript
        .iter()
        .position(|item| *item == Applied::Resynced);
    if signals {
        ensure!(
            "resynced",
            resynced.is_some(),
            "the backend resynced in band, so Resynced should have been applied"
        );
    }
    let before: Vec<u8> = transcript
        .iter()
        .take(resynced.unwrap_or(transcript.len()))
        .filter_map(|item| match item {
            Applied::Bytes(bytes) => Some(bytes.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect();
    ensure!(
        "order-intact",
        payload.bytes().starts_with(&before),
        "the output before the resync is not a prefix of the payload"
    );
    within_bounds(&stats_of(&h, att)?, &h.config)?;
    h.finish(&[&c])
}

/// Case 23.
pub async fn many_small_frames_to_a_paused_client_never_gap<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, Harness::<T>::small_caps())?;
    let term = h.spawn("p", 80, 24).await?;
    let (mut c, att) = marked_client(&mut h, &term).await?;
    c.read = ReadPolicy::Paused;

    let payload = Payload::new(23, SMALL_FRAMES * FRAME);
    emit_frames(&mut h, &term, &payload, FRAME).await?;
    h.settle(&mut [&mut c]).await?;
    let stats = stats_of(&h, att)?;
    ensure!(
        "coalesced",
        stats.events <= 1,
        "{SMALL_FRAMES} small frames left {} entries; they should coalesce into one",
        stats.events
    );
    within_bounds(&stats, &h.config)?;
    not_gapped(&stats)?;

    resume_reading(&mut c);
    settle_through(&mut h, &mut c, att, payload.len() as u64).await?;
    exact(view_of(&c, att)?, &payload)?;
    h.finish(&[&c])
}

/// Case 24.
pub async fn client_reading_without_acks_is_held_back_not_gapped<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, Harness::<T>::small_caps())?;
    let term = h.spawn("p", 80, 24).await?;
    let (mut c, att) = marked_client(&mut h, &term).await?;
    c.ack = AckPolicy::Never;

    let payload = Payload::new(24, SMALL_FRAMES * FRAME);
    let mut held_back = false;
    for chunk in payload.pieces(FRAME) {
        h.emit(&term, chunk).await?;
        h.settle(&mut [&mut c]).await?;
        held_back |= stats_of(&h, att)?.held_back;
    }
    ensure!(
        "held-back",
        held_back,
        "a client that reads without acking was never held back"
    );
    let stats = stats_of(&h, att)?;
    within_bounds(&stats, &h.config)?;
    not_gapped(&stats)?;

    resume_reading(&mut c);
    settle_through(&mut h, &mut c, att, payload.len() as u64).await?;
    exact(view_of(&c, att)?, &payload)?;
    h.finish(&[&c])
}

/// Case 25.
pub async fn credit_is_granted_only_while_the_log_has_room<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, Harness::<T>::small_caps())?;
    let term = h.spawn("p", 80, 24).await?;
    let byte_credit = h.driver.backend().capabilities().byte_credit;
    let (mut c, att) = marked_client(&mut h, &term).await?;
    c.read = ReadPolicy::Paused;
    h.journal();

    let payload = Payload::new(25, 8 * PIECE);
    for chunk in payload.pieces(PIECE) {
        h.emit(&term, chunk).await?;
        h.settle(&mut [&mut c]).await?;
        let stats = stats_of(&h, att)?;
        ensure!(
            "bounded-bytes",
            stats.accounted_bytes <= h.config.log_max_bytes,
            "accounted bytes {} exceed {}",
            stats.accounted_bytes,
            h.config.log_max_bytes
        );
    }
    let early = credits(&h.journal());
    ensure!(
        "credit-room",
        early == 0,
        "credit was granted {early} time(s) while the log had no room"
    );

    resume_reading(&mut c);
    h.settle(&mut [&mut c]).await?;
    let later = credits(&h.journal());
    ensure!(
        "credit-room",
        (later > 0) == byte_credit,
        "after the client acked, {later} credit grant(s) went out (byte credit: {byte_credit})"
    );
    within_bounds(&stats_of(&h, att)?, &h.config)?;
    h.finish(&[&c])
}

/// Sends a client that owns the terminal's size 2,000 alternating resizes, which the backend
/// answers with `Resized` events and no paint, and never reads them.
async fn flood_of_resizes<T: Target>(
    h: &mut Harness<'_, T>,
    term: &TermId,
) -> Result<(ClientSim, AttachmentId), Failure> {
    h.target.set_paint_after_resize(false)?;
    let mut c = h.client();
    let att = h
        .attach(&mut c, term, request_options(80, 24, false))
        .await?;
    h.request_ownership(&mut c, att).await?;
    h.settle(&mut [&mut c]).await?;
    h.mark(term, &mut [&mut c]).await?;
    c.read = ReadPolicy::Paused;
    for i in 0..FLOOD {
        let cols = if i % 2 == 0 { 81 } else { 80 };
        h.resize(&mut c, att, cols, 24).await?;
        h.settle(&mut [&mut c]).await?;
    }
    Ok((c, att))
}

/// Case 26.
pub async fn event_flood_ends_with_a_final_gap_once_drained<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, Harness::<T>::small_caps())?;
    let term = h.spawn("p", 80, 24).await?;
    let (mut c, att) = flood_of_resizes(&mut h, &term).await?;

    within_bounds(&stats_of(&h, att)?, &h.config)?;
    let halves = h.target.open_halves(&term)?;
    ensure!(
        "halves-closed",
        halves == 0,
        "the backend still has {halves} open half(s) after the overflow"
    );

    resume_reading(&mut c);
    h.settle_until(&mut [&mut c], "the final gap", |clients| {
        clients[0]
            .view(att)
            .is_some_and(|view| view.ending.is_some())
    })
    .await?;
    let ending = view_of(&c, att)?.ending.clone();
    ensure!(
        "final-gap",
        ending == Some(Ending::Gap),
        "the drained stream should end with a gap; it ended with {ending:?}"
    );

    let cursor = view_of(&c, att)?
        .last_applied
        .ok_or_else(|| Failure::new("harness", "nothing was applied"))?;
    let request = c.next_request();
    h.send(&mut c, ClientMessage::Resume(Resume { request, cursor }))
        .await?;
    h.settle(&mut [&mut c]).await?;
    ensure!(
        "resume-gap",
        c.resume_refused(att),
        "a later resume should get a gap with the request; got {:?}",
        c.controls
    );
    h.finish(&[&c])
}

/// Case 27.
pub async fn blocked_client_is_disconnected_and_resume_gets_gap<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, Harness::<T>::small_caps())?;
    let term = h.spawn("p", 80, 24).await?;
    let (mut c, att) = flood_of_resizes(&mut h, &term).await?;

    let connection = c
        .connection
        .ok_or_else(|| Failure::new("harness", "the client has no connection"))?;
    let closed = h.advance(10_001).await?;
    ensure!(
        "disconnected",
        closed.contains(&connection),
        "the blocked client's connection was not closed; closed {closed:?}"
    );

    h.drop_connection(&mut c);
    h.reconnect(&mut c).await?;
    ensure!(
        "resume-gap",
        c.resume_refused(att),
        "resuming after the disconnect should get a gap with the request; got {:?}",
        c.controls
    );
    h.finish(&[&c])
}

/// Case 28.
pub async fn backend_without_byte_credit_is_still_bounded<T: Target>(t: &mut T) -> Outcome {
    t.disable_byte_credit()?;
    let mut h = Harness::new(t, Harness::<T>::small_caps())?;
    let term = h.spawn("p", 80, 24).await?;
    let (mut c, att) = marked_client(&mut h, &term).await?;
    c.read = ReadPolicy::Paused;

    let payload = Payload::new(28, 4 * 1_048_576);
    for chunk in payload.pieces(PIECE) {
        h.emit(&term, chunk).await?;
        h.settle(&mut [&mut c]).await?;
        within_bounds(&stats_of(&h, att)?, &h.config)?;
    }
    let stats = stats_of(&h, att)?;
    ensure!(
        "gap",
        stats.ended == Some(LogEnd::Gap),
        "output beyond the log's bounds should end it with a gap; it ended {:?}",
        stats.ended
    );
    h.finish(&[&c])
}
