//! One attachment: the handle the daemon calls, and the supervisor that keeps its pair of
//! viewers alive.
//!
//! The handle never awaits a round trip. It locks the state, lets `fencing` decide, and
//! `try_send`s encoded frames. The supervisor owns the connections: it turns the display
//! half's frames into events, credits and discards the input half's output, and rebuilds the
//! pair when a half ends.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use combine_protocol::{BackendGeneration, CloseReason, Display, Fidelity, OwnershipGeneration};
use futures::stream::StreamExt;
use tokio::sync::mpsc;

#[cfg(feature = "test-support")]
use super::control::PairHalf;
use super::fencing::{Effect, Facts, Fence, Out, Phase, Request};
use super::frame::{self, Frame, Kind};
use super::jsonrpc::{self, fidelity_of};
use super::pair::{self, Half, Halves, OpenError, OpenParams};
use super::Env;
#[cfg(feature = "test-support")]
use super::INITIAL_CREDIT;
use crate::ports::{AttachEvent, AttachEvents, Attachment, BackendError, BackendResult};

/// Two halves that end within this window of each other were ended by one cause: the core.
const CORE_WIDE_EOF_WINDOW: Duration = Duration::from_millis(50);

type EventTx = mpsc::UnboundedSender<(BackendGeneration, AttachEvent)>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Remembers which terminals have been inexact. A terminal that was inexact stays inexact: an
/// exact report after a restart doesn't undo the loss. Keyed by address and creation time, so
/// an address the core reuses for a new terminal starts clean.
#[derive(Debug, Default)]
pub(super) struct Fidelities {
    inexact: HashMap<(String, u64), ()>,
}

impl Fidelities {
    pub(super) fn observe(&mut self, address: &str, created_ms: u64, screen: &str) -> Fidelity {
        let key = (address.to_string(), created_ms);
        if fidelity_of(screen) == Fidelity::Inexact {
            self.inexact.insert(key, ());
            return Fidelity::Inexact;
        }
        if self.inexact.contains_key(&key) {
            Fidelity::Inexact
        } else {
            Fidelity::Exact
        }
    }
}

/// How long to wait before the first reopen attempt: one interval when the core ended both
/// halves, or when there is only one half to tell, and none when a single half failed.
pub(super) fn first_reopen_delay(
    interval: Duration,
    first_ended: Instant,
    other_ended: Option<Instant>,
    watch_only: bool,
) -> Duration {
    let core_wide = other_ended
        .is_some_and(|other| other.saturating_duration_since(first_ended) <= CORE_WIDE_EOF_WINDOW);
    if watch_only || core_wide {
        interval
    } else {
        Duration::ZERO
    }
}

/// What a supervisor does after a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Next {
    Continue,
    Rebuild,
    Finish,
}

#[derive(Debug, PartialEq)]
pub(super) struct Reaction {
    pub events: Vec<AttachEvent>,
    /// Bytes to credit back to the input half.
    pub credit_input: u32,
    pub next: Next,
}

impl Reaction {
    fn next(next: Next) -> Self {
        Reaction {
            events: Vec::new(),
            credit_input: 0,
            next,
        }
    }
}

/// What the supervisor has seen of the current pair.
#[derive(Debug, Default)]
pub(super) struct PairRun {
    /// The input half got CLOSE: the program ended.
    program_ended: bool,
}

impl PairRun {
    /// `None` is the display half's end without a CLOSE.
    pub(super) fn on_display(&mut self, frame: Option<Frame>) -> Reaction {
        let Some(frame) = frame else {
            return if self.program_ended {
                Reaction {
                    events: vec![AttachEvent::Closed(CloseReason::Exited)],
                    credit_input: 0,
                    next: Next::Finish,
                }
            } else {
                Reaction::next(Next::Rebuild)
            };
        };
        match frame.kind {
            Kind::Data => Reaction {
                events: vec![AttachEvent::Data(frame.payload)],
                credit_input: 0,
                next: Next::Continue,
            },
            Kind::Resize => match frame::parse_resize(&frame.payload) {
                Some((cols, rows)) => Reaction {
                    events: vec![AttachEvent::Resized { cols, rows }],
                    credit_input: 0,
                    next: Next::Continue,
                },
                None => Reaction::next(Next::Continue),
            },
            Kind::Close => Reaction {
                events: vec![jsonrpc::close_event(&frame.payload)],
                credit_input: 0,
                next: Next::Finish,
            },
            _ => Reaction::next(Next::Continue),
        }
    }

