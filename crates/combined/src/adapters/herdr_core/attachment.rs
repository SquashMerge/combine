//! One attachment: the handle the daemon calls, and the supervisor that keeps its pair of
//! viewers alive.
//!
//! The handle never awaits a round trip. It locks the state, reserves room in the writer
//! queue for every frame the request can send, lets `fencing` decide, and sends through the
//! reserved slots, so an accepted request is never dropped and a request that doesn't fit is
//! refused. The supervisor owns the connections: it turns the display half's frames into
//! events, credits and discards the input half's output, and rebuilds the pair when a half
//! ends. Credit that finds the queue full is owed and sent when room returns.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use combine_protocol::{BackendGeneration, CloseReason, Display, Fidelity, OwnershipGeneration};
use futures::stream::StreamExt;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, Notify};

use super::connection::reserve_slot;
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
    /// Credit the daemon granted that the display half's queue had no room for yet.
    display_owed: u64,
    /// Bytes the core may still send on the display half: its allowance, plus the credit given,
    /// less the bytes received. Lets a test target tell when output is credit-bound.
    #[cfg(feature = "test-support")]
    window: i64,
    /// Display-half DATA bytes received over the attachment's life.
    #[cfg(feature = "test-support")]
    received: u64,
}

impl State {
    /// The next CREDIT's worth of the owed display credit.
    fn take_display_credit(&mut self) -> Option<u32> {
        let bytes = u32::try_from(self.display_owed).unwrap_or(u32::MAX);
        if bytes == 0 {
            return None;
        }
        self.display_owed -= u64::from(bytes);
        #[cfg(feature = "test-support")]
        {
            self.window += i64::from(bytes);
        }
        Some(bytes)
    }
}

/// What the handle, the supervisor and the control handle share.
pub(super) struct Shared {
    address: String,
    env: Arc<Env>,
    history: u32,
    state: Mutex<State>,
    events: Mutex<Option<EventTx>>,
    commands: mpsc::UnboundedSender<Command>,
    /// Wakes the supervisor when display credit is owed.
    credit_wake: Notify,
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

