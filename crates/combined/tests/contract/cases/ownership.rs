//! Cases 34-39: who owns a terminal's size and input when several attachments share it.

use combine_protocol::{
    AttachmentId, ClientMessage, ErrorCode, OwnershipGeneration, Resize, TermId,
};
use combined::core::{BackendCommand, CoreConfig};
use combined::testing::Half;
use futures::FutureExt;

use crate::common::{display, request_options};
use crate::contract::{Case, ClientSim, Failure, Fault, Harness, Outcome, Target};

pub fn cases<T: Target>() -> Vec<Case<T>> {
    vec![
        Case {
            name: "opening_a_second_panel_keeps_the_pty_size",
            needs: &[Fault::Observation],
            heavy: false,
            run: |t| opening_a_second_panel_keeps_the_pty_size(t).boxed_local(),
        },
        Case {
            name: "watch_only_never_takes_size_or_ownership",
            needs: &[Fault::Observation],
            heavy: false,
            run: |t| watch_only_never_takes_size_or_ownership(t).boxed_local(),
        },
        Case {
            name: "stale_resize_is_rejected",
            needs: &[Fault::Observation],
            heavy: false,
            run: |t| stale_resize_is_rejected(t).boxed_local(),
        },
        Case {
            name: "non_owner_input_takes_ownership_before_it_is_forwarded",
            needs: &[],
            heavy: false,
            run: |t| non_owner_input_takes_ownership_before_it_is_forwarded(t).boxed_local(),
        },
        Case {
            name: "owner_leaving_hands_off_to_most_recently_focused",
            needs: &[],
            heavy: false,
            run: |t| owner_leaving_hands_off_to_most_recently_focused(t).boxed_local(),
        },
        Case {
            name: "ownership_changes_while_a_pair_is_reopening",
            needs: &[Fault::HalfFailure, Fault::Observation],
            heavy: false,
            run: |t| ownership_changes_while_a_pair_is_reopening(t).boxed_local(),
        },
    ]
}

/// What the client last heard about who owns the terminal its attachment is on.
fn heard(
    c: &ClientSim,
    attachment: AttachmentId,
) -> Result<(OwnershipGeneration, Option<AttachmentId>), Failure> {
    c.view(attachment)
        .and_then(|view| view.ownership)
        .ok_or_else(|| Failure::new("harness", "the client has heard no ownership"))
}

/// The journal's commands for one attachment, without the byte credit the daemon grants it.
fn commands_for(journal: &[BackendCommand], attachment: AttachmentId) -> Vec<&BackendCommand> {
    journal
        .iter()
        .filter(|command| {
            command.attachment() == attachment && !matches!(command, BackendCommand::Credit { .. })
        })
        .collect()
}

/// Attaches an owner A at 80x24, then a second attachment B at `size`, and settles with both
/// measuring from a marker.
async fn owner_and_second<T: Target>(
    h: &mut Harness<'_, T>,
    term: &TermId,
    ca: &mut ClientSim,
    cb: &mut ClientSim,
    size: (u16, u16),
) -> Result<(AttachmentId, AttachmentId), Failure> {
    let a = h.attach(ca, term, request_options(80, 24, false)).await?;
    h.request_ownership(ca, a).await?;
    h.settle(&mut [&mut *ca]).await?;
    let b = h
        .attach(cb, term, request_options(size.0, size.1, false))
        .await?;
    h.mark(term, &mut [&mut *ca, &mut *cb]).await?;
    Ok((a, b))
}

/// Case 34.
pub async fn opening_a_second_panel_keeps_the_pty_size<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut ca = h.client();
    let a = h
        .attach(&mut ca, &term, request_options(80, 24, false))
        .await?;
    h.request_ownership(&mut ca, a).await?;
    h.settle(&mut [&mut ca]).await?;
    h.journal();

    let mut cb = h.client();
    let b = h
        .attach(&mut cb, &term, request_options(100, 30, false))
        .await?;
    h.settle(&mut [&mut ca, &mut cb]).await?;

    let view = ca
        .view(a)
        .ok_or_else(|| Failure::new("harness", "no view of A"))?;
    ensure!(
        "resized",
        view.resizes().is_empty() && view.size == (80, 24),
        "the second panel resized the owner's view: {:?}, size {:?}",
        view.resizes(),
        view.size
    );
    let size = h.target.pty_size(&term)?;
    ensure!(
        "pty-size",
        size == (80, 24),
        "the PTY is {size:?}, expected the unchanged (80, 24)"
    );
    let journal = h.journal();
    let for_b = commands_for(&journal, b);
    ensure!(
        "journal",
        matches!(for_b.as_slice(), [BackendCommand::Attach { .. }]),
        "the journal for B should be Attach only; got {for_b:?}"
    );
    h.finish(&[&ca, &cb])
}

