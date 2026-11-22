//! Cases 12-15: a resize is a boundary in the stream. Output sent at the old size comes before
//! it, output at the new size comes after it, and a replay keeps that order.

use combine_protocol::{
    AttachmentId, ClientMessage, DaemonMessage, OwnershipGeneration, Resize, TermId,
};
use combined::core::{BackendCommand, CoreConfig};
use futures::FutureExt;

use crate::common::request_options;
use crate::contract::client::{Applied, ReadPolicy, View};
use crate::contract::{Case, ClientSim, Failure, Fault, Harness, Outcome, Target};

pub fn cases<T: Target>() -> Vec<Case<T>> {
    vec![
        Case {
            name: "replay_from_before_a_resize_boundary",
            needs: &[],
            heavy: false,
            run: |t| replay_from_before_a_resize_boundary(t).boxed_local(),
        },
        Case {
            name: "replay_from_after_a_resize_boundary",
            needs: &[],
            heavy: false,
            run: |t| replay_from_after_a_resize_boundary(t).boxed_local(),
        },
        Case {
            name: "old_size_output_in_flight_lands_before_resized",
            needs: &[Fault::DelayedOutput],
            heavy: false,
            run: |t| old_size_output_in_flight_lands_before_resized(t).boxed_local(),
        },
        Case {
            name: "same_size_and_non_owner_resizes_change_nothing",
            needs: &[Fault::Observation],
            heavy: false,
            run: |t| same_size_and_non_owner_resizes_change_nothing(t).boxed_local(),
        },
    ]
}

fn view_of(c: &ClientSim, attachment: AttachmentId) -> Result<&View, Failure> {
    c.view(attachment)
        .ok_or_else(|| Failure::new("harness", "the client has no view of the attachment"))
}

fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

/// The `<old:k>` and `<new:k>` tokens in a transcript and the `Resized` items between them,
/// in the order they were applied.
fn order(view: &View) -> Vec<String> {
    let mut seen = Vec::new();
    for item in &view.transcript {
        match item {
            Applied::Bytes(bytes) => {
                let text = String::from_utf8_lossy(bytes);
                for piece in text.split('<').skip(1) {
                    let Some(end) = piece.find('>') else {
                        continue;
                    };
                    let name = &piece[..end];
                    if name.starts_with("old:") || name.starts_with("new:") {
                        seen.push(format!("<{name}>"));
                    }
                }
            }
            Applied::Resized(cols, rows) => seen.push(format!("resized {cols}x{rows}")),
            _ => {}
        }
    }
    seen
}

fn token(age: &str, k: u32) -> Vec<u8> {
    format!("<{age}:{k}>").into_bytes()
}

/// Owner A and a watch-only reference B, both attached at 80x24 and measuring from one marker.
async fn setup<T: Target>(
    h: &mut Harness<'_, T>,
) -> Result<(TermId, ClientSim, ClientSim, AttachmentId), Failure> {
    let term = h.spawn("p", 80, 24).await?;
    let mut a = h.client();
    let mut b = h.client();
    let att_a = h
        .attach(&mut a, &term, request_options(80, 24, false))
        .await?;
    h.attach(&mut b, &term, request_options(80, 24, true))
        .await?;
    h.request_ownership(&mut a, att_a).await?;
    h.settle(&mut [&mut a, &mut b]).await?;
    h.mark(&term, &mut [&mut a, &mut b]).await?;
    Ok((term, a, b, att_a))
}

/// A and B must agree on how many `Resized(120,40)` A applied, where it sits and what screen
/// they end on.
fn check_resize(a: &ClientSim, b: &ClientSim, att_a: AttachmentId, expected: &[&str]) -> Outcome {
    let view = view_of(a, att_a)?;
    ensure!(
        "resized-count",
        view.resizes() == [(120, 40)],
        "expected exactly one Resized(120,40) in A; it applied {:?}",
        view.resizes()
    );
    ensure!(
        "resize-order",
        order(view) == expected,
        "expected the order {expected:?}; A applied {:?}",
        order(view)
    );
    let reference = b
        .views
        .values()
        .next()
        .ok_or_else(|| Failure::new("harness", "the reference client has no view"))?;
    ensure!(
        "same-screen",
        view.size == reference.size && view.transcript == reference.transcript,
        "A ends at {:?} with {:?}; the reference ends at {:?} with {:?}",
        view.size,
        view.transcript,
        reference.size,
        reference.transcript
    );
    Ok(())
}

