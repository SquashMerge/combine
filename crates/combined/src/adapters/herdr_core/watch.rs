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

use super::connection::{reserve_slot, CallError, Connection};
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

/// Reads the subscription until it ends. Credit that finds the writer queue full is owed and
/// sent when a slot frees, so every byte read is credited once.
async fn pump(
    env: &Env,
    subscription: &mut Subscription,
    state: &mut WatchState,
    tx: &mpsc::UnboundedSender<TermEvent>,
) -> Ended {
    let mut buffer: Vec<u8> = Vec::new();
    let writer = subscription.connection.sender();
    let mut owed = 0u64;
    loop {
        let frame = tokio::select! {
            frame = subscription.frames.recv() => frame,
            slot = reserve_slot(writer.as_ref()), if owed > 0 => {
                match (slot, subscription.channel) {
                    (Some(slot), Some(channel)) => {
                        let bytes = u32::try_from(owed).unwrap_or(u32::MAX);
                        owed -= u64::from(bytes);
                        slot.send(frame::credit(channel, bytes));
                    }
                    _ => owed = 0,
                }
                continue;
            }
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
        if !env.watch_credit_stalled() && writer.is_some() && subscription.channel.is_some() {
            owed += frame.payload.len() as u64;
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

/// The watch CREDIT goes into the bounded writer queue, and what the full queue can't take
/// is owed. These tests drive `pump` against an in-memory core that honours credit windows,
/// with the core not reading the client's side while it bursts.
#[cfg(all(test, feature = "test-support"))]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use serde_json::{json, Value};
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    use super::*;
    use crate::adapters::herdr_core::attachment::Fidelities;
    use crate::adapters::herdr_core::connection::WRITER_QUEUE_FRAMES;
    use crate::adapters::herdr_core::{control, HerdrCoreConfig};

    const CHANNEL: u32 = 1;

    /// What the model core saw.
    #[derive(Debug)]
    struct Outcome {
        sent: usize,
        credited: usize,
        /// The window was exhausted and no credit came back.
        stalled: bool,
    }

    async fn read_line(server: &mut DuplexStream) {
        let mut byte = [0u8; 1];
        while server.read_exact(&mut byte).await.is_ok() {
            if byte[0] == b'\n' {
                break;
            }
        }
    }

    async fn read_frame(server: &mut DuplexStream) -> Option<(u32, u8, Vec<u8>)> {
        let mut header = [0u8; frame::HEADER_LEN];
        server.read_exact(&mut header).await.ok()?;
        let channel = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let length = u32::from_le_bytes([header[5], header[6], header[7], header[8]]);
        let mut body = vec![0u8; length as usize];
        server.read_exact(&mut body).await.ok()?;
        Some((channel, header[4], body))
    }

    /// A core that answers hello and `events.subscribe`, then sends one-byte DATA frames within
    /// a credit window of `window` bytes, never reading while it sends. After each burst it
    /// reads credit until the line goes quiet.
    async fn model_core(mut server: DuplexStream, window: usize, total: usize) -> Outcome {
        read_line(&mut server).await;
        let hello = json!({"jsonrpc": "2.0", "id": 0, "result": {
            "generation": 1, "version": "0.1.21", "framed": true,
            "core": {"id": "x", "name": "y"},
            "methods": ["terminal.attach", "terminal.list", "terminal.run", "terminal.kill",
                        "events.subscribe"],
            "features": []}});
        server
            .write_all(format!("{hello}\n").as_bytes())
            .await
            .expect("hello");
        let (_, _, body) = read_frame(&mut server).await.expect("subscribe");
        let request: Value = serde_json::from_slice(&body).expect("json");
        let reply = json!({"jsonrpc": "2.0", "id": request["id"],
            "result": {"seq": 0, "channel": CHANNEL, "terminals": []}});
        server
            .write_all(&frame::rpc(reply.to_string().as_bytes()))
            .await
            .expect("reply");

        let (mut sent, mut credited, mut window) = (0usize, 0usize, window);
        let mut stalled = false;
        while sent < total {
            let burst = window.min(total - sent);
            for _ in 0..burst {
                server
                    .write_all(&frame::encode(CHANNEL, Kind::Data, b"\n"))
                    .await
                    .expect("data");
            }
            sent += burst;
            window -= burst;
            let mut got = 0usize;
            while let Ok(Some((channel, kind, body))) =
                tokio::time::timeout(Duration::from_millis(300), read_frame(&mut server)).await
            {
                if channel == CHANNEL && kind == Kind::Credit as u8 {
                    got += u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
                }
            }
            credited += got;
            window += got;
            if window == 0 && sent < total {
                stalled = true;
                break;
            }
        }
        Outcome {
            sent,
            credited,
            stalled,
        }
    }

    /// Runs `pump` on the client end against the model core and returns what the core saw.
    async fn run(window: usize, total: usize) -> Outcome {
        // A 64-byte pipe: the client's writer blocks after a few credit frames while the
        // core is not reading, so the rest queue up and then drop.
        let (client, server) = tokio::io::duplex(64);
        let core = tokio::spawn(model_core(server, window, total));
        let (connection, frames) = Connection::from_stream(client).await.expect("open");
        let result = connection
            .call("events.subscribe", json!({}))
            .await
            .expect("subscribe");
        let snapshot = parse_snapshot(&result).expect("snapshot");
        let mut subscription = Subscription {
            connection,
            frames,
            channel: snapshot.channel,
            snapshot,
        };
        let env = Env {
            config: HerdrCoreConfig::new("/nonexistent".into()),
            fence: Mutex::new(HashMap::new()),
            fidelities: Mutex::new(Fidelities::default()),
            automatic_feature: false,
            control: Arc::new(control::Shared::default()),
        };
        let mut state = WatchState::default();
        let (tx, _rx) = mpsc::unbounded_channel();
        let pump = tokio::spawn(async move {
            pump(&env, &mut subscription, &mut state, &tx).await;
        });
        let outcome = core.await.expect("core");
        // The core dropped its end: the pump sees the end of the stream.
        tokio::time::timeout(Duration::from_secs(5), pump)
            .await
            .expect("pump ends with the stream")
            .expect("pump");
        outcome
    }

    /// A burst larger than the writer queue never stalls the stream: owed credit is sent as the
    /// queue drains.
    #[tokio::test(flavor = "multi_thread")]
    async fn dropped_watch_credit_does_not_stall_the_stream() {
        let window = WRITER_QUEUE_FRAMES * 3;
        let outcome = run(window, window * 3).await;
        assert!(!outcome.stalled, "the stream stalled: {outcome:?}");
        assert_eq!(outcome.sent, window * 3, "{outcome:?}");
    }

    /// Every DATA byte read is credited back, even when the writer queue is full while the
    /// core bursts: the credit is owed until the queue has room.
    #[tokio::test(flavor = "multi_thread")]
    async fn every_watch_data_byte_is_credited_back() {
        let window = WRITER_QUEUE_FRAMES * 3;
        let outcome = run(window, window).await;
        assert_eq!(
            outcome.credited, outcome.sent,
            "credit was dropped: {outcome:?}"
        );
    }
}
