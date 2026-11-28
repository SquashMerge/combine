//! Cases 18-21: what a client sees when the daemon or the backend restarts.

use combine_protocol::{DaemonMessage, Epoch, Fidelity, OwnershipGeneration};
use combined::core::{BackendCommand, CoreConfig};
use combined::testing::{LATE_DATA, LATE_RESIZED};
use futures::FutureExt;

use crate::common::request_options;
use crate::contract::client::ReadPolicy;
use crate::contract::{Case, Failure, Fault, Harness, Outcome, Target};

pub fn cases<T: Target>() -> Vec<Case<T>> {
    vec![
        Case {
            name: "daemon_restart_answers_resume_with_gap_then_attaches_fresh",
            needs: &[],
            heavy: false,
            run: |t| daemon_restart_answers_resume_with_gap_then_attaches_fresh(t).boxed_local(),
        },
        Case {
            name: "backend_restart_starts_one_new_exact_epoch",
            needs: &[Fault::BackendRestart, Fault::LateOldGenerationEvents],
            heavy: false,
            run: |t| backend_restart_starts_one_new_exact_epoch(t).boxed_local(),
        },
        Case {
            name: "backend_crash_starts_one_new_inexact_epoch",
            needs: &[Fault::BackendCrash, Fault::LateOldGenerationEvents],
            heavy: false,
            run: |t| backend_crash_starts_one_new_inexact_epoch(t).boxed_local(),
        },
        Case {
            name: "inexact_terminal_stays_inexact",
            needs: &[Fault::InexactTerminal, Fault::BackendRestart],
            heavy: false,
            run: |t| inexact_terminal_stays_inexact(t).boxed_local(),
        },
    ]
}

/// Case 18.
pub async fn daemon_restart_answers_resume_with_gap_then_attaches_fresh<T: Target>(
    t: &mut T,
) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut c = h.client();
    let old = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    let second = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    h.mark(&term, &mut [&mut c]).await?;

    h.restart_daemon(&mut [&mut c]).await?;
    h.reconnect(&mut c).await?;
    for attachment in [old, second] {
        ensure!(
            "resume-gap",
            c.resume_refused(attachment),
            "resuming attachment {} after a daemon restart should be a gap with the request; got {:?}",
            attachment.0,
            c.controls
        );
    }

    let fresh = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    ensure!(
        "fresh-id",
        fresh > old && fresh > second,
        "the fresh attachment {} should have an id above every old one ({}, {})",
        fresh.0,
        old.0,
        second.0
    );
    let view = c
        .view(fresh)
        .ok_or_else(|| Failure::new("harness", "no view of the fresh attachment"))?;
    ensure!(
        "fresh-epoch",
        view.epoch == Some(Epoch(1)) && view.started.len() == 1,
        "a fresh attachment should be in epoch 1 with one Started; got {:?} with {}",
        view.epoch,
        view.started.len()
    );
    h.finish(&[&c])
}