    /// Emits what the fence decided and returns the frames it wants sent, in order. Called
    /// with the state locked.
    fn frames_of(&self, state: &State, effects: Vec<Effect>) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        for effect in effects {
            match effect {
                Effect::Emit(event) => self.emit(state.fence.generation, event),
                Effect::Send(out) => {
                    if state.writers.input.is_none() {
                        continue;
                    }
                    let channel = state.writers.input_channel;
                    #[cfg(feature = "test-support")]
                    if matches!(out, Out::Resize(..) | Out::Facts(_) | Out::Focus) {
                        self.env.control.touch();
                    }
                    match out {
                        Out::Data(bytes) => frames.extend(frame::data_frames(channel, &bytes)),
                        Out::Automatic(bytes) => {
                            frames.extend(frame::automatic_frames(channel, &bytes));
                        }
                        Out::Resize(cols, rows) => frames.push(frame::resize(channel, cols, rows)),
                        Out::Facts(display) => frames.push(frame::facts(
                            channel,
                            jsonrpc::display_json(&display).to_string().as_bytes(),
                        )),
                        Out::Focus => frames.push(frame::focus(channel, true)),
                    }
                }
            }
        }
        frames
    }

    /// Starts the reopen when the input writer is gone.
    fn input_writer_closed(&self, state: &mut State) {
        state.fence.begin_reopen();
        let _ = self.commands.send(Command::Fail);
    }

    fn request(&self, ownership: OwnershipGeneration, request: Request) -> BackendResult<()> {
        let mut state = lock(&self.state);
        let writer = state.writers.input.clone();
        let mut permits = None;
        if state.fence.phase == Phase::Ready && !state.fence.watch_only {
            if let Some(writer) = &writer {
                match writer.try_reserve_many(Fence::frames_needed(&request)) {
                    Ok(reserved) => permits = Some(reserved),
                    Err(TrySendError::Full(())) => {
                        // The request is refused and changes nothing on this attachment, but
                        // its ownership generation still reached the terminal: record it, as
                        // a refused reopen-queue request does, so another attachment's older
                        // deferred request can't replay as current (port contract).
                        let mut fence = lock(&self.env.fence);
                        let highest = fence.entry(self.address.clone()).or_insert(0);
                        *highest = (*highest).max(ownership.0);
                        return Err(BackendError::Failed("input queue full".to_string()));
                    }
                    Err(TrySendError::Closed(())) => self.input_writer_closed(&mut state),
                }
            }
        }
        let effects = {
            let mut fence = lock(&self.env.fence);
            let highest = fence.entry(self.address.clone()).or_insert(0);
            state.fence.request(highest, ownership.0, request)?
        };
        let frames = self.frames_of(&state, effects);
        if let Some(permits) = permits {
            // `frames_needed` bounds what `Fence::apply` can send for a request, so every
            // frame has a slot.
            debug_assert!(frames.len() <= permits.len());
            for (permit, bytes) in permits.zip(frames) {
                permit.send(bytes);
            }
        }
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

    /// Fails one half. False when the attachment is closed. The reopen begins at once, under
    /// the lock, as it does when a connection breaks, so a request made right after this call
    /// is queued for the new pair rather than racing the supervisor.
    #[cfg(feature = "test-support")]
    pub(super) fn fail(&self, half: PairHalf) -> bool {
        {
            let mut state = lock(&self.state);
            if state.fence.phase == Phase::Closed {
                return false;
            }
            if state.fence.watch_only && half == PairHalf::Input {
                return true;
            }
            state.fence.begin_reopen();
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
        display_owed: 0,
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
        credit_wake: Notify::new(),
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
///
/// Credit for the input half's output is owed until the writer queue has room for it, and the
/// display half's owed credit (from `credit`) is sent the same way, so every byte received is
/// credited once.
async fn run_pair(
    shared: &Shared,
    halves: &mut Halves,
    commands: &mut mpsc::UnboundedReceiver<Command>,
) -> Ended {
    let mut run = PairRun::default();
    let mut input_done = false;
    let mut input_owed = 0u64;
    let input_writer = halves
        .input
        .as_ref()
        .and_then(|input| input.connection.sender());
    let input_channel = halves.input.as_ref().map_or(0, |input| input.channel);
    let display_writer = halves.display.connection.sender();
    let display_channel = halves.display.channel;
    let prefetched = std::mem::take(&mut halves.prefetched);
    for frame in prefetched {
        let reaction = run.on_display(Some(frame));
        if let Some(ended) = apply(shared, reaction, true, &mut input_owed) {
            return ended;
        }
    }
    loop {
        let display_owed = lock(&shared.state).display_owed > 0;
        let (reaction, from_display) = tokio::select! {
            frame = halves.display.frames.recv() => (run.on_display(frame), true),
            frame = recv_input(&mut halves.input), if !input_done && halves.input.is_some() => {
                if frame.is_none() {
                    input_done = true;
                }
                (run.on_input(frame), false)
            }
            slot = reserve_slot(input_writer.as_ref()), if input_owed > 0 => {
                match slot {
                    Some(slot) => {
                        let bytes = u32::try_from(input_owed).unwrap_or(u32::MAX);
                        input_owed -= u64::from(bytes);
                        slot.send(frame::credit(input_channel, bytes));
                    }
                    None => input_owed = 0,
                }
                continue;
            }
            slot = reserve_slot(display_writer.as_ref()), if display_owed => {
                let mut state = lock(&shared.state);
                match slot {
                    Some(slot) => {
                        if let Some(bytes) = state.take_display_credit() {
                            slot.send(frame::credit(display_channel, bytes));
                        }
                    }
                    None => state.display_owed = 0,
                }
                continue;
            }
            () = shared.credit_wake.notified() => continue,
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
        if let Some(ended) = apply(shared, reaction, from_display, &mut input_owed) {
            return ended;
        }
    }
}

/// Emits a reaction's events and owes the input half its credit. `Some` when the pair is over.
fn apply(
    shared: &Shared,
    reaction: Reaction,
    from_display: bool,
    input_owed: &mut u64,
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
    *input_owed += u64::from(reaction.credit_input);
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
                if install(shared, &halves) {
                    return Some(halves);
                }
                halves.close();
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
/// test asked for, then the queued requests in order. The replay's frames are reserved in the
/// new writer queue first. If they can't be, the pair is not installed and this returns
/// false: the queue stays, and the caller counts the attempt and tries again.
fn install(shared: &Shared, halves: &Halves) -> bool {
    let mut state = lock(&shared.state);
    let writers = writers_of(halves);
    let input = writers.input.clone();
    let permits = match &input {
        Some(writer) => match writer.try_reserve_many(state.fence.queued_frames_bound()) {
            Ok(permits) => Some(permits),
            Err(_) => return false,
        },
        None => None,
    };
    let fidelity =
        lock(&shared.env.fidelities).observe(&shared.address, halves.created_ms, &halves.screen);
    state.fidelity = fidelity;
    state.ordered = halves.ordered;
    state.writers = writers;
    state.display_open = true;
    state.input_open = halves.input.is_some();
    state.display_owed = 0;
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
    let frames = shared.frames_of(&state, effects);
    if let Some(permits) = permits {
        // The bound counted at enqueue covers every frame the replay can send.
        debug_assert!(frames.len() <= permits.len());
        for (permit, bytes) in permits.zip(frames) {
            permit.send(bytes);
        }
    }
    true
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
        let frames = self.shared.frames_of(&state, effects);
        if frames.is_empty() {
            return Ok(());
        }
        let Some(writer) = state.writers.input.clone() else {
            return Ok(());
        };
        let reserved = writer.try_reserve_many(frames.len());
        match reserved {
            Ok(permits) => {
                #[cfg(feature = "test-support")]
                self.shared
                    .env
                    .control
                    .record_automatic(&self.shared.address, bytes);
                debug_assert!(frames.len() <= permits.len());
                for (permit, bytes) in permits.zip(frames) {
                    permit.send(bytes);
                }
                Ok(())
            }
            Err(TrySendError::Full(())) => {
                Err(BackendError::Failed("input queue full".to_string()))
            }
            Err(TrySendError::Closed(())) => {
                self.shared.input_writer_closed(&mut state);
                Ok(())
            }
        }
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
        let mut state = lock(&self.shared.state);
        if !state.fence.credit_gate()? {
            return Ok(());
        }
        let Some(writer) = state.writers.display.clone() else {
            return Ok(());
        };
        state.display_owed += u64::from(bytes);
        if let Ok(slot) = writer.try_reserve() {
            if let Some(owed) = state.take_display_credit() {
                slot.send(frame::credit(state.writers.display_channel, owed));
            }
        }
        if state.display_owed > 0 {
            self.shared.credit_wake.notify_one();
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

/// The handle against an in-memory core: the writer queue fills while the core is not
/// reading, and credit competes with typed input for it.
#[cfg(all(test, feature = "test-support"))]
mod queue_tests {
    use std::time::Duration;

    use combine_protocol::Display;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};

    use super::super::connection::{Connection, WRITER_QUEUE_FRAMES};
    use super::super::control;
    use super::super::fencing::REOPEN_QUEUE_REQUESTS;
    use super::*;
    use crate::ports::AttachOptions;

    const DISPLAY_CHANNEL: u32 = 1;
    const INPUT_CHANNEL: u32 = 2;
    const QUIET: Duration = Duration::from_millis(400);

    /// The core's end of one connection. A task of its own reads whole frames, so a timeout
    /// on `next` never drops half a frame. It starts only on `resume`, so a test can leave
    /// the core not reading while the client's writer queue fills.
    struct Core {
        frames: mpsc::UnboundedReceiver<Frame>,
        write: WriteHalf<DuplexStream>,
        go: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl Core {
        fn new(mut read: ReadHalf<DuplexStream>, write: WriteHalf<DuplexStream>) -> Core {
            let (tx, frames) = mpsc::unbounded_channel();
            let (go, wait) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                if wait.await.is_err() {
                    return;
                }
                while let Some(frame) = read_frame(&mut read).await {
                    if tx.send(frame).is_err() {
                        return;
                    }
                }
            });
            Core {
                frames,
                write,
                go: Some(go),
            }
        }

        fn resume(&mut self) {
            if let Some(go) = self.go.take() {
                let _ = go.send(());
            }
        }

        async fn next(&mut self, wait: Duration) -> Option<Frame> {
            self.resume();
            tokio::time::timeout(wait, self.frames.recv())
                .await
                .ok()
                .flatten()
        }

        /// Frames until `done` holds for what has arrived (at most 30 s), then whatever
        /// else comes before the line is quiet.
        async fn drain_until(&mut self, done: impl Fn(&[Frame]) -> bool) -> Vec<Frame> {
            let mut frames = Vec::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            while !done(&frames) {
                let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                match self.next(left).await {
                    Some(frame) => frames.push(frame),
                    None => return frames,
                }
            }
            while let Some(frame) = self.next(QUIET).await {
                frames.push(frame);
            }
            frames
        }

        /// What arrives until the line is quiet.
        async fn drain(&mut self) -> Vec<Frame> {
            self.drain_until(|_| true).await
        }
    }

    fn display(tag: &str) -> Display {
        Display {
            foreground: format!("#{tag}{tag}{tag}"),
            background: "#000000".to_string(),
            cursor_color: "#ffffff".to_string(),
            cell_width: 9,
            cell_height: 18,
        }
    }

    fn env() -> Arc<Env> {
        Arc::new(Env {
            config: super::super::HerdrCoreConfig::new("/nonexistent".into()),
            fence: Mutex::new(HashMap::new()),
            fidelities: Mutex::new(Fidelities::default()),
            automatic_feature: true,
            control: Arc::new(control::Shared::default()),
        })
    }

    async fn read_line(read: &mut ReadHalf<DuplexStream>) {
        let mut byte = [0u8; 1];
        while read.read_exact(&mut byte).await.is_ok() {
            if byte[0] == b'\n' {
                break;
            }
        }
    }

    /// A connection to a model core that answers hello. `pipe` is the size of the in-memory
    /// pipe: a small one makes the writer block, so its queue fills.
    async fn connect(pipe: usize, channel: u32) -> (Half, Core) {
        let (client, server) = tokio::io::duplex(pipe);
        let (mut read, mut write) = tokio::io::split(server);
        let hello = serde_json::json!({"jsonrpc": "2.0", "id": 0, "result": {
            "generation": 1, "version": "0.1.21", "framed": true,
            "core": {"id": "x", "name": "y"},
            "methods": ["terminal.attach", "terminal.list", "terminal.run", "terminal.kill",
                        "events.subscribe"],
            "features": ["automatic_input"]}});
        let answer = tokio::spawn(async move {
            read_line(&mut read).await;
            write
                .write_all(format!("{hello}\n").as_bytes())
                .await
                .expect("hello");
            (read, write)
        });
        let (connection, frames) = Connection::from_stream(client).await.expect("open");
        let (read, write) = answer.await.expect("core");
        (
            Half {
                connection,
                frames,
                channel,
            },
            Core::new(read, write),
        )
    }

    /// A pair whose input half goes through a `pipe`-byte pipe.
    async fn halves(pipe: usize) -> (Halves, Core, Core) {
        let (display_half, display_core) = connect(65_536, DISPLAY_CHANNEL).await;
        let (input_half, input_core) = connect(pipe, INPUT_CHANNEL).await;
        let halves = Halves {
            display: display_half,
            input: Some(input_half),
            prefetched: Vec::new(),
            size: (80, 24),
            ordered: true,
            automatic: true,
            created_ms: 1,
            screen: "exact".to_string(),
        };
        (halves, display_core, input_core)
    }

    fn options() -> AttachOptions {
        AttachOptions {
            cols: 80,
            rows: 24,
            display: display("aa"),
            observe: false,
            resumed: false,
            history: 0,
        }
    }

    async fn read_frame(read: &mut ReadHalf<DuplexStream>) -> Option<Frame> {
        let mut header = [0u8; frame::HEADER_LEN];
        read.read_exact(&mut header).await.ok()?;
        let channel = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let length = u32::from_le_bytes([header[5], header[6], header[7], header[8]]);
        let mut payload = vec![0u8; length as usize];
        read.read_exact(&mut payload).await.ok()?;
        let kind = match header[4] {
            0 => Kind::Data,
            1 => Kind::Credit,
            2 => Kind::Close,
            3 => Kind::Resize,
            4 => Kind::Focus,
            5 => Kind::Facts,
            _ => Kind::AutomaticInput,
        };
        Some(Frame {
            channel,
            kind,
            payload,
        })
    }

    fn credited(frames: &[Frame]) -> u64 {
        frames
            .iter()
            .filter(|frame| frame.kind == Kind::Credit)
            .map(|frame| {
                let bytes = &frame.payload;
                u64::from(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            })
            .sum()
    }

    fn typed_len(frames: &[Frame]) -> usize {
        frames
            .iter()
            .filter(|frame| frame.kind == Kind::Data)
            .map(|frame| frame.payload.len())
            .sum()
    }

    fn typed(frames: &[Frame]) -> Vec<u8> {
        frames
            .iter()
            .filter(|frame| frame.kind == Kind::Data)
            .flat_map(|frame| frame.payload.clone())
            .collect()
    }

    async fn attach(pipe: usize) -> (Box<dyn Attachment>, Arc<Shared>, Core, Core) {
        let (halves, display_core, input_core) = halves(pipe).await;
        let handle = start(env(), "t1", &options(), halves);
        let shared = Arc::clone(&handle.shared);
        (Box::new(handle), shared, display_core, input_core)
    }

    /// The input half's output is credited back byte for byte, though the core stops reading
    /// while it bursts and the writer queue overflows.
    #[tokio::test(flavor = "multi_thread")]
    async fn every_input_half_output_byte_is_credited_back() {
        let (_attachment, _shared, _display, mut input) = attach(64).await;
        let total = WRITER_QUEUE_FRAMES * 3;
        for _ in 0..total {
            input
                .write
                .write_all(&frame::encode(INPUT_CHANNEL, Kind::Data, b"x"))
                .await
                .expect("burst");
        }
        let mut credit = 0u64;
        while credit < total as u64 {
            match input.next(Duration::from_secs(10)).await {
                Some(frame) if frame.kind == Kind::Credit => {
                    credit += credited(&[frame]);
                }
                Some(_) => {}
                None => break,
            }
        }
        assert_eq!(credit, total as u64, "input-half credit was dropped");
    }

    /// Display credit the daemon grants while the display queue is full is owed, not refused.
    #[tokio::test(flavor = "multi_thread")]
    async fn display_credit_is_never_refused_for_a_full_queue() {
        let (halves, mut display_core, _input) = {
            let (display_half, display_core) = connect(64, DISPLAY_CHANNEL).await;
            let (input_half, input_core) = connect(65_536, INPUT_CHANNEL).await;
            let halves = Halves {
                display: display_half,
                input: Some(input_half),
                prefetched: Vec::new(),
                size: (80, 24),
                ordered: true,
                automatic: true,
                created_ms: 1,
                screen: "exact".to_string(),
            };
            (halves, display_core, input_core)
        };
        let mut attachment = start(env(), "t1", &options(), halves);
        let grants = WRITER_QUEUE_FRAMES * 3;
        for _ in 0..grants {
            attachment.credit(1).await.expect("credit is never refused");
        }
        let mut credit = 0u64;
        while credit < grants as u64 {
            match display_core.next(Duration::from_secs(10)).await {
                Some(frame) => credit += credited(&[frame]),
                None => break,
            }
        }
        assert_eq!(credit, grants as u64);
    }

    /// A write that returned `Ok` arrives whole and in order, and every other write was
    /// refused with "input queue full", while input-half output keeps credit competing for
    /// the queue. Some writes span several frames.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_accepted_write_is_never_dropped_while_credit_competes() {
        let (mut attachment, _shared, _display, input) = attach(64).await;
        let Core { frames, write, go } = input;
        let mut flood_write = write;
        let flood = tokio::spawn(async move {
            for _ in 0..WRITER_QUEUE_FRAMES * 2 {
                if flood_write
                    .write_all(&frame::encode(INPUT_CHANNEL, Kind::Data, b"y"))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            flood_write
        });
        let mut accepted = Vec::new();
        let mut refused = 0usize;
        for index in 0..WRITER_QUEUE_FRAMES * 2 {
            let len = if index % 40 == 0 { 40_000 } else { 100 };
            let bytes = vec![u8::try_from(index % 251).unwrap_or(0); len];
            match attachment.write(OwnershipGeneration(1), &bytes).await {
                Ok(()) => accepted.extend_from_slice(&bytes),
                Err(error) => {
                    assert_eq!(error, BackendError::Failed("input queue full".to_string()));
                    refused += 1;
                }
            }
            if index % 64 == 0 {
                tokio::task::yield_now().await;
            }
        }
        let mut input = Core {
            frames,
            write: flood.await.expect("flood"),
            go,
        };
        let want = accepted.len();
        let frames = input.drain_until(|frames| typed_len(frames) >= want).await;
        assert!(refused > 0, "the queue never filled");
        assert!(
            typed(&frames) == accepted,
            "an accepted write was dropped or reordered: sent {} of {} accepted bytes",
            typed(&frames).len(),
            accepted.len()
        );
    }

    /// At a full queue `write_automatic` is refused, never dropped.
    #[tokio::test(flavor = "multi_thread")]
    async fn write_automatic_at_a_full_queue_fails() {
        let (mut attachment, _shared, _display, mut input) = attach(64).await;
        let mut accepted = 0usize;
        for _ in 0..WRITER_QUEUE_FRAMES * 2 {
            if attachment
                .write(OwnershipGeneration(1), b"typed")
                .await
                .is_err()
            {
                break;
            }
            accepted += 1;
        }
        assert_eq!(
            attachment
                .write_automatic(BackendGeneration(1), b"reply")
                .await,
            Err(BackendError::Failed("input queue full".to_string()))
        );
        let frames = input
            .drain_until(|frames| typed_len(frames) >= accepted * 5)
            .await;
        assert_eq!(typed(&frames).len(), accepted * 5);
        assert!(frames
            .iter()
            .all(|frame| frame.kind != Kind::AutomaticInput));
        attachment
            .write_automatic(BackendGeneration(1), b"reply")
            .await
            .expect("room again");
        let frames = input
            .drain_until(|frames| frames.iter().any(|f| f.kind == Kind::AutomaticInput))
            .await;
        let replies: Vec<&Frame> = frames
            .iter()
            .filter(|frame| frame.kind == Kind::AutomaticInput)
            .collect();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].payload, b"reply");
    }

    /// A request that a full Ready writer refuses changes nothing on its attachment, but its
    /// newer ownership generation still supersedes another attachment's older reopen queue:
    /// that queue's focus doesn't activate, and its typed bytes go out as automatic input.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_full_ready_queue_still_supersedes_an_older_reopen_queue() {
        let env = env();
        let (a_halves, _a_display, _a_input) = halves(65_536).await;
        let a_handle = start(Arc::clone(&env), "t1", &options(), a_halves);
        let a_shared = Arc::clone(&a_handle.shared);
        let mut a: Box<dyn Attachment> = Box::new(a_handle);
        lock(&a_shared.state).fence.begin_reopen();
        a.write(OwnershipGeneration(1), b"w").await.expect("queued");
        a.focus(OwnershipGeneration(1)).await.expect("queued");

        let (b_halves, _b_display, _b_input) = halves(64).await;
        let b_handle = start(Arc::clone(&env), "t1", &options(), b_halves);
        let b_shared = Arc::clone(&b_handle.shared);
        let mut b: Box<dyn Attachment> = Box::new(b_handle);
        let mut full = false;
        for _ in 0..WRITER_QUEUE_FRAMES * 2 {
            if b.write(OwnershipGeneration(1), b"x").await.is_err() {
                full = true;
                break;
            }
        }
        assert!(full, "b's writer queue fills");
        assert_eq!(
            b.resize(OwnershipGeneration(2), 100, 30).await,
            Err(BackendError::Failed("input queue full".to_string()))
        );
        assert_eq!(
            lock(&b_shared.state).fence.requested.size,
            (80, 24),
            "the refused resize is not applied"
        );

        let (next, _display, mut input) = halves(65_536).await;
        assert!(install(&a_shared, &next));
        let mut frames = Vec::new();
        while let Some(frame) = input.next(Duration::from_millis(300)).await {
            frames.push(frame);
        }
        let kinds: Vec<Kind> = frames.iter().map(|frame| frame.kind).collect();
        assert_eq!(kinds, [Kind::AutomaticInput], "{kinds:?}");
        assert_eq!(frames[0].payload, b"w");
    }

    /// The largest reopen queue replays whole into a new pair's writer, once, in order,
    /// though that writer is not draining while it is installed.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_full_reopen_queue_replays_once_into_the_new_writer() {
        let (mut attachment, shared, _display, _input) = attach(65_536).await;
        lock(&shared.state).fence.begin_reopen();
        let big = vec![b'a'; 49_152];
        attachment
            .write(OwnershipGeneration(5), &big)
            .await
            .expect("queued");
        let mut sent = big;
        attachment
            .resize(OwnershipGeneration(1), 100, 30)
            .await
            .expect("queued");
        attachment
            .set_display(OwnershipGeneration(1), display("bb"))
            .await
            .expect("queued");
        for _ in 0..REOPEN_QUEUE_REQUESTS - 4 {
            attachment
                .write(OwnershipGeneration(5), &[b'b'; 16])
                .await
                .expect("queued");
            sent.extend([b'b'; 16]);
        }
        attachment
            .focus(OwnershipGeneration(5))
            .await
            .expect("queued");
        assert_eq!(
            attachment.write(OwnershipGeneration(5), b"z").await,
            Err(BackendError::Failed("reopen queue full".to_string()))
        );

        let (next, _display, mut input) = halves(64).await;
        assert!(install(&shared, &next));
        let want = 3 + (REOPEN_QUEUE_REQUESTS - 4) + 3;
        let frames = input.drain_until(|frames| frames.len() >= want).await;
        let kinds: Vec<Kind> = frames.iter().map(|frame| frame.kind).collect();
        assert_eq!(
            kinds.len(),
            3 + (REOPEN_QUEUE_REQUESTS - 4) + 3,
            "{kinds:?}"
        );
        assert_eq!(
            kinds[kinds.len() - 3..],
            [Kind::Facts, Kind::Resize, Kind::Focus]
        );
        assert!(
            typed(&frames) == sent,
            "every typed byte goes out once, in order"
        );
    }

    /// Makes the input writer closed, as when its connection died.
    fn close_input_writer(shared: &Shared) {
        let (dead, queue) = mpsc::channel(8);
        drop(queue);
        lock(&shared.state).writers.input = Some(dead);
    }

    /// Waits until the supervisor has begun rebuilding (both halves marked closed).
    async fn rebuilding(shared: &Shared) {
        for _ in 0..500 {
            if shared.halves_open() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the supervisor never began the rebuild");
    }

    /// A request that finds the input writer closed is accepted, the reopen begins, and the
    /// bytes go out once, in order, to the next pair.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_write_to_a_closed_writer_is_queued_for_the_reopen() {
        let (mut attachment, shared, _display, _input) = attach(65_536).await;
        close_input_writer(&shared);
        attachment
            .write(OwnershipGeneration(1), b"one")
            .await
            .expect("accepted");
        assert_eq!(lock(&shared.state).fence.phase, Phase::Reopening);
        attachment
            .write(OwnershipGeneration(1), b"two")
            .await
            .expect("queued");
        rebuilding(&shared).await;
        let (next, _display, mut input) = halves(65_536).await;
        assert!(install(&shared, &next));
        let frames = input.drain_until(|frames| frames.len() >= 2).await;
        assert_eq!(frames.len(), 2, "each write goes out once");
        assert_eq!(typed(&frames), b"onetwo");
    }

    /// An automatic reply that finds the writer closed is not an error, and the reopen begins.
    #[tokio::test(flavor = "multi_thread")]
    async fn write_automatic_to_a_closed_writer_begins_the_reopen() {
        let (mut attachment, shared, _display, _input) = attach(65_536).await;
        close_input_writer(&shared);
        attachment
            .write_automatic(BackendGeneration(1), b"reply")
            .await
            .expect("not an error");
        assert_eq!(lock(&shared.state).fence.phase, Phase::Reopening);
    }

    /// One 64 KiB write (4 frames) and 1,023 resizes make 1,027 frames: more than one per
    /// request. All of them go out, in order.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_reopen_queue_of_split_writes_replays_every_frame() {
        let (mut attachment, shared, _display, _input) = attach(65_536).await;
        lock(&shared.state).fence.begin_reopen();
        let big: Vec<u8> = (0..65_536u32).map(|n| (n % 251) as u8).collect();
        attachment
            .write(OwnershipGeneration(5), &big)
            .await
            .expect("queued");
        for n in 0..REOPEN_QUEUE_REQUESTS - 1 {
            let cols = 100 + u16::try_from(n % 50).unwrap_or(0);
            attachment
                .resize(OwnershipGeneration(5), cols, 30)
                .await
                .expect("queued");
        }
        let (next, _display, mut input) = halves(64).await;
        assert!(install(&shared, &next));
        let frames = input.drain_until(|frames| frames.len() >= 1_027).await;
        assert_eq!(frames.len(), 1_027);
        assert!(frames[..4].iter().all(|frame| frame.kind == Kind::Data));
        assert!(typed(&frames) == big, "the write arrives whole, in order");
        for (n, frame) in frames[4..].iter().enumerate() {
            let cols = 100 + u16::try_from(n % 50).unwrap_or(0);
            assert_eq!(frame.kind, Kind::Resize);
            assert_eq!(frame::parse_resize(&frame.payload), Some((cols, 30)));
        }
    }

    /// A new pair whose writer can't take the replay is not installed: the attempt fails, the
    /// queue stays, and the next pair gets it once.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pair_that_cannot_take_the_replay_is_not_installed() {
        let (mut attachment, shared, _display, _input) = attach(65_536).await;
        lock(&shared.state).fence.begin_reopen();
        attachment
            .write(OwnershipGeneration(1), b"kept")
            .await
            .expect("queued");
        let (full, _display, _core) = halves(64).await;
        let writer = full
            .input
            .as_ref()
            .and_then(|half| half.connection.sender())
            .expect("a writer");
        while writer.try_send(frame::credit(INPUT_CHANNEL, 1)).is_ok() {}
        assert!(!install(&shared, &full), "the queue is full");
        assert_eq!(lock(&shared.state).fence.phase, Phase::Reopening);
        assert_eq!(lock(&shared.state).fence.generation, 1);
        let (next, _display, mut input) = halves(65_536).await;
        assert!(install(&shared, &next));
        let frames = input.drain_until(|frames| !frames.is_empty()).await;
        assert_eq!(typed(&frames), b"kept");
        assert_eq!(frames.len(), 1);
    }

    /// A frame the core's line stalls in the middle of arrives whole: reading it is not
    /// cancelled by the quiet timeout.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_frame_stalled_mid_way_is_read_whole() {
        let (pair, _display, mut input) = halves(65_536).await;
        let writer = pair
            .input
            .as_ref()
            .and_then(|half| half.connection.sender())
            .expect("a writer");
        let second = frame::encode(INPUT_CHANNEL, Kind::Data, b"second frame");
        let cut = second.len() - 4;
        writer
            .send(frame::encode(INPUT_CHANNEL, Kind::Data, b"first"))
            .await
            .expect("first");
        writer.send(second[..cut].to_vec()).await.expect("part");
        let first = input.drain().await;
        assert_eq!(typed(&first), b"first");
        writer.send(second[cut..].to_vec()).await.expect("rest");
        let rest = input.drain_until(|frames| !frames.is_empty()).await;
        assert_eq!(typed(&rest), b"second frame");
    }
}