    /// The input half's own output is credited and discarded; its CLOSE only marks that the
    /// program ended, and the display half's CLOSE says so to the daemon.
    pub(super) fn on_input(&mut self, frame: Option<Frame>) -> Reaction {
        let Some(frame) = frame else {
            return Reaction::next(if self.program_ended {
                Next::Continue
            } else {
                Next::Rebuild
            });
        };
        match frame.kind {
            Kind::Data => Reaction {
                events: Vec::new(),
                credit_input: u32::try_from(frame.payload.len()).unwrap_or(u32::MAX),
                next: Next::Continue,
            },
            Kind::Close => {
                self.program_ended = true;
                Reaction::next(Next::Continue)
            }
            _ => Reaction::next(Next::Continue),
        }
    }
}

enum Command {
    Detach,
    /// A half failed: rebuild the pair at once.
    Fail,
}

/// Where a request's frames go.
struct Writers {
    display: Option<mpsc::Sender<Vec<u8>>>,
    display_channel: u32,
    input: Option<mpsc::Sender<Vec<u8>>>,
    input_channel: u32,
}

struct State {
    fence: Fence,
    writers: Writers,
    fidelity: Fidelity,
    ordered: bool,
    display_open: bool,
    input_open: bool,
    /// Bytes the core may still send on the display half: its allowance, plus the credit given,
    /// less the bytes received. Lets a test target tell when output is credit-bound.
    #[cfg(feature = "test-support")]
    window: i64,
    /// Display-half DATA bytes received over the attachment's life.
    #[cfg(feature = "test-support")]
    received: u64,
}

/// What the handle, the supervisor and the control handle share.
pub(super) struct Shared {
    address: String,
    env: Arc<Env>,
    history: u32,
    state: Mutex<State>,
    events: Mutex<Option<EventTx>>,
    commands: mpsc::UnboundedSender<Command>,
}

impl Shared {
    fn emit(&self, generation: u64, event: AttachEvent) {
        let guard = lock(&self.events);
        let Some(tx) = guard.as_ref() else {
            return;
        };
        #[cfg(feature = "test-support")]
        let Some(event) = self.env.control.hold(&self.address, tx, generation, event) else {
            return;
        };
        let _ = tx.send((BackendGeneration(generation), event));
    }

    fn end_events(&self) {
        *lock(&self.events) = None;
    }

    fn generation(&self) -> u64 {
        lock(&self.state).fence.generation
    }

    /// Sends what the fence decided, in order. Called with the state locked.
    fn perform(&self, state: &State, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Emit(event) => self.emit(state.fence.generation, event),
                Effect::Send(out) => {
                    let Some(writer) = &state.writers.input else {
                        continue;
                    };
                    let channel = state.writers.input_channel;
                    #[cfg(feature = "test-support")]
                    if matches!(out, Out::Resize(..) | Out::Facts(_) | Out::Focus) {
                        self.env.control.touch();
                    }
                    let frames = match out {
                        Out::Data(bytes) => frame::data_frames(channel, &bytes),
                        Out::Automatic(bytes) => frame::automatic_frames(channel, &bytes),
                        Out::Resize(cols, rows) => vec![frame::resize(channel, cols, rows)],
                        Out::Facts(display) => vec![frame::facts(
                            channel,
                            jsonrpc::display_json(&display).to_string().as_bytes(),
                        )],
                        Out::Focus => vec![frame::focus(channel, true)],
                    };
                    for bytes in frames {
                        let _ = writer.try_send(bytes);
                    }
                }
            }
        }
    }

    fn request(&self, ownership: OwnershipGeneration, request: Request) -> BackendResult<()> {
        let mut state = lock(&self.state);
        if state.fence.phase == Phase::Ready && !state.fence.watch_only {
            match &state.writers.input {
                Some(writer) if writer.is_closed() => {
                    state.fence.begin_reopen();
                    let _ = self.commands.send(Command::Fail);
                }
                Some(writer) if writer.capacity() < Fence::frames_needed(&request) => {
                    return Err(BackendError::Failed("input queue full".to_string()));
                }
                _ => {}
            }
        }
        let effects = {
            let mut fence = lock(&self.env.fence);
            let highest = fence.entry(self.address.clone()).or_insert(0);
            state.fence.request(highest, ownership.0, request)?
        };
        self.perform(&state, effects);
        Ok(())
    }

    #[cfg(feature = "test-support")]
    pub(super) fn halves_open(&self) -> usize {
        let state = lock(&self.state);
        usize::from(state.display_open) + usize::from(state.input_open)
    }

    #[cfg(feature = "test-support")]
    pub(super) fn pair_status(&self) -> Option<super::control::PairStatus> {
        let state = lock(&self.state);
        if state.fence.phase == Phase::Closed {
            return None;
        }
        Some(super::control::PairStatus {
            generation: state.fence.generation,
            open: state.fence.phase == Phase::Ready
                && state.display_open
                && (state.input_open || state.fence.watch_only),
        })
    }

    /// Where `received` will stand once `len` more bytes arrive, if the core can send them
    /// all: the pair is ready and its credit covers them.
    #[cfg(feature = "test-support")]
    pub(super) fn delivery_goal(&self, len: usize) -> Option<u64> {
        let state = lock(&self.state);
        let ready = state.fence.phase == Phase::Ready && state.display_open;
        (ready && state.window >= len as i64).then_some(state.received + len as u64)
    }

    #[cfg(feature = "test-support")]
    pub(super) fn received(&self) -> u64 {
        lock(&self.state).received
    }

    /// Fails one half. False when the attachment is closed.
    #[cfg(feature = "test-support")]
    pub(super) fn fail(&self, half: PairHalf) -> bool {
        {
            let state = lock(&self.state);
            if state.fence.phase == Phase::Closed {
                return false;
            }
            if state.fence.watch_only && half == PairHalf::Input {
                return true;
            }
        }
        self.commands.send(Command::Fail).is_ok()
    }
}

