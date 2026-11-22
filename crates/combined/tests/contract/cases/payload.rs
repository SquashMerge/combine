//! Cases 7-10: program output arrives byte for byte, across reconnects, a hidden panel, a
//! burst and a resync.

use combine_protocol::{Cursor, Seq};
use combined::core::CoreConfig;
use futures::FutureExt;

use crate::common::request_options;
use crate::contract::client::{Applied, View};
use crate::contract::generator::Payload;
use crate::contract::{Case, ClientSim, Failure, Fault, Harness, Outcome, Target};

const PIECE: usize = 65_536;
const TOTAL: usize = 2_097_152;

pub fn cases<T: Target>() -> Vec<Case<T>> {
    vec![
        Case {
            name: "reconnect_during_output_is_byte_exact",
            needs: &[],
            heavy: false,
            run: |t| reconnect_during_output_is_byte_exact(t).boxed_local(),
        },
        Case {
            name: "hide_and_show_during_output_is_byte_exact",
            needs: &[],
            heavy: false,
            run: |t| hide_and_show_during_output_is_byte_exact(t).boxed_local(),
        },
        Case {
            name: "burst_of_50_mb_is_byte_exact",
            needs: &[],
            heavy: true,
            run: |t| burst_of_50_mb_is_byte_exact(t).boxed_local(),
        },
        Case {
            name: "resynced_begins_a_fresh_paint_in_order",
            needs: &[Fault::ForcedResync],
            heavy: false,
            run: |t| resynced_begins_a_fresh_paint_in_order(t).boxed_local(),
        },
    ]
}

fn view_of(c: &ClientSim, attachment: combine_protocol::AttachmentId) -> Result<&View, Failure> {
    c.view(attachment)
        .ok_or_else(|| Failure::new("harness", "the client has no view of the attachment"))
}

fn same_sha(measured: [u8; 32], expected: [u8; 32]) -> bool {
    measured == expected
}

/// Case 7.
pub async fn reconnect_during_output_is_byte_exact<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut c = h.client();
    let att = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    h.mark(&term, &mut [&mut c]).await?;

    let payload = Payload::new(7, TOTAL);
    let pieces: Vec<&[u8]> = payload.pieces(PIECE).collect();
    let mut next = 0;
    while view_of(&c, att)?.measured_len < 524_288 {
        h.emit(&term, pieces[next]).await?;
        next += 1;
        h.settle(&mut [&mut c]).await?;
    }
    ensure!(
        "harness",
        next + 1 < pieces.len(),
        "the payload ran out before the drop"
    );

    h.emit(&term, pieces[next]).await?;
    next += 1;
    let lost = h.read_step(&mut c, false).await;
    ensure!(
        "harness",
        lost.is_some(),
        "no message was in flight to lose"
    );
    h.drop_connection(&mut c);
    for piece in &pieces[next..] {
        h.emit(&term, piece).await?;
    }

    let applied = view_of(&c, att)?
        .last_applied
        .ok_or_else(|| Failure::new("harness", "nothing was applied before the drop"))?;
    let retained = h
        .stats(att)
        .ok_or_else(|| Failure::new("harness", "the daemon lost the attachment"))?
        .events;
    h.reconnect(&mut c).await?;

    let view = view_of(&c, att)?;
    let last_entry = Cursor {
        seq: Seq(applied.seq.0 + retained as u64),
        ..applied
    };
    ensure!(
        "replay-through",
        view.replay_through == Some(last_entry),
        "resumed replay_through is {:?}; the log's last entry was {last_entry:?}",
        view.replay_through
    );
    ensure!(
        "payload-length",
        view.measured_len == TOTAL as u64,
        "measured {} bytes, expected {TOTAL}",
        view.measured_len
    );
    ensure!(
        "payload-sha256",
        same_sha(view.measured_sha256(), payload.sha256()),
        "the measured bytes are not the payload"
    );
    h.finish(&[&c])
}

/// Case 8.
pub async fn hide_and_show_during_output_is_byte_exact<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut c = h.client();
    let hidden = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    h.mark(&term, &mut [&mut c]).await?;

    let payload = Payload::new(8, 32 * PIECE);
    let pieces: Vec<&[u8]> = payload.pieces(PIECE).collect();
    for piece in &pieces[..8] {
        h.emit(&term, piece).await?;
        h.settle(&mut [&mut c]).await?;
    }
    h.detach(&mut c, hidden).await?;
    for piece in &pieces[8..16] {
        h.emit(&term, piece).await?;
        h.settle(&mut [&mut c]).await?;
    }
    ensure!(
        "detached",
        view_of(&c, hidden)?.transcript.last()
            == Some(&Applied::Closed(combine_protocol::CloseReason::Detached)),
        "the hidden attachment should end with Closed(Detached)"
    );

    let shown = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    h.mark(&term, &mut [&mut c]).await?;
    for piece in &pieces[16..] {
        h.emit(&term, piece).await?;
        h.settle(&mut [&mut c]).await?;
    }
    let view = view_of(&c, shown)?;
    let expected = 16 * PIECE;
    ensure!(
        "payload-length",
        view.measured_len == expected as u64,
        "measured {} bytes, expected the last 16 pieces ({expected})",
        view.measured_len
    );
    ensure!(
        "payload-sha256",
        same_sha(
            view.measured_sha256(),
            payload.sha256_of(16 * PIECE..32 * PIECE)
        ),
        "the measured bytes are not pieces 17-32"
    );
    h.finish(&[&c])
}

/// Case 9.
pub async fn burst_of_50_mb_is_byte_exact<T: Target>(t: &mut T) -> Outcome {
    const BURST: usize = 50_000_000;
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut c = h.client();
    c.keep_raw = false;
    let att = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    h.mark(&term, &mut [&mut c]).await?;

    let payload = Payload::new(9, BURST);
    h.emit_paced(&term, &payload, 1_000_000, &mut [&mut c])
        .await?;
    let view = view_of(&c, att)?;
    ensure!(
        "payload-length",
        view.measured_len == BURST as u64,
        "measured {} bytes, expected {BURST}",
        view.measured_len
    );
    ensure!(
        "payload-sha256",
        same_sha(view.measured_sha256(), payload.sha256()),
        "the measured bytes are not the burst"
    );
    h.finish(&[&c])
}

/// Case 10.
pub async fn resynced_begins_a_fresh_paint_in_order<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let signals = h.driver.backend().capabilities().resync_signal;
    let mut c = h.client();
    let att = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    h.mark(&term, &mut [&mut c]).await?;

    let payload = Payload::new(10, 1_048_576);
    h.target.emit(&term, payload.bytes()).await?;
    h.target.force_resync(&term, 0)?;
    let marker = h.mark(&term, &mut [&mut c]).await?;

    let transcript = &view_of(&c, att)?.transcript;
    let resynced = transcript
        .iter()
        .position(|item| *item == Applied::Resynced);
    let marked = transcript.iter().position(
        |item| matches!(item, Applied::Bytes(bytes) if bytes.windows(marker.len()).any(|w| w == marker)),
    );
    ensure!(
        "resynced-order",
        marked.is_some(),
        "the second marker never reached the transcript"
    );
    if signals {
        ensure!(
            "resynced-order",
            resynced.is_some_and(|at| Some(at) < marked),
            "Resynced should be applied before the second marker; Resynced at {resynced:?}, marker at {marked:?}"
        );
    }
    let before: Vec<u8> = transcript
        .iter()
        .take(resynced.or(marked).unwrap_or(0))
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
    h.finish(&[&c])
}