/// Case 35.
pub async fn watch_only_never_takes_size_or_ownership<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut ca = h.client();
    let a = h
        .attach(&mut ca, &term, request_options(80, 24, false))
        .await?;
    h.request_ownership(&mut ca, a).await?;
    h.settle(&mut [&mut ca]).await?;
    let before = heard(&ca, a)?;
    h.journal();

    let mut cb = h.client();
    let b = h
        .attach(&mut cb, &term, request_options(100, 30, true))
        .await?;
    h.settle(&mut [&mut ca, &mut cb]).await?;
    h.type_input(&mut cb, b, b"x").await?;
    h.request_ownership(&mut cb, b).await?;
    h.resize(&mut cb, b, 110, 35).await?;
    h.settle(&mut [&mut ca, &mut cb]).await?;
    ensure!(
        "watch-only",
        cb.error_codes() == [ErrorCode::WatchOnly; 3],
        "input, request_ownership and resize should each be watch_only; got {:?}",
        cb.error_codes()
    );

    h.focus(&mut cb, b).await?;
    h.set_display(&mut cb, b, display()).await?;
    h.settle(&mut [&mut ca, &mut cb]).await?;
    ensure!(
        "watch-only",
        cb.error_codes() == [ErrorCode::WatchOnly; 3],
        "focus and set_display should give no error; got {:?}",
        cb.error_codes()
    );

    ensure!(
        "owner",
        heard(&ca, a)? == before && heard(&cb, b)? == before,
        "a watch-only attachment changed ownership: {before:?} became {:?}",
        heard(&cb, b)
    );
    let journal = h.journal();
    let for_b = commands_for(&journal, b);
    ensure!(
        "journal",
        matches!(for_b.as_slice(), [BackendCommand::Attach { .. }]),
        "the journal for B should be Attach only; got {for_b:?}"
    );
    let size = h.target.pty_size(&term)?;
    ensure!(
        "pty-size",
        size == (80, 24),
        "the PTY is {size:?}, expected the unchanged (80, 24)"
    );
    h.finish(&[&ca, &cb])
}

/// Case 36.
pub async fn stale_resize_is_rejected<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut ca = h.client();
    let mut cb = h.client();
    let (a, b) = owner_and_second(&mut h, &term, &mut ca, &mut cb, (100, 30)).await?;
    let (first, _) = heard(&ca, a)?;

    h.request_ownership(&mut cb, b).await?;
    h.settle(&mut [&mut ca, &mut cb]).await?;
    let (second, owner) = heard(&ca, a)?;
    ensure!(
        "ownership",
        second != first && owner == Some(b),
        "B should have taken ownership in a new generation; A heard {second:?}, {owner:?}"
    );
    let size = h.target.pty_size(&term)?;
    h.journal();

    let stale = Resize {
        attachment: a,
        cols: 120,
        rows: 40,
        ownership_generation: first,
    };
    h.send(&mut ca, ClientMessage::Resize(stale)).await?;
    h.settle(&mut [&mut ca, &mut cb]).await?;
    ensure!(
        "stale-generation",
        ca.error_codes() == [ErrorCode::StaleGeneration],
        "a resize under the old generation should be stale_generation; got {:?}",
        ca.error_codes()
    );
    ensure!(
        "pty-size",
        h.target.pty_size(&term)? == size,
        "the stale resize changed the PTY from {size:?} to {:?}",
        h.target.pty_size(&term)
    );
    let journal = h.journal();
    ensure!(
        "journal",
        !journal
            .iter()
            .any(|command| matches!(command, BackendCommand::Resize { .. })),
        "the stale resize reached the backend: {journal:?}"
    );
    h.finish(&[&ca, &cb])
}