/// The handle `attach` returns.
pub(super) struct HerdrCoreAttachment {
    shared: Arc<Shared>,
    events: Option<AttachEvents>,
}

/// Starts the supervisor for a pair that is open, and returns the handle. The first event is
/// `Started` at generation 1.
pub(super) fn start(
    env: Arc<Env>,
    address: &str,
    opts: &crate::ports::AttachOptions,
    halves: Halves,
) -> HerdrCoreAttachment {
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    let requested = Facts {
        size: (opts.cols, opts.rows),
        display: opts.display.clone(),
    };
    let mut fence = Fence::new(opts.observe, requested);
    fence.automatic = halves.automatic;
    let fidelity = lock(&env.fidelities).observe(address, halves.created_ms, &halves.screen);
    let state = State {
        fence,
        writers: writers_of(&halves),
        fidelity,
        ordered: halves.ordered,
        display_open: true,
        input_open: halves.input.is_some(),
        #[cfg(feature = "test-support")]
        window: i64::from(INITIAL_CREDIT),
        #[cfg(feature = "test-support")]
        received: 0,
    };
    let shared = Arc::new(Shared {
        address: address.to_string(),
        env: Arc::clone(&env),
        history: opts.history,
        state: Mutex::new(state),
        events: Mutex::new(Some(events_tx)),
        commands: commands_tx,
    });
    #[cfg(feature = "test-support")]
    env.control.register(address, Arc::downgrade(&shared));
    shared.emit(
        1,
        AttachEvent::Started {
            cols: halves.size.0,
            rows: halves.size.1,
            fidelity,
            ordered_resizes: halves.ordered,
        },
    );
    tokio::spawn(supervise(Arc::clone(&shared), halves, commands_rx));
    HerdrCoreAttachment {
        shared,
        events: Some(
            futures::stream::unfold(events_rx, |mut rx| async move {
                rx.recv().await.map(|item| (item, rx))
            })
            .boxed(),
        ),
    }
}

fn writers_of(halves: &Halves) -> Writers {
    Writers {
        display: halves.display.connection.sender(),
        display_channel: halves.display.channel,
        input: halves
            .input
            .as_ref()
            .and_then(|input| input.connection.sender()),
        input_channel: halves.input.as_ref().map_or(0, |input| input.channel),
    }
}

enum Ended {
    Detached,
    Finished,
    /// `display_first`: the display half ended first; the input half may follow.
    Rebuild {
        injected: bool,
        display_first: bool,
    },
}

async fn recv_input(input: &mut Option<Half>) -> Option<Frame> {
    match input {
        Some(half) => half.frames.recv().await,
        None => std::future::pending().await,
    }
}

