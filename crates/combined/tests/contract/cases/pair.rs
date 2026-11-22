//! Cases 29-33: an attachment's input and display halves fail, or fail to open, and the
//! daemon still gives each client one clean new epoch.

use combine_protocol::{DaemonMessage, Epoch, ErrorCode, OwnershipGeneration};
use combined::core::{BackendCommand, CoreConfig};
use combined::testing::Half;
use futures::FutureExt;

use crate::common::{display, request_options};
use crate::contract::client::ReadPolicy;
use crate::contract::{Case, ClientSim, Failure, Fault, Harness, Outcome, Target};

pub fn cases<T: Target>() -> Vec<Case<T>> {
    vec![
        Case {
            name: "input_half_failure_starts_one_new_epoch",
            needs: &[Fault::HalfFailure, Fault::LateOldGenerationEvents],
            heavy: false,
            run: |t| input_half_failure_starts_one_new_epoch(t).boxed_local(),
        },
        Case {
            name: "display_half_failure_starts_one_new_epoch",
            needs: &[Fault::HalfFailure, Fault::LateOldGenerationEvents],
            heavy: false,
            run: |t| display_half_failure_starts_one_new_epoch(t).boxed_local(),
        },
        Case {
            name: "reopen_failing_halfway_starts_one_new_epoch",
            needs: &[
                Fault::HalfFailure,
                Fault::OpenFailure,
                Fault::LateOldGenerationEvents,
            ],
            heavy: false,
            run: |t| reopen_failing_halfway_starts_one_new_epoch(t).boxed_local(),
        },
        Case {
            name: "first_attach_failing_halfway_is_attach_failed",
            needs: &[Fault::OpenFailure, Fault::Observation],
            heavy: false,
            run: |t| first_attach_failing_halfway_is_attach_failed(t).boxed_local(),
        },
        Case {
            name: "requests_while_reopening_apply_in_order",
            needs: &[Fault::HalfFailure, Fault::Observation],
            heavy: false,
            run: |t| requests_while_reopening_apply_in_order(t).boxed_local(),
        },
    ]
}

/// How the owner's pair is made to fail in cases 29-31.
#[derive(Clone, Copy)]
enum Trigger {
    Half(Half),
    /// The first reopen attempt fails too, then a half fails again.
    ReopenFailsOnce,
}

/// Takes every message the daemon has for the client, applying each.
async fn drain<T: Target>(h: &mut Harness<'_, T>, c: &mut ClientSim) -> Vec<DaemonMessage> {
    let mut messages = Vec::new();
    while let Some(message) = h.read_step(c, true).await {
        messages.push(message);
    }
    messages
}

/// The sizes of the `Resized` messages, in order.
fn resized_sizes(messages: &[DaemonMessage]) -> Vec<(u16, u16)> {
    messages
        .iter()
        .filter_map(|message| match message {
            DaemonMessage::Resized(resized) => Some((resized.cols, resized.rows)),
            _ => None,
        })
        .collect()
}

/// The shared body of cases 29-31: the owner's pair fails and, with the backend sending old
/// generation events after the new one starts, the client gets exactly one new epoch and
/// none of those events.
async fn one_new_epoch<T: Target>(t: &mut T, trigger: Trigger) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    h.target.set_late_old_generation_events(true)?;
    let term = h.spawn("p", 80, 24).await?;
    let mut c = h.client();
    let att = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    h.request_ownership(&mut c, att).await?;
    h.settle(&mut [&mut c]).await?;
    h.mark(&term, &mut [&mut c]).await?;
    ensure!(
        "owner",
        c.view(att).is_some_and(|view| view.owns()),
        "the attachment should own the terminal before its pair fails"
    );

    c.read = ReadPolicy::Paused;
    h.journal();
    match trigger {
        Trigger::Half(half) => h.target.fail_half(&term, 0, half)?,
        Trigger::ReopenFailsOnce => {
            h.target.fail_next_opens(&term, 1)?;
            h.target.fail_half(&term, 0, Half::Display)?;
        }
    }
    h.settle(&mut [&mut c]).await?;
    let messages = drain(&mut h, &mut c).await;

    let started = messages
        .iter()
        .filter(|message| matches!(message, DaemonMessage::Started(_)))
        .count();
    ensure!(
        "new-epoch",
        started == 1,
        "expected exactly one new Started; the client was sent {started}"
    );
    let view = c
        .view(att)
        .ok_or_else(|| Failure::new("harness", "the client has no view of the attachment"))?;
    ensure!(
        "new-epoch",
        view.started.len() == 2 && view.epoch == Some(Epoch(2)),
        "expected epoch 2 after one new Started; got {} Started, epoch {:?}",
        view.started.len(),
        view.epoch
    );
    let late = messages.iter().any(|message| {
        matches!(message, DaemonMessage::Data(data)
            if data.bytes.windows(6).any(|window| window == b"<late>"))
    });
    ensure!(
        "late-output",
        !late,
        "output from the old generation reached the client: {messages:?}"
    );
    let sizes = resized_sizes(&messages);
    ensure!(
        "late-resized",
        sizes.len() <= 1 && sizes.iter().all(|size| *size == (80, 24)),
        "the only Resized the new epoch may have is the owner's reapplied 80x24; got {sizes:?}"
    );
    ensure!(
        "owner",
        view.owns(),
        "ownership changed across the rebuild: {:?}",
        view.ownership
    );
    let journal = h.journal();
    ensure!(
        "focus",
        journal.contains(&BackendCommand::Focus {
            attachment: att,
            ownership: OwnershipGeneration(0)
        }),
        "the owner's rebuilt pair was not focused; journal {journal:?}"
    );

    c.read = ReadPolicy::Auto;
    h.mark(&term, &mut [&mut c]).await?;
    h.emit(&term, b"after").await?;
    h.settle(&mut [&mut c]).await?;
    let heard = c.view(att).map(|view| view.bytes()).unwrap_or_default();
    ensure!(
        "output-after",
        heard == b"after",
        "output after the rebuild should arrive; got {:?}",
        String::from_utf8_lossy(&heard)
    );
    h.finish(&[&c])
}

