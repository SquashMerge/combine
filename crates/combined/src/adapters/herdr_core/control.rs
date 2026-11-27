#![cfg(feature = "test-support")]
//! The handle a test uses to drive and observe a [`HerdrCoreBackend`](super::HerdrCoreBackend):
//! the adapter's counterpart of the fake's `FakeControl`.
//!
//! It injects faults the real core can't be asked for (a half that fails, opens that fail,
//! old-generation events after a rebuild, output held back after the adapter received it) and
//! reports what the adapter did. Nothing here is compiled without `test-support`.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use combine_protocol::BackendGeneration;
use tokio::sync::mpsc::UnboundedSender;

use super::attachment;
use crate::ports::{AttachEvent, Half};

type EventTx = UnboundedSender<(BackendGeneration, AttachEvent)>;

/// Size events the raw recording keeps.
const RAW_SIZE_EVENTS: usize = 40;

/// How long after a RESIZE, FOCUS or FACTS frame the adapter still counts as busy.
const BUSY_WINDOW: Duration = Duration::from_millis(50);

/// A pair's state, as `pairs()` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairStatus {
    pub generation: u64,
    /// Both halves are open and the pair takes requests.
    pub open: bool,
}

#[derive(Default)]
struct Held {
    on: bool,
    items: Vec<(EventTx, u64, AttachEvent)>,
}

struct Late {
    data: Vec<u8>,
    resized: (u16, u16),
}

#[derive(Default)]
struct Inner {
    attachments: Vec<(String, Weak<attachment::Shared>)>,
    failing_opens: HashMap<String, u32>,
    automatic: HashMap<String, Vec<Vec<u8>>>,
    held: HashMap<String, Held>,
    late: Option<Late>,
    late_delivered: usize,
    touched: Option<Instant>,
    raw: Option<File>,
    raw_dir: Option<PathBuf>,
    raw_sizes: usize,
    subscribes: usize,
    watch_credit_stalled: bool,
}

/// What the adapter shares with its handle.
#[derive(Default)]
pub(super) struct Shared {
    inner: Mutex<Inner>,
    /// Attaches in flight: opened but not yet registered.
    opening: AtomicUsize,
}

/// Marks an initial attach as busy from the first call to the core until the attachment is
/// registered. Dropping it, on success or error, ends the mark.
pub(super) struct OpenGuard(Arc<Shared>);

impl Drop for OpenGuard {
    fn drop(&mut self) {
        self.0.opening.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn opening(self: &Arc<Self>) -> OpenGuard {
        self.opening.fetch_add(1, Ordering::SeqCst);
        OpenGuard(Arc::clone(self))
    }

    pub(super) fn register(&self, address: &str, attachment: Weak<attachment::Shared>) {
        self.lock()
            .attachments
            .push((address.to_string(), attachment));
    }

    /// True once per injected failure.
    pub(super) fn take_open_failure(&self, address: &str) -> bool {
        let mut inner = self.lock();
        match inner.failing_opens.get_mut(address) {
            Some(count) if *count > 0 => {
                *count -= 1;
                true
            }
            _ => false,
        }
    }

    /// Takes the event while its terminal's output is held; hands it back otherwise.
    pub(super) fn hold(
        &self,
        address: &str,
        tx: &EventTx,
        generation: u64,
        event: AttachEvent,
    ) -> Option<AttachEvent> {
        let mut inner = self.lock();
        match inner.held.get_mut(address) {
            Some(held) if held.on => {
                held.items.push((tx.clone(), generation, event));
                None
            }
            _ => Some(event),
        }
    }

    pub(super) fn late(&self) -> Option<(Vec<u8>, (u16, u16))> {
        self.lock()
            .late
            .as_ref()
            .map(|late| (late.data.clone(), late.resized))
    }

    pub(super) fn count_late(&self, events: usize) {
        self.lock().late_delivered += events;
    }

    pub(super) fn touch(&self) {
        self.lock().touched = Some(Instant::now());
    }

    pub(super) fn record_automatic(&self, address: &str, bytes: &[u8]) {
        self.lock()
            .automatic
            .entry(address.to_string())
            .or_default()
            .push(bytes.to_vec());
    }

    pub(super) fn watch_credit_stalled(&self) -> bool {
        self.lock().watch_credit_stalled
    }

    /// Keeps every line, except size events past the first `RAW_SIZE_EVENTS`: a flood of them
    /// teaches nothing and would bloat the committed fixture.
    pub(super) fn record_raw(&self, line: &str) {
        let mut inner = self.lock();
        if line.contains("\"type\":\"size\"") {
            inner.raw_sizes += 1;
            if inner.raw_sizes > RAW_SIZE_EVENTS {
                return;
            }
        }
        if let Some(file) = inner.raw.as_mut() {
            let _ = writeln!(file, "{line}");
        }
    }

    /// The first two subscription snapshots are saved whole as `subscribe-1.json` and
    /// `subscribe-2.json`.
    pub(super) fn record_subscribe(&self, result: &serde_json::Value) {
        let mut inner = self.lock();
        inner.subscribes += 1;
        let number = inner.subscribes;
        if let (Some(dir), true) = (&inner.raw_dir, number <= 2) {
            let _ = std::fs::write(
                dir.join(format!("subscribe-{number}.json")),
                result.to_string(),
            );
        }
    }

    fn live(&self, address: &str) -> Vec<Arc<attachment::Shared>> {
        self.lock()
            .attachments
            .iter()
            .filter(|(a, _)| a == address)
            .filter_map(|(_, weak)| weak.upgrade())
            .collect()
    }
}

/// Output the adapter should receive; see [`HerdrCoreControl::expect_delivery`].
pub struct DeliveryWait {
    goals: Vec<(Arc<attachment::Shared>, u64)>,
}

impl DeliveryWait {
    pub fn reached(&self) -> bool {
        self.goals
            .iter()
            .all(|(shared, goal)| shared.received() >= *goal)
    }
}

/// Drives and observes one backend. Clones share it.
#[derive(Clone)]
pub struct HerdrCoreControl {
    shared: Arc<Shared>,
}

impl HerdrCoreControl {
    pub(super) fn new(shared: Arc<Shared>) -> Self {
        HerdrCoreControl { shared }
    }