/// Runs one pair until it ends. `halves` keeps the connections for the caller to close.
async fn run_pair(
    shared: &Shared,
    halves: &mut Halves,
    commands: &mut mpsc::UnboundedReceiver<Command>,
) -> Ended {
    let mut run = PairRun::default();
    let mut input_done = false;
    let prefetched = std::mem::take(&mut halves.prefetched);
    for frame in prefetched {
        let reaction = run.on_display(Some(frame));
        if let Some(ended) = apply(shared, halves, reaction, true) {
            return ended;
        }
    }
    loop {
        let (reaction, from_display) = tokio::select! {
            frame = halves.display.frames.recv() => (run.on_display(frame), true),
            frame = recv_input(&mut halves.input), if !input_done && halves.input.is_some() => {
                if frame.is_none() {
                    input_done = true;
                }
                (run.on_input(frame), false)
            }
            command = commands.recv() => {
                return match command {
                    Some(Command::Fail) => Ended::Rebuild {
                        injected: true,
                        display_first: true,
                    },
                    Some(Command::Detach) | None => Ended::Detached,
                };
            }
        };
        if let Some(ended) = apply(shared, halves, reaction, from_display) {
            return ended;
        }
    }
}

/// Emits a reaction's events and credits the input half. `Some` when the pair is over.
fn apply(
    shared: &Shared,
    halves: &Halves,
    reaction: Reaction,
    from_display: bool,
) -> Option<Ended> {
    let generation = shared.generation();
    for event in reaction.events {
        #[cfg(feature = "test-support")]
        if let AttachEvent::Data(bytes) = &event {
            let mut state = lock(&shared.state);
            state.received += bytes.len() as u64;
            state.window -= bytes.len() as i64;
        }
        #[cfg(feature = "test-support")]
        if matches!(event, AttachEvent::Resized { .. }) {
            shared.env.control.touch();
        }
        shared.emit(generation, event);
    }
    if reaction.credit_input > 0 {
        if let Some(input) = &halves.input {
            if let Some(writer) = input.connection.sender() {
                let _ = writer.try_send(frame::credit(input.channel, reaction.credit_input));
            }
        }
    }
    match reaction.next {
        Next::Continue => None,
        Next::Finish => Some(Ended::Finished),
        Next::Rebuild => Some(Ended::Rebuild {
            injected: false,
            display_first: from_display,
        }),
    }
}

async fn supervise(
    shared: Arc<Shared>,
    mut halves: Halves,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    loop {
        let ended = run_pair(&shared, &mut halves, &mut commands).await;
        match ended {
            Ended::Detached => {
                finish(
                    &shared,
                    Some(halves),
                    Some(AttachEvent::Closed(CloseReason::Detached)),
                );
                return;
            }
            Ended::Finished => {
                finish(&shared, Some(halves), None);
                return;
            }
            Ended::Rebuild {
                injected,
                display_first,
            } => match rebuild(&shared, halves, &mut commands, injected, display_first).await {
                Some(next) => halves = next,
                None => return,
            },
        }
    }
}

/// Closes the pair and ends the event stream, after `last` if there is one.
fn finish(shared: &Shared, halves: Option<Halves>, last: Option<AttachEvent>) {
    {
        let mut state = lock(&shared.state);
        state.fence.close();
        state.writers.display = None;
        state.writers.input = None;
        state.display_open = false;
        state.input_open = false;
    }
    if let Some(event) = last {
        let generation = shared.generation();
        shared.emit(generation, event);
    }
    shared.end_events();
    if let Some(halves) = halves {
        halves.close();
    }
}

/// Waits briefly for the other half to end too, which says the core ended both.
async fn other_half_end(halves: &mut Halves, first_was_display: bool) -> Option<Instant> {
    let wait = async {
        if first_was_display {
            while let Some(frame) = recv_input(&mut halves.input).await {
                if frame.kind == Kind::Close {
                    break;
                }
            }
        } else {
            while let Some(frame) = halves.display.frames.recv().await {
                if frame.kind == Kind::Close {
                    break;
                }
            }
        }
        Instant::now()
    };
    tokio::time::timeout(CORE_WIDE_EOF_WINDOW, wait).await.ok()
}