/// Case 29.
pub async fn input_half_failure_starts_one_new_epoch<T: Target>(t: &mut T) -> Outcome {
    one_new_epoch(t, Trigger::Half(Half::Input)).await
}

/// Case 30.
pub async fn display_half_failure_starts_one_new_epoch<T: Target>(t: &mut T) -> Outcome {
    one_new_epoch(t, Trigger::Half(Half::Display)).await
}

/// Case 31.
pub async fn reopen_failing_halfway_starts_one_new_epoch<T: Target>(t: &mut T) -> Outcome {
    one_new_epoch(t, Trigger::ReopenFailsOnce).await
}

/// Case 32.
pub async fn first_attach_failing_halfway_is_attach_failed<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    h.target.fail_next_opens(&term, 1)?;
    let mut c = h.client();
    let answer = h
        .try_attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    ensure!(
        "attach-failed",
        answer
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::AttachFailed),
        "an attach failing halfway should be attach_failed; got {answer:?}"
    );
    ensure!(
        "attach-failed",
        !c.controls
            .iter()
            .any(|message| matches!(message, DaemonMessage::Attached(_))),
        "the client was told it is attached: {:?}",
        c.controls
    );
    let halves = h.target.open_halves(&term)?;
    ensure!(
        "open-halves",
        halves == 0,
        "{halves} half(s) are still open after the failed attach"
    );
    h.finish(&[&c])
}

/// Case 33.
pub async fn requests_while_reopening_apply_in_order<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut c = h.client();
    let att = h
        .attach(&mut c, &term, request_options(80, 24, false))
        .await?;
    h.request_ownership(&mut c, att).await?;
    h.settle(&mut [&mut c]).await?;
    h.mark(&term, &mut [&mut c]).await?;

    c.read = ReadPolicy::Paused;
    h.journal();
    h.target.fail_half(&term, 0, Half::Input)?;
    h.resize(&mut c, att, 100, 30).await?;
    h.resize(&mut c, att, 110, 35).await?;
    let mut changed = display();
    changed.foreground = "#00ff00".to_string();
    h.set_display(&mut c, att, changed.clone()).await?;
    h.type_input(&mut c, att, b"x").await?;
    h.settle(&mut [&mut c]).await?;
    let messages = drain(&mut h, &mut c).await;

    let journal = h.journal();
    let requested: Vec<&BackendCommand> = journal
        .iter()
        .filter(|command| {
            command.attachment() == att
                && !matches!(
                    command,
                    BackendCommand::Credit { .. } | BackendCommand::Focus { .. }
                )
        })
        .collect();
    let expected = [
        BackendCommand::Resize {
            ownership: OwnershipGeneration(0),
            attachment: att,
            cols: 100,
            rows: 30,
        },
        BackendCommand::Resize {
            ownership: OwnershipGeneration(0),
            attachment: att,
            cols: 110,
            rows: 35,
        },
        BackendCommand::SetDisplay {
            ownership: OwnershipGeneration(0),
            attachment: att,
            display: changed,
        },
        BackendCommand::Write {
            ownership: OwnershipGeneration(0),
            attachment: att,
            bytes: b"x".to_vec(),
        },
    ];
    ensure!(
        "request-order",
        requested == expected.iter().collect::<Vec<_>>(),
        "the requests reached the backend as {requested:?}"
    );

    let size = h.target.pty_size(&term)?;
    ensure!(
        "pty-size",
        size == (110, 35),
        "the PTY is {size:?}, expected (110, 35), the last resize"
    );
    let typed = h.target.typed_input(&term)?;
    ensure!(
        "typed",
        typed == b"x",
        "the program was typed {:?}, expected \"x\"",
        String::from_utf8_lossy(&typed)
    );
    let echoed: Vec<u8> = messages
        .iter()
        .filter_map(|message| match message {
            DaemonMessage::Data(data) => Some(data.bytes.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect();
    ensure!(
        "echo",
        echoed.ends_with(b"x"),
        "x should be echoed after the reopen; the client heard {:?}",
        String::from_utf8_lossy(&echoed)
    );
    h.finish(&[&c])
}
