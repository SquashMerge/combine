//! `events.subscribe` over its own connection: credit, line splitting, and re-subscribing when
//! the subscription is lost.
//!
//! A `missed` line means the subscriber fell behind: subscribe again on a new connection and
//! report the difference. A lost connection waits one interval, subscribes again and reports
//! the difference, with `Restarted` for every survivor. After the configured number of
//! failures every cached running terminal is reported `Lost`, and the retries go on.

use std::sync::Arc;

use futures::stream::{BoxStream, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;

use super::connection::{CallError, Connection};
use super::events::{parse_snapshot, DiffMode, Snapshot, WatchState};
use super::frame::{self, Frame, Kind};
use super::Env;
use crate::model::TermEvent;
use crate::ports::{BackendError, BackendResult};

struct Subscription {
    connection: Connection,
    frames: mpsc::UnboundedReceiver<Frame>,
    channel: Option<u32>,
    snapshot: Snapshot,
}

async fn subscribe(env: &Env) -> BackendResult<Subscription> {
    let (connection, frames) = Connection::open(&env.config.socket).await?;
    let result = connection
        .call("events.subscribe", json!({}))
        .await
        .map_err(|error: CallError| error.to_failure())?;
    env.record_subscribe(&result);
    let snapshot = parse_snapshot(&result)
        .ok_or_else(|| BackendError::Failed("unreadable events.subscribe result".to_string()))?;
    Ok(Subscription {
        connection,
        frames,
        channel: snapshot.channel,
        snapshot,
    })
}

enum Ended {
    Lost,
    Missed,
    Gone,
}

/// Subscribes, seeds the cache from the snapshot (no events) and returns the stream.
pub(super) async fn start(env: Arc<Env>) -> BackendResult<BoxStream<'static, TermEvent>> {
    let first = subscribe(&env).await?;
    let mut state = WatchState::default();
    state.seed(&first.snapshot);
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(env, first, state, tx));
    Ok(futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|event| (event, rx))
    })
    .boxed())
}

async fn run(
    env: Arc<Env>,
    first: Subscription,
    mut state: WatchState,
    tx: mpsc::UnboundedSender<TermEvent>,
) {
    let mut subscription = first;
    loop {
        let ended = pump(&env, &mut subscription, &mut state, &tx).await;
        let mode = match ended {
            Ended::Gone => return,
            Ended::Missed => DiffMode::Missed,
            Ended::Lost => {
                tokio::time::sleep(env.config.reopen_interval).await;
                DiffMode::Reconnect
            }
        };
        drop(subscription);
        let mut failures = 0u32;
        subscription = loop {
            match subscribe(&env).await {
                Ok(next) => break next,
                Err(_) => {
                    failures += 1;
                    if failures == env.config.reopen_attempts {
                        for event in state.lose_all() {
                            if tx.send(event).is_err() {
                                return;
                            }
                        }
                    }
                    tokio::time::sleep(env.config.reopen_interval).await;
                    if tx.is_closed() {
                        return;
                    }
                }
            }
        };
        for event in state.reseed(&subscription.snapshot, mode) {
            if tx.send(event).is_err() {
                return;
            }
        }
    }
}

/// Reads the subscription until it ends.
async fn pump(
    env: &Env,
    subscription: &mut Subscription,
    state: &mut WatchState,
    tx: &mpsc::UnboundedSender<TermEvent>,
) -> Ended {
    let mut buffer: Vec<u8> = Vec::new();
    let writer = subscription.connection.sender();
    loop {
        let frame = tokio::select! {
            frame = subscription.frames.recv() => frame,
            () = tx.closed() => return Ended::Gone,
        };
        let Some(frame) = frame else {
            return Ended::Lost;
        };
        if Some(frame.channel) != subscription.channel {
            continue;
        }
        match frame.kind {
            Kind::Close => return Ended::Lost,
            Kind::Data => {}
            _ => continue,
        }
        if !env.watch_credit_stalled() {
            if let (Some(writer), Some(channel)) = (&writer, subscription.channel) {
                let bytes = u32::try_from(frame.payload.len()).unwrap_or(u32::MAX);
                let _ = writer.try_send(frame::credit(channel, bytes));
            }
        }
        buffer.extend_from_slice(&frame.payload);
        while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = buffer.drain(..=end).collect();
            let Ok(line) = std::str::from_utf8(&line[..end]) else {
                continue;
            };
            env.record_raw(line);
            let applied = state.apply_line(line);
            for event in applied.events {
                if tx.send(event).is_err() {
                    return Ended::Gone;
                }
            }
            if applied.missed {
                return Ended::Missed;
            }
        }
    }
}