async fn rebuild(
    shared: &Arc<Shared>,
    mut old: Halves,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    injected: bool,
    display_first: bool,
) -> Option<Halves> {
    let first_ended = Instant::now();
    let watch_only = {
        let mut state = lock(&shared.state);
        state.fence.begin_reopen();
        state.writers.display = None;
        state.writers.input = None;
        state.display_open = false;
        state.input_open = false;
        state.fence.watch_only
    };
    let other_ended = if injected || watch_only || old.input.is_none() {
        None
    } else {
        other_half_end(&mut old, display_first).await
    };
    old.close();

    let interval = shared.env.config.reopen_interval;
    let mut wait = first_reopen_delay(interval, first_ended, other_ended, watch_only);
    for _ in 0..shared.env.config.reopen_attempts {
        let slept = tokio::time::sleep(wait);
        tokio::pin!(slept);
        loop {
            tokio::select! {
                () = &mut slept => break,
                command = commands.recv() => match command {
                    Some(Command::Fail) => {}
                    Some(Command::Detach) | None => {
                        finish(shared, None, Some(AttachEvent::Closed(CloseReason::Detached)));
                        return None;
                    }
                },
            }
        }
        wait = interval;
        let (size, display) = {
            let state = lock(&shared.state);
            (
                state.fence.requested.size,
                state.fence.requested.display.clone(),
            )
        };
        let params = OpenParams {
            address: &shared.address,
            size,
            display: &display,
            history: shared.history,
            resumed: true,
            watch_only,
        };
        match pair::open(&shared.env, &params).await {
            Ok(halves) => {
                install(shared, &halves);
                return Some(halves);
            }
            Err(OpenError::Finished) => {
                finish(shared, None, Some(AttachEvent::Closed(CloseReason::Exited)));
                return None;
            }
            Err(OpenError::NotFound | OpenError::Failed(_)) => {}
        }
    }
    finish(shared, None, Some(AttachEvent::Lost));
    None
}

/// Swaps in a rebuilt pair under one lock: the next generation, `Started`, the late events a
/// test asked for, then the queued requests in order.
fn install(shared: &Shared, halves: &Halves) {
    let mut state = lock(&shared.state);
    let fidelity =
        lock(&shared.env.fidelities).observe(&shared.address, halves.created_ms, &halves.screen);
    state.fidelity = fidelity;
    state.ordered = halves.ordered;
    state.writers = writers_of(halves);
    state.display_open = true;
    state.input_open = halves.input.is_some();
    #[cfg(feature = "test-support")]
    {
        state.window = i64::from(INITIAL_CREDIT);
    }
    state.fence.automatic = halves.automatic;
    let effects = {
        let mut table = lock(&shared.env.fence);
        let highest = table.entry(shared.address.clone()).or_insert(0);
        state.fence.finish_reopen(*highest)
    };
    let generation = state.fence.generation;
    shared.emit(
        generation,
        AttachEvent::Started {
            cols: halves.size.0,
            rows: halves.size.1,
            fidelity,
            ordered_resizes: halves.ordered,
        },
    );
    #[cfg(feature = "test-support")]
    if let Some((data, (cols, rows))) = shared.env.control.late() {
        shared.emit(generation - 1, AttachEvent::Data(data));
        shared.emit(generation - 1, AttachEvent::Resized { cols, rows });
        shared.env.control.count_late(2);
    }
    shared.perform(&state, effects);
}

#[async_trait]
impl Attachment for HerdrCoreAttachment {
    fn fidelity(&self) -> Fidelity {
        lock(&self.shared.state).fidelity
    }

    fn ordered_resizes(&self) -> bool {
        lock(&self.shared.state).ordered
    }

    fn events(&mut self) -> AttachEvents {
        self.events
            .take()
            .unwrap_or_else(|| futures::stream::empty().boxed())
    }

    async fn write(&mut self, ownership: OwnershipGeneration, bytes: &[u8]) -> BackendResult<()> {
        self.shared
            .request(ownership, Request::Write(bytes.to_vec()))
    }

    async fn write_automatic(
        &mut self,
        generation: BackendGeneration,
        bytes: &[u8],
    ) -> BackendResult<()> {
        let mut state = lock(&self.shared.state);
        let effects = state.fence.write_automatic(generation, bytes)?;
        #[cfg(feature = "test-support")]
        if !effects.is_empty() {
            self.shared
                .env
                .control
                .record_automatic(&self.shared.address, bytes);
        }
        self.shared.perform(&state, effects);
        Ok(())
    }

    async fn resize(
        &mut self,
        ownership: OwnershipGeneration,
        cols: u16,
        rows: u16,
    ) -> BackendResult<()> {
        self.shared.request(ownership, Request::Resize(cols, rows))
    }

    async fn focus(&mut self, ownership: OwnershipGeneration) -> BackendResult<()> {
        self.shared.request(ownership, Request::Focus)
    }

    async fn set_display(
        &mut self,
        ownership: OwnershipGeneration,
        display: Display,
    ) -> BackendResult<()> {
        self.shared.request(ownership, Request::SetDisplay(display))
    }

