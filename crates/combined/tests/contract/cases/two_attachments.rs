//! Cases 16-17: two attachments on one terminal. Each operation goes to the attachment that
//! sent it, and a rebuilt attachment comes back at the size it last asked for.

use combine_protocol::{AttachRequestOptions, AttachmentId, Display, OwnershipGeneration};
use combined::core::{BackendCommand, CoreConfig};
use combined::ports::Half;
use futures::FutureExt;

use crate::common::{display, request_options};
use crate::contract::client::View;
use crate::contract::{Case, ClientSim, Failure, Fault, Harness, Outcome, Target};

pub fn cases<T: Target>() -> Vec<Case<T>> {
    vec![
        Case {
            name: "two_attachments_route_operations_and_show_the_pty_size",
            needs: &[Fault::Observation],
            heavy: false,
            run: |t| two_attachments_route_operations_and_show_the_pty_size(t).boxed_local(),
        },
        Case {
            name: "rebuilt_attachment_reapplies_its_latest_requested_size",
            needs: &[Fault::HalfFailure, Fault::Observation],
            heavy: false,
            run: |t| rebuilt_attachment_reapplies_its_latest_requested_size(t).boxed_local(),
        },
    ]
}

fn view_of(c: &ClientSim, attachment: AttachmentId) -> Result<&View, Failure> {
    c.view(attachment)
        .ok_or_else(|| Failure::new("harness", "the client has no view of the attachment"))
}

fn options(cols: u16, rows: u16, display: Display) -> AttachRequestOptions {
    AttachRequestOptions {
        display,
        ..request_options(cols, rows, false)
    }
}

fn sent_to(journal: &[BackendCommand], attachment: AttachmentId) -> Vec<BackendCommand> {
    journal
        .iter()
        .filter(|command| command.attachment() == attachment)
        .cloned()
        .collect()
}

/// What the backend should be told when an attachment takes over: its display, its size, the
/// focus, then the typed bytes.
fn takeover(
    attachment: AttachmentId,
    display: Display,
    size: (u16, u16),
    typed: &[u8],
) -> Vec<BackendCommand> {
    vec![
        BackendCommand::SetDisplay {
            ownership: OwnershipGeneration(0),
            attachment,
            display,
        },
        BackendCommand::Resize {
            ownership: OwnershipGeneration(0),
            attachment,
            cols: size.0,
            rows: size.1,
        },
        BackendCommand::Focus {
            attachment,
            ownership: OwnershipGeneration(0),
        },
        BackendCommand::Write {
            ownership: OwnershipGeneration(0),
            attachment,
            bytes: typed.to_vec(),
        },
    ]
}

/// Case 16.
pub async fn two_attachments_route_operations_and_show_the_pty_size<T: Target>(
    t: &mut T,
) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let display_a = display();
    let mut display_b = display();
    display_b.foreground = "#00ff00".to_string();
    let mut a = h.client();
    let mut b = h.client();
    let att_a = h
        .attach(&mut a, &term, options(80, 24, display_a.clone()))
        .await?;
    let att_b = h
        .attach(&mut b, &term, options(100, 30, display_b.clone()))
        .await?;
    h.settle(&mut [&mut a, &mut b]).await?;
    h.journal();

    h.type_input(&mut a, att_a, b"a").await?;
    h.settle(&mut [&mut a, &mut b]).await?;
    let journal = h.journal();
    let to_a = sent_to(&journal, att_a);
    let expected = takeover(att_a, display_a, (80, 24), b"a");
    ensure!(
        "route-a",
        to_a == expected,
        "A's journal should be {expected:?}; it is {to_a:?}"
    );
    let to_b = sent_to(&journal, att_b);
    ensure!(
        "route-a",
        to_b.is_empty(),
        "A's typing sent B commands: {to_b:?}"
    );

    h.resize(&mut b, att_b, 110, 35).await?;
    h.type_input(&mut b, att_b, b"b").await?;
    h.settle(&mut [&mut a, &mut b]).await?;
    let to_b = sent_to(&h.journal(), att_b);
    let expected = takeover(att_b, display_b, (110, 35), b"b");
    ensure!(
        "route-b",
        to_b == expected,
        "B's journal should be {expected:?}; it is {to_b:?}"
    );

    for (name, client, attachment) in [("A", &a, att_a), ("B", &b, att_b)] {
        let size = view_of(client, attachment)?.size;
        ensure!(
            "pty-size",
            size == (110, 35),
            "{name}'s view ends at {size:?}, expected (110, 35)"
        );
    }
    let size = h.target.pty_size(&term)?;
    ensure!(
        "pty-size",
        size == (110, 35),
        "the PTY is {size:?}, expected (110, 35)"
    );
    h.finish(&[&a, &b])
}

/// Case 17.
pub async fn rebuilt_attachment_reapplies_its_latest_requested_size<T: Target>(
    t: &mut T,
) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut a = h.client();
    let att_a = h
        .attach(&mut a, &term, request_options(80, 24, false))
        .await?;
    h.request_ownership(&mut a, att_a).await?;
    h.settle(&mut [&mut a]).await?;
    h.resize(&mut a, att_a, 120, 40).await?;
    h.settle(&mut [&mut a]).await?;
    h.journal();

    h.target.fail_half(&term, 0, Half::Input)?;
    h.settle(&mut [&mut a]).await?;
    let started = view_of(&a, att_a)?.started.len();
    ensure!(
        "restarted",
        started == 2,
        "the rebuilt attachment should start one new epoch; it has {started} Started"
    );
    let focused: Vec<AttachmentId> = h
        .journal()
        .iter()
        .filter(|command| matches!(command, BackendCommand::Focus { .. }))
        .map(BackendCommand::attachment)
        .collect();
    ensure!(
        "focus",
        focused == [att_a],
        "the journal should show Focus to A only; it shows {focused:?}"
    );
    let size = h.target.pty_size(&term)?;
    ensure!(
        "pty-size",
        size == (120, 40),
        "the PTY is {size:?}, expected the requested (120, 40)"
    );
    h.finish(&[&a])
}