    // observation

    /// Attachments opened on the terminal, in attach order, closed ones included.
    pub fn attachments(&self, address: &str) -> usize {
        self.shared
            .lock()
            .attachments
            .iter()
            .filter(|(a, _)| a == address)
            .count()
    }

    /// Halves of the terminal's attachments that are open right now.
    pub fn open_halves(&self, address: &str) -> usize {
        self.shared
            .live(address)
            .iter()
            .map(|shared| shared.halves_open())
            .sum()
    }

    /// Automatic replies the adapter sent to the core for the terminal, in order.
    pub fn automatic_input(&self, address: &str) -> Vec<Vec<u8>> {
        self.shared
            .lock()
            .automatic
            .get(address)
            .cloned()
            .unwrap_or_default()
    }

    /// Every open or reopening pair, by terminal address, in attach order.
    pub fn pairs(&self) -> Vec<(String, PairStatus)> {
        let attachments = self.shared.lock().attachments.clone();
        attachments
            .into_iter()
            .filter_map(|(address, weak)| {
                let shared = weak.upgrade()?;
                shared.pair_status().map(|status| (address, status))
            })
            .collect()
    }

    /// A RESIZE, FOCUS or FACTS frame passed under 50 ms ago.
    pub fn recently_touched(&self) -> bool {
        self.shared
            .lock()
            .touched
            .is_some_and(|at| at.elapsed() < BUSY_WINDOW)
    }

    /// Notes where each attachment on the terminal must get to for `len` more bytes of program
    /// output to have reached the adapter. An attachment whose credit can't cover them (a
    /// paused client) isn't waited for: the core holds that output back.
    pub fn expect_delivery(&self, address: &str, len: usize) -> DeliveryWait {
        let goals = self
            .shared
            .live(address)
            .into_iter()
            .filter_map(|shared| shared.delivery_goal(len).map(|goal| (shared, goal)))
            .collect();
        DeliveryWait { goals }
    }

    /// An attach is in flight, a pair is reopening, or a RESIZE, FOCUS or FACTS frame passed
    /// under 50 ms ago.
    pub fn busy(&self) -> bool {
        self.shared.opening.load(Ordering::SeqCst) > 0
            || self.recently_touched()
            || self.pairs().iter().any(|(_, status)| !status.open)
    }

    /// Old-generation events delivered after rebuilds since the fault was switched on.
    pub fn late_events_delivered(&self) -> usize {
        self.shared.lock().late_delivered
    }

    // injection

    /// Fails one half of the `index`th attachment on the terminal, as if its connection broke.
    /// False when there is no such attachment or it is closed.
    pub fn fail_half(&self, address: &str, index: usize, half: Half) -> bool {
        let attachment = {
            let inner = self.shared.lock();
            inner
                .attachments
                .iter()
                .filter(|(a, _)| a == address)
                .nth(index)
                .and_then(|(_, weak)| weak.upgrade())
        };
        attachment.is_some_and(|shared| shared.fail(half))
    }