    async fn credit(&mut self, bytes: u32) -> BackendResult<()> {
        #[cfg_attr(not(feature = "test-support"), allow(unused_mut))]
        let mut state = lock(&self.shared.state);
        if !state.fence.credit_gate()? {
            return Ok(());
        }
        if let Some(writer) = &state.writers.display {
            writer
                .try_send(frame::credit(state.writers.display_channel, bytes))
                .map_err(|_| BackendError::Failed("input queue full".to_string()))?;
            #[cfg(feature = "test-support")]
            {
                state.window += i64::from(bytes);
            }
        }
        Ok(())
    }

    async fn detach(self: Box<Self>) -> BackendResult<()> {
        let _ = self.shared.commands.send(Command::Detach);
        Ok(())
    }
}

impl Drop for HerdrCoreAttachment {
    fn drop(&mut self) {
        let _ = self.shared.commands.send(Command::Detach);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(kind: Kind, payload: &[u8]) -> Frame {
        Frame {
            channel: 1,
            kind,
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn input_half_output_is_credited_and_discarded() {
        let mut run = PairRun::default();
        let reaction = run.on_input(Some(frame(Kind::Data, b"0123456789")));
        assert_eq!(
            reaction,
            Reaction {
                events: Vec::new(),
                credit_input: 10,
                next: Next::Continue
            }
        );
    }

    #[test]
    fn display_data_becomes_data_events_in_frame_order() {
        let mut run = PairRun::default();
        let first = run.on_display(Some(frame(Kind::Data, b"one")));
        let second = run.on_display(Some(frame(Kind::Data, b"two")));
        assert_eq!(first.events, vec![AttachEvent::Data(b"one".to_vec())]);
        assert_eq!(second.events, vec![AttachEvent::Data(b"two".to_vec())]);
        assert_eq!(second.next, Next::Continue);
    }

    #[test]
    fn display_resize_becomes_resized() {
        let mut run = PairRun::default();
        let reaction = run.on_display(Some(frame(Kind::Resize, &[100, 0, 30, 0])));
        assert_eq!(
            reaction.events,
            vec![AttachEvent::Resized {
                cols: 100,
                rows: 30
            }]
        );
    }

    #[test]
    fn input_close_waits_for_the_display_close() {
        let mut run = PairRun::default();
        let reaction = run.on_input(Some(frame(Kind::Close, b"{}")));
        assert_eq!((reaction.events.len(), reaction.next), (0, Next::Continue));
        let end = run.on_input(None);
        assert_eq!(
            end.next,
            Next::Continue,
            "the program ended: nothing to rebuild"
        );
        let display = run.on_display(None);
        assert_eq!(
            display.events,
            vec![AttachEvent::Closed(CloseReason::Exited)]
        );
        assert_eq!(display.next, Next::Finish);
        let mut plain = PairRun::default();
        assert_eq!(plain.on_display(None).next, Next::Rebuild);
        assert_eq!(plain.on_input(None).next, Next::Rebuild);
    }

    #[test]
    fn core_wide_eof_delays_the_first_reopen() {
        let interval = Duration::from_millis(250);
        let start = Instant::now();
        let soon = Some(start + Duration::from_millis(10));
        let late = Some(start + Duration::from_millis(400));
        assert_eq!(first_reopen_delay(interval, start, soon, false), interval);
        assert_eq!(
            first_reopen_delay(interval, start, late, false),
            Duration::ZERO
        );
        assert_eq!(
            first_reopen_delay(interval, start, None, false),
            Duration::ZERO
        );
        assert_eq!(first_reopen_delay(interval, start, None, true), interval);
    }

    #[test]
    fn inexact_terminal_stays_inexact_after_an_exact_report() {
        let mut fidelities = Fidelities::default();
        assert_eq!(fidelities.observe("t1", 5, "exact"), Fidelity::Exact);
        assert_eq!(fidelities.observe("t1", 5, "inexact"), Fidelity::Inexact);
        assert_eq!(fidelities.observe("t1", 5, "exact"), Fidelity::Inexact);
    }

    #[test]
    fn a_reused_address_is_not_tainted() {
        let mut fidelities = Fidelities::default();
        assert_eq!(fidelities.observe("t1", 5, "inexact"), Fidelity::Inexact);
        assert_eq!(fidelities.observe("t1", 9, "exact"), Fidelity::Exact);
    }
}
