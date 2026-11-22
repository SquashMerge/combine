//! Cases 40-42: the terminal's queries are answered once, by the owner, for live output only.

use combine_protocol::{
    AttachmentId, AutomaticReply, ClientMessage, Epoch, OwnershipGeneration, Seq, TermId,
};
use combined::core::CoreConfig;
use futures::FutureExt;

use crate::common::request_options;
use crate::contract::client::{ReadPolicy, ReplyPolicy};
use crate::contract::{Case, ClientSim, Failure, Fault, Harness, Outcome, Target};

/// The query the simulated client answers: a cursor position report request.
const QUERY: &[u8] = b"\x1b[?6n";

pub fn cases<T: Target>() -> Vec<Case<T>> {
    vec![
        Case {
            name: "only_the_owners_reply_is_forwarded_once",
            needs: &[Fault::Observation],
            heavy: false,
            run: |t| only_the_owners_reply_is_forwarded_once(t).boxed_local(),
        },
        Case {
            name: "replayed_then_live_query_gets_one_reply_for_the_live_one",
            needs: &[Fault::Observation],
            heavy: false,
            run: |t| replayed_then_live_query_gets_one_reply_for_the_live_one(t).boxed_local(),
        },
        Case {
            name: "stale_replies_are_dropped_silently",
            needs: &[Fault::Observation, Fault::BackendRestart],
            heavy: false,
            run: |t| stale_replies_are_dropped_silently(t).boxed_local(),
        },
    ]
}

/// What the simulated client sends for the first query in the `Data` event at `seq`.
fn reply_to(seq: Seq) -> Vec<u8> {
    format!("\x1b[?{};1R", seq.0).into_bytes()
}

/// The seq of the last event the client applied for the attachment.
fn last_seq(c: &ClientSim, attachment: AttachmentId) -> Result<Seq, Failure> {
    c.view(attachment)
        .and_then(|view| view.last_applied)
        .map(|cursor| cursor.seq)
        .ok_or_else(|| Failure::new("harness", "the client has applied nothing"))
}

/// Attaches an owner that has finished its first read.
async fn owner<T: Target>(
    h: &mut Harness<'_, T>,
    term: &TermId,
    c: &mut ClientSim,
) -> Result<AttachmentId, Failure> {
    let att = h.attach(c, term, request_options(80, 24, false)).await?;
    h.request_ownership(c, att).await?;
    h.settle(&mut [&mut *c]).await?;
    Ok(att)
}

/// Case 40.
pub async fn only_the_owners_reply_is_forwarded_once<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut ca = h.client();
    let a = owner(&mut h, &term, &mut ca).await?;
    let mut cb = h.client();
    cb.replies = ReplyPolicy::Always;
    h.attach(&mut cb, &term, request_options(80, 24, false))
        .await?;

    h.emit(&term, QUERY).await?;
    h.settle(&mut [&mut ca, &mut cb]).await?;
    let expected = reply_to(last_seq(&ca, a)?);
    let forwarded = h.target.automatic_input(&term)?;
    ensure!(
        "reply",
        forwarded == [expected.clone()],
        "exactly the owner's reply {:?} should reach the program; got {:?}",
        String::from_utf8_lossy(&expected),
        forwarded
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect::<Vec<_>>()
    );
    ensure!(
        "no-error",
        ca.error_codes().is_empty() && cb.error_codes().is_empty(),
        "the replies caused errors: {:?} and {:?}",
        ca.error_codes(),
        cb.error_codes()
    );
    h.finish(&[&ca, &cb])
}

/// Case 41.
pub async fn replayed_then_live_query_gets_one_reply_for_the_live_one<T: Target>(
    t: &mut T,
) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut c = h.client();
    let a = owner(&mut h, &term, &mut c).await?;

    c.read = ReadPolicy::Paused;
    h.emit(&term, QUERY).await?;
    h.reconnect(&mut c).await?;
    h.emit(&term, QUERY).await?;
    c.read = ReadPolicy::Auto;
    h.settle(&mut [&mut c]).await?;

    let view = c
        .view(a)
        .ok_or_else(|| Failure::new("harness", "the client has no view of the attachment"))?;
    let through = view.replay_through.map(|cursor| cursor.seq);
    let live = last_seq(&c, a)?;
    ensure!(
        "replay",
        through.is_some_and(|through| through < live),
        "the first query should be replayed and the second live; replay_through {through:?}, last {live:?}"
    );
    let forwarded = h.target.automatic_input(&term)?;
    ensure!(
        "reply",
        forwarded == [reply_to(live)],
        "exactly the live query's reply should reach the program; got {:?}",
        forwarded
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect::<Vec<_>>()
    );
    ensure!(
        "no-error",
        c.error_codes().is_empty(),
        "the replies caused errors: {:?}",
        c.error_codes()
    );
    h.finish(&[&c])
}

/// Case 42.
pub async fn stale_replies_are_dropped_silently<T: Target>(t: &mut T) -> Outcome {
    let mut h = Harness::new(t, CoreConfig::default())?;
    let term = h.spawn("p", 80, 24).await?;
    let mut c = h.client();
    c.replies = ReplyPolicy::Never;
    let a = owner(&mut h, &term, &mut c).await?;
    let epoch = c.view(a).and_then(|view| view.epoch);
    let (generation, _) = c
        .view(a)
        .and_then(|view| view.ownership)
        .ok_or_else(|| Failure::new("harness", "the client has heard no ownership"))?;
    let (Some(epoch), Some(older)) = (epoch, generation.0.checked_sub(1)) else {
        return Err(Failure::new(
            "harness",
            "the attachment has no epoch or no earlier generation",
        ));
    };
    let reply = |epoch: Epoch, generation: OwnershipGeneration| {
        ClientMessage::AutomaticReply(AutomaticReply {
            attachment: a,
            epoch,
            ownership_generation: generation,
            bytes: b"\x1b[?1;1R".to_vec(),
        })
    };

    h.send(&mut c, reply(epoch, OwnershipGeneration(older)))
        .await?;
    h.settle(&mut [&mut c]).await?;
    let forwarded = h.target.automatic_input(&term)?;
    ensure!(
        "stale-generation",
        forwarded.is_empty(),
        "a reply under generation {older} was forwarded: {forwarded:?}"
    );

    h.target.restart_backend().await?;
    h.settle(&mut [&mut c]).await?;
    ensure!(
        "restarted",
        c.view(a).is_some_and(|view| view.started.len() == 2),
        "the backend restart should have started a new epoch"
    );
    h.send(&mut c, reply(epoch, generation)).await?;
    h.settle(&mut [&mut c]).await?;
    let forwarded = h.target.automatic_input(&term)?;
    ensure!(
        "stale-epoch",
        forwarded.is_empty(),
        "a reply from the previous epoch was forwarded: {forwarded:?}"
    );
    ensure!(
        "no-error",
        c.error_codes().is_empty(),
        "dropping the stale replies caused errors: {:?}",
        c.error_codes()
    );

    let current = c
        .view(a)
        .and_then(|view| view.epoch)
        .ok_or_else(|| Failure::new("harness", "the attachment has no epoch"))?;
    h.send(&mut c, reply(current, generation)).await?;
    h.settle(&mut [&mut c]).await?;
    let forwarded = h.target.automatic_input(&term)?;
    ensure!(
        "current-reply",
        forwarded.len() == 1,
        "a reply under the current epoch and generation should be forwarded once; got {forwarded:?}"
    );
    h.finish(&[&c])
}