/// Case 37.
pub async fn non_owner_input_takes_ownership_before_it_is_forwarded<T: Target>(
    t: &mut T,
) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut ca = h.client();
    let mut cb = h.client();
    let (a, b) = owner_and_second(&mut h, &term, &mut ca, &mut cb, (100, 30)).await?;
    let (before, _) = heard(&ca, a)?;
    h.journal();

    h.type_input(&mut cb, b, b"x").await?;
    h.settle(&mut [&mut ca, &mut cb]).await?;
    let (generation, owner) = heard(&ca, a)?;
    ensure!(
        "ownership",
        generation > before && owner == Some(b),
        "A should hear ownership {{g', B}} with g' above {before:?}; got {generation:?}, {owner:?}"
    );
    ensure!(
        "ownership",
        heard(&cb, b)? == (generation, Some(b)),
        "B should hear the same ownership as A; got {:?}",
        heard(&cb, b)
    );

    let journal = h.journal();
    let for_b = commands_for(&journal, b);
    let at = |wanted: &BackendCommand| for_b.iter().position(|command| *command == wanted);
    let write = at(&BackendCommand::Write {
        attachment: b,
        bytes: b"x".to_vec(),
    });
    let resize = at(&BackendCommand::Resize {
        attachment: b,
        cols: 100,
        rows: 30,
    });
    let focus = at(&BackendCommand::Focus { attachment: b });
    ensure!(
        "order",
        matches!((resize, focus, write), (Some(r), Some(f), Some(w)) if r < w && f < w),
        "B's Resize and Focus should come before its Write; the journal for B is {for_b:?}"
    );
    h.finish(&[&ca, &cb])
}

/// Case 38.
pub async fn owner_leaving_hands_off_to_most_recently_focused<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut ca = h.client();
    let mut cb = h.client();
    let mut cc = h.client();
    let mut cw = h.client();
    let a = h
        .attach(&mut ca, &term, request_options(80, 24, false))
        .await?;
    let c = h
        .attach(&mut cc, &term, request_options(80, 24, false))
        .await?;
    let b = h
        .attach(&mut cb, &term, request_options(80, 24, false))
        .await?;
    let w = h
        .attach(&mut cw, &term, request_options(80, 24, true))
        .await?;
    h.request_ownership(&mut ca, a).await?;
    h.focus(&mut cb, b).await?;
    h.focus(&mut cc, c).await?;
    h.focus(&mut cw, w).await?;
    h.settle(&mut [&mut ca, &mut cb, &mut cc, &mut cw]).await?;
    let (mut last, owner) = heard(&cw, w)?;
    ensure!(
        "ownership",
        owner == Some(a),
        "A should own the terminal first; the watcher heard {owner:?}"
    );

    h.detach(&mut ca, a).await?;
    h.settle(&mut [&mut ca, &mut cb, &mut cc, &mut cw]).await?;
    let (generation, owner) = heard(&cw, w)?;
    ensure!(
        "hand-off",
        owner == Some(c) && generation > last,
        "A leaving should hand off to C, the latest focus; got {owner:?} at {generation:?}"
    );
    last = generation;

    h.drop_connection(&mut cc);
    h.settle(&mut [&mut ca, &mut cb, &mut cc, &mut cw]).await?;
    let (generation, owner) = heard(&cw, w)?;
    ensure!(
        "hand-off",
        owner == Some(b) && generation > last,
        "C dropping should hand off to B; got {owner:?} at {generation:?}"
    );
    last = generation;

    h.detach(&mut cb, b).await?;
    h.settle(&mut [&mut ca, &mut cb, &mut cc, &mut cw]).await?;
    let (generation, owner) = heard(&cw, w)?;
    ensure!(
        "hand-off",
        owner.is_none() && generation > last,
        "with no candidate left the owner should be none; got {owner:?} at {generation:?}"
    );
    h.finish(&[&ca, &cb, &cc, &cw])
}

/// Case 39.
pub async fn ownership_changes_while_a_pair_is_reopening<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut ca = h.client();
    let mut cb = h.client();
    let (a, b) = owner_and_second(&mut h, &term, &mut ca, &mut cb, (100, 30)).await?;
    h.journal();

    h.target.fail_half(&term, 0, Half::Input)?;
    h.type_input(&mut cb, b, b"x").await?;
    h.settle(&mut [&mut ca, &mut cb]).await?;

    let view = ca
        .view(a)
        .ok_or_else(|| Failure::new("harness", "no view of A"))?;
    ensure!(
        "new-epoch",
        view.started.len() == 2,
        "A's pair should have reopened with one new Started; got {}",
        view.started.len()
    );
    ensure!(
        "owner",
        heard(&ca, a)?.1 == Some(b) && heard(&cb, b)?.1 == Some(b),
        "B should own the terminal; A heard {:?}",
        heard(&ca, a)
    );
    let journal = h.journal();
    ensure!(
        "focus",
        !journal.contains(&BackendCommand::Focus { attachment: a }),
        "A was focused after it lost ownership; journal {journal:?}"
    );
    let size = h.target.pty_size(&term)?;
    ensure!(
        "pty-size",
        size == (100, 30),
        "the PTY is {size:?}, expected B's size (100, 30)"
    );
    h.finish(&[&ca, &cb])
}