/// The shared body of cases 19 and 20: a backend that restarts or crashes gives each
/// attachment exactly one new epoch with the stated fidelity and, with the backend sending old
/// generation events after the new one starts, none of those events.
async fn one_new_epoch<T: Target>(t: &mut T, crash: bool, fidelity: Fidelity) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut c = h.client();
    let att = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    h.type_input(&mut c, att, b"x").await?;
    h.settle(&mut [&mut c]).await?;
    h.mark(&term, &mut [&mut c]).await?;
    let before = c
        .view(att)
        .ok_or_else(|| Failure::new("harness", "no view"))?
        .ownership;
    ensure!(
        "owner",
        c.view(att).is_some_and(|view| view.owns()),
        "typing should have made the attachment the owner"
    );
    h.journal();
    h.target.set_late_old_generation_events(true)?;
    c.read = ReadPolicy::Paused;

    if crash {
        h.target.crash_backend().await?;
    } else {
        h.target.restart_backend().await?;
    }
    h.settle(&mut [&mut c]).await?;
    let messages = h.drain(&mut c).await;
    let late = messages.iter().any(|message| {
        matches!(message, DaemonMessage::Data(data)
            if data.bytes.windows(LATE_DATA.len()).any(|window| window == LATE_DATA))
    });
    ensure!(
        "late-output",
        !late,
        "output from the old generation reached the client: {messages:?}"
    );
    let late_resized = messages.iter().any(|message| {
        matches!(message, DaemonMessage::Resized(resized)
            if (resized.cols, resized.rows) == LATE_RESIZED)
    });
    ensure!(
        "late-resized",
        !late_resized,
        "the old generation's late Resized {LATE_RESIZED:?} reached the client: {messages:?}"
    );
    // After the late checks, so a late event that leaked is reported as a failure first.
    h.target.late_events_delivered()?;
    c.read = ReadPolicy::Auto;

    let view = c
        .view(att)
        .ok_or_else(|| Failure::new("harness", "no view"))?;
    ensure!(
        "new-epoch",
        view.started.len() == 2 && view.epoch == Some(Epoch(2)),
        "expected exactly one new Started (epoch 2); got {} Started, epoch {:?}",
        view.started.len(),
        view.epoch
    );
    ensure!(
        "new-epoch",
        view.started[1].backend_generation > view.started[0].backend_generation,
        "the new epoch's backend generation {:?} should be above {:?}",
        view.started[1].backend_generation,
        view.started[0].backend_generation
    );
    ensure!(
        "fidelity",
        view.fidelity == Some(fidelity),
        "the new epoch is {:?}, expected {fidelity:?}",
        view.fidelity
    );
    let held = h
        .driver
        .attachment(att)
        .map(|attachment| attachment.fidelity());
    ensure!(
        "fidelity",
        held == Some(fidelity),
        "the driver's attachment reports {held:?}, expected {fidelity:?}"
    );
    ensure!(
        "owner",
        view.ownership == before && view.owns(),
        "ownership changed across the restart: {before:?} became {:?}",
        view.ownership
    );
    let journal = h.journal();
    ensure!(
        "focus",
        journal.contains(&BackendCommand::Focus {
            attachment: att,
            ownership: OwnershipGeneration(0)
        }),
        "the owner's new pair was not focused; journal {journal:?}"
    );

    h.mark(&term, &mut [&mut c]).await?;
    h.emit(&term, b"after").await?;
    h.settle(&mut [&mut c]).await?;
    let heard = c.view(att).map(|view| view.bytes()).unwrap_or_default();
    ensure!(
        "output-after",
        heard == b"after",
        "output after the new epoch should arrive; got {:?}",
        String::from_utf8_lossy(&heard)
    );
    h.finish(&[&c])
}

/// Case 19.
pub async fn backend_restart_starts_one_new_exact_epoch<T: Target>(t: &mut T) -> Outcome {
    one_new_epoch(t, false, Fidelity::Exact).await
}

/// Case 20.
pub async fn backend_crash_starts_one_new_inexact_epoch<T: Target>(t: &mut T) -> Outcome {
    one_new_epoch(t, true, Fidelity::Inexact).await
}

/// Case 21.
pub async fn inexact_terminal_stays_inexact<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    h.target.set_inexact(&term)?;
    let mut c = h.client();
    let att = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    let first = c.view(att).and_then(|view| view.fidelity);
    ensure!(
        "fidelity",
        first == Some(Fidelity::Inexact),
        "an inexact terminal attaches as {first:?}"
    );

    h.target.restart_backend().await?;
    h.settle(&mut [&mut c]).await?;
    let view = c
        .view(att)
        .ok_or_else(|| Failure::new("harness", "no view"))?;
    ensure!(
        "fidelity",
        view.started.len() == 2
            && view
                .started
                .iter()
                .all(|started| started.fidelity == Fidelity::Inexact),
        "both epochs should be Inexact; got {:?}",
        view.started.iter().map(|s| s.fidelity).collect::<Vec<_>>()
    );
    h.finish(&[&c])
}