/// Cases 12 and 13: A reads up to a point, loses the next message in flight, and drops. The
/// replay must show the boundary exactly once and in order.
async fn replay_across_resize<T: Target>(t: &mut T, through_resized: bool) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let (term, mut a, mut b, att_a) = setup(&mut h).await?;

    a.read = ReadPolicy::Paused;
    for k in 1..=2 {
        h.emit(&term, &token("old", k)).await?;
    }
    h.resize(&mut a, att_a, 120, 40).await?;
    for k in 1..=2 {
        h.emit(&term, &token("new", k)).await?;
    }
    h.settle(&mut [&mut b]).await?;

    if through_resized {
        h.read_until(&mut a, |m| matches!(m, DaemonMessage::Resized(_)))
            .await?;
    } else {
        let last_old = token("old", 2);
        h.read_until(
            &mut a,
            |m| matches!(m, DaemonMessage::Data(data) if contains(&data.bytes, &last_old)),
        )
        .await?;
    }
    // A backend that never sends the boundary leaves nothing in flight; the checks below
    // report that, so a missing message is not a plumbing failure here.
    h.read_step(&mut a, false).await;
    h.drop_connection(&mut a);
    a.read = ReadPolicy::Auto;
    h.reconnect(&mut a).await?;
    h.settle(&mut [&mut a, &mut b]).await?;

    let expected = ["<old:1>", "<old:2>", "resized 120x40", "<new:1>", "<new:2>"];
    check_resize(&a, &b, att_a, &expected)?;
    h.finish(&[&a, &b])
}

/// Case 12.
pub async fn replay_from_before_a_resize_boundary<T: Target>(t: &mut T) -> Outcome {
    replay_across_resize(t, false).await
}

/// Case 13.
pub async fn replay_from_after_a_resize_boundary<T: Target>(t: &mut T) -> Outcome {
    replay_across_resize(t, true).await
}

/// Case 14.
pub async fn old_size_output_in_flight_lands_before_resized<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let (term, mut a, mut b, att_a) = setup(&mut h).await?;

    h.target.delay_output(&term)?;
    h.emit(&term, &token("old", 1)).await?;
    h.resize(&mut a, att_a, 120, 40).await?;
    h.emit(&term, &token("new", 1)).await?;
    let sent = BackendCommand::Resize {
        ownership: OwnershipGeneration(0),
        attachment: att_a,
        cols: 120,
        rows: 40,
    };
    let journal = h.journal();
    ensure!(
        "resize-journal",
        journal.contains(&sent),
        "the journal should show Resize(120,40) to A; it shows {journal:?}"
    );
    h.target.release_output(&term)?;
    h.settle(&mut [&mut a, &mut b]).await?;

    let view = view_of(&a, att_a)?;
    let expected = ["<old:1>", "resized 120x40", "<new:1>"];
    ensure!(
        "resize-order",
        order(view) == expected,
        "expected the order {expected:?}; A applied {:?}",
        order(view)
    );
    h.finish(&[&a, &b])
}

/// Case 15.
pub async fn same_size_and_non_owner_resizes_change_nothing<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut a = h.client();
    let mut b = h.client();
    let att_a = h
        .attach(&mut a, &term, request_options(80, 24, false))
        .await?;
    let att_b = h
        .attach(&mut b, &term, request_options(100, 30, false))
        .await?;
    h.request_ownership(&mut a, att_a).await?;
    h.settle(&mut [&mut a, &mut b]).await?;
    h.mark(&term, &mut [&mut a, &mut b]).await?;
    let before = h.target.pty_size(&term)?;
    h.journal();

    h.resize(&mut a, att_a, 80, 24).await?;
    h.settle(&mut [&mut a, &mut b]).await?;
    ensure!(
        "same-size",
        view_of(&a, att_a)?.resizes().is_empty() && view_of(&b, att_b)?.resizes().is_empty(),
        "a resize to the current size produced a Resized"
    );

    let generation = a
        .generation(att_a)
        .ok_or_else(|| Failure::new("harness", "A has heard no ownership generation"))?;
    let stray = Resize {
        attachment: att_b,
        cols: 110,
        rows: 35,
        ownership_generation: generation,
    };
    h.send(&mut b, ClientMessage::Resize(stray)).await?;
    h.settle(&mut [&mut a, &mut b]).await?;
    let to_b: Vec<BackendCommand> = h
        .journal()
        .into_iter()
        .filter(|command| {
            matches!(command, BackendCommand::Resize { attachment, .. } if *attachment == att_b)
        })
        .collect();
    ensure!(
        "non-owner-resize",
        to_b.is_empty(),
        "a non-owner's resize reached the backend: {to_b:?}"
    );
    let after = h.target.pty_size(&term)?;
    ensure!(
        "pty-size",
        after == before,
        "the PTY changed from {before:?} to {after:?}"
    );
    ensure!(
        "non-owner-resize",
        view_of(&a, att_a)?.resizes().is_empty() && view_of(&b, att_b)?.resizes().is_empty(),
        "a non-owner's resize produced a Resized"
    );
    h.finish(&[&a, &b])
}