    /// The next `count` opens of the terminal fail halfway.
    pub fn fail_next_opens(&self, address: &str, count: u32) {
        self.shared
            .lock()
            .failing_opens
            .insert(address.to_string(), count);
    }

    /// After each rebuild the adapter sends `data` and a `Resized` to `resized` under the
    /// previous generation, as a slow backend would.
    pub fn set_late_old_generation_events(&self, on: bool, data: &[u8], resized: (u16, u16)) {
        let mut inner = self.shared.lock();
        inner.late = on.then(|| Late {
            data: data.to_vec(),
            resized,
        });
        inner.late_delivered = 0;
    }

    /// Holds every event the terminal's attachments would send, in order, until released.
    pub fn hold_output(&self, address: &str, on: bool) {
        let mut inner = self.shared.lock();
        let held = inner.held.entry(address.to_string()).or_default();
        held.on = on;
        if !on {
            for (tx, generation, event) in held.items.drain(..) {
                let _ = tx.send((BackendGeneration(generation), event));
            }
        }
    }

    /// Whether the held output contains `needle`.
    pub fn held_contains(&self, address: &str, needle: &[u8]) -> bool {
        let inner = self.shared.lock();
        let Some(held) = inner.held.get(address) else {
            return false;
        };
        let bytes: Vec<u8> = held
            .items
            .iter()
            .filter_map(|(_, _, event)| match event {
                AttachEvent::Data(data) => Some(data.as_slice()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect();
        !needle.is_empty() && bytes.windows(needle.len()).any(|window| window == needle)
    }

    // capture

    /// Appends every raw event line the watcher reads to `<dir>/events.jsonl`, and saves the first
    /// two subscription snapshots there.
    pub fn record_raw_events(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(dir.join("events.jsonl"))?;
        let mut inner = self.shared.lock();
        inner.raw = Some(file);
        inner.raw_dir = Some(dir.to_path_buf());
        inner.subscribes = 0;
        inner.raw_sizes = 0;
        Ok(())
    }

    /// While on, the watcher grants the core no credit, so the core's queue for it grows.
    pub fn stall_watch_credit(&self, on: bool) {
        self.shared.lock().watch_credit_stalled = on;
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::super::connection::Connection;
    use super::*;

    fn control() -> (Arc<Shared>, HerdrCoreControl) {
        let shared = Arc::new(Shared::default());
        let control = HerdrCoreControl::new(Arc::clone(&shared));
        (shared, control)
    }

    /// A core that waits `delay`, then answers hello or hangs up.
    async fn slow_core(mut server: tokio::io::DuplexStream, delay: Duration, answer: bool) {
        let mut byte = [0u8; 1];
        while server.read_exact(&mut byte).await.is_ok() && byte[0] != b'\n' {}
        tokio::time::sleep(delay).await;
        if answer {
            let hello = r#"{"jsonrpc":"2.0","id":0,"result":{"generation":1,"version":"0.1.21","framed":true,"core":{"id":"x","name":"y"},"methods":["terminal.attach","terminal.list","terminal.run","terminal.kill","events.subscribe"],"features":[]}}"#;
            let _ = server.write_all(format!("{hello}\n").as_bytes()).await;
            std::future::pending::<()>().await;
        }
    }

    /// An attach whose core is slow keeps `busy()` true for the whole wait, and the mark ends
    /// when the open fails, with no count left behind.
    #[tokio::test]
    async fn an_attach_in_flight_is_busy_until_it_ends() {
        let (shared, control) = control();
        assert!(!control.busy());
        let (client, server) = tokio::io::duplex(4_096);
        let core = tokio::spawn(slow_core(server, Duration::from_millis(100), true));
        let guard = shared.opening();
        let open = tokio::spawn(Connection::from_stream(client));
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(control.busy(), "busy while the core has not answered");
        let opened = open.await.expect("task").expect("opened");
        assert!(control.busy(), "busy until the attachment is registered");
        drop(guard);
        assert!(!control.busy());
        drop(opened);
        core.abort();

        let (client, server) = tokio::io::duplex(4_096);
        let core = tokio::spawn(slow_core(server, Duration::from_millis(20), false));
        let failed = {
            let _guard = shared.opening();
            assert!(control.busy());
            Connection::from_stream(client).await.is_err()
        };
        assert!(failed, "the core hung up before hello");
        assert!(!control.busy(), "an open that failed releases the mark");
        core.await.expect("core");
    }
}
