#![cfg(feature = "test-support")]
//! The fake's whole state and the rules that change it. The adapter layer in `fake_backend`
//! and `fake_attachment` and the handle in `control` take the lock, call one method here and
//! release it, never holding it across an `.await`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::Waker;

use combine_protocol::{BackendGeneration, CloseReason, Display, Fidelity, TermId};

use super::{
    FakeAttachmentRef, FakeCall, FakeConfig, Half, Mutation, ResyncMode, FAKE_PAINT_END,
    FAKE_PAINT_START,
};
use crate::model::{SpawnSpec, TermEvent, TermInfo, TermState};
use crate::ports::{AttachEvent, AttachOptions, BackendError, BackendResult, Capabilities};

pub(crate) const ID_PREFIX: &str = "fake";

/// A `DropByte` mutation only touches output at least this long.
const DROP_BYTE_THRESHOLD: usize = 1_024;

pub(crate) type Shared = Arc<Mutex<World>>;

pub(crate) fn lock(world: &Shared) -> MutexGuard<'_, World> {
    world.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn term_id(address: &str) -> TermId {
    TermId(format!("{ID_PREFIX}:{address}"))
}

/// The address of a fake ID, or `None` when the prefix or the address is wrong.
pub(crate) fn address_of(id: &TermId) -> Option<&str> {
    id.0.strip_prefix(ID_PREFIX)
        .and_then(|rest| rest.strip_prefix(':'))
        .filter(|address| !address.is_empty())
}

/// An address naming another machine's terminal, like `work/t1`.
pub(crate) fn is_remote(address: &str) -> bool {
    address.contains('/')
}

struct Terminal {
    address: String,
    cwd: String,
    argv: Vec<String>,
    labels: BTreeMap<String, String>,
    cols: u16,
    rows: u16,
    running: bool,
    fidelity: Fidelity,
    output_bytes: u64,
    delayed: bool,
    failing_opens: u32,
    active: Option<u64>,
    /// How many times any attachment has become the active viewer.
    activations: u64,
    typed: Vec<u8>,
    automatic: Vec<Vec<u8>>,
}

impl Terminal {
    fn info(&self) -> TermInfo {
        TermInfo {
            id: term_id(&self.address),
            cwd: self.cwd.clone(),
            argv: self.argv.clone(),
            state: if self.running {
                TermState::Running
            } else {
                TermState::Exited { exit_code: None }
            },
            labels: self.labels.clone(),
            cols: self.cols,
            rows: self.rows,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Reopening,
    /// The program ended. Output waiting for credit still drains, then `Closed(Exited)` and
    /// the end of the stream follow. It takes no requests but display credit.
    Draining,
    Closed,
}

/// A request held for the reopen, with the terminal's activation count when it was queued.
struct Queued {
    request: Request,
    activations: u64,
}

enum Request {
    Write(Vec<u8>),
    Resize(u16, u16),
    Focus,
    SetDisplay(Display),
}

struct Piped {
    generation: u64,
    event: AttachEvent,
    paint: bool,
}

struct Att {
    address: String,
    observe: bool,
    state: State,
    generation: u64,
    credit: u64,
    requested: ((u16, u16), Display),
    display_open: bool,
    input_open: bool,
    fidelity: Fidelity,
    pipe: VecDeque<Piped>,
    outbox: VecDeque<(BackendGeneration, AttachEvent)>,
    waker: Option<Waker>,
    queue: VecDeque<Queued>,
    queued_input: usize,
    attempts: u32,
    mutated: bool,
}

impl Att {
    fn push_out(&mut self, generation: u64, event: AttachEvent) {
        self.outbox
            .push_back((BackendGeneration(generation), event));
        self.wake();
    }

    fn wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    fn pipe(&mut self, generation: u64, event: AttachEvent, paint: bool) {
        self.pipe.push_back(Piped {
            generation,
            event,
            paint,
        });
    }

    fn pipe_data_bytes(&self) -> usize {
        self.pipe
            .iter()
            .map(|item| match &item.event {
                AttachEvent::Data(bytes) => bytes.len(),
                _ => 0,
            })
            .sum()
    }

    fn halves_open(&self) -> usize {
        usize::from(self.display_open) + usize::from(self.input_open)
    }

    fn close_halves(&mut self) {
        self.display_open = false;
        self.input_open = false;
    }
}

struct Subscriber {
    queue: VecDeque<TermEvent>,
    waker: Option<Waker>,
}

pub(crate) struct World {
    config: FakeConfig,
    next_address: u64,
    next_attachment: u64,
    crashed: bool,
    terminals: Vec<Terminal>,
    attachments: BTreeMap<u64, Att>,
    subscribers: Vec<Subscriber>,
    calls: Vec<FakeCall>,
    filtered_events: usize,
}

fn paint_bytes(cols: u16, rows: u16, fidelity: Fidelity, output_bytes: u64) -> Vec<u8> {
    let fidelity = match fidelity {
        Fidelity::Exact => "exact",
        Fidelity::Inexact => "inexact",
    };
    let mut paint = FAKE_PAINT_START.to_vec();
    paint.extend_from_slice(
        format!("screen {cols}x{rows} {fidelity} after {output_bytes} bytes").as_bytes(),
    );
    paint.extend_from_slice(FAKE_PAINT_END);
    paint
}

impl World {
    pub(crate) fn new(config: FakeConfig) -> Self {
        World {
            config,
            next_address: 1,
            next_attachment: 1,
            crashed: false,
            terminals: Vec::new(),
            attachments: BTreeMap::new(),
            subscribers: Vec::new(),
            calls: Vec::new(),
            filtered_events: 0,
        }
    }

    pub(crate) fn capabilities(&self) -> Capabilities {
        Capabilities {
            resync_signal: self.config.resync == ResyncMode::Signal,
            ..self.config.capabilities
        }
    }

    // ---- lookups ----

    fn index(&self, address: &str) -> Option<usize> {
        self.terminals
            .iter()
            .position(|terminal| terminal.address == address)
    }

    fn index_of(&self, term: &TermId) -> usize {
        address_of(term)
            .and_then(|address| self.index(address))
            .unwrap_or_else(|| panic!("the fake has no terminal {}", term.0))
    }

    fn attachment_ids(&self, address: &str) -> Vec<u64> {
        self.attachments
            .iter()
            .filter(|(_, att)| att.address == address)
            .map(|(id, _)| *id)
            .collect()
    }

    /// The index of a running terminal on this machine.
    pub(crate) fn live_local(&self, address: &str) -> Option<usize> {
        self.index(address)
            .filter(|&index| self.terminals[index].running && !is_remote(address))
    }

    // ---- events and subscribers ----

    fn raise(&mut self, address: &str, event: TermEvent) {
        for subscriber in &mut self.subscribers {
            if is_remote(address) {
                self.filtered_events += 1;
                continue;
            }
            subscriber.queue.push_back(event.clone());
            if let Some(waker) = subscriber.waker.take() {
                waker.wake();
            }
        }
    }

    pub(crate) fn subscribe(&mut self) -> usize {
        self.subscribers.push(Subscriber {
            queue: VecDeque::new(),
            waker: None,
        });
        self.subscribers.len() - 1
    }

    pub(crate) fn poll_subscriber(
        &mut self,
        subscriber: usize,
        waker: &Waker,
    ) -> Option<TermEvent> {
        let subscriber = &mut self.subscribers[subscriber];
        let event = subscriber.queue.pop_front();
        if event.is_none() {
            subscriber.waker = Some(waker.clone());
        }
        event
    }

    // ---- terminals ----

    pub(crate) fn list(&self) -> Vec<TermInfo> {
        self.terminals
            .iter()
            .filter(|terminal| terminal.running && !is_remote(&terminal.address))
            .map(Terminal::info)
            .collect()
    }

    pub(crate) fn spawn(&mut self, spec: SpawnSpec) -> BackendResult<TermId> {
        if spec.cols == 0 || spec.rows == 0 {
            return Err(BackendError::InvalidRequest(
                "a terminal needs at least one column and one row".to_string(),
            ));
        }
        let address = format!("t{}", self.next_address);
        self.next_address += 1;
        let terminal = Terminal {
            address: address.clone(),
            cwd: spec.cwd,
            argv: spec.argv,
            labels: spec.labels,
            cols: spec.cols,
            rows: spec.rows,
            running: true,
            fidelity: if self.crashed {
                Fidelity::Inexact
            } else {
                Fidelity::Exact
            },
            output_bytes: 0,
            delayed: false,
            failing_opens: 0,
            active: None,
            activations: 0,
            typed: Vec::new(),
            automatic: Vec::new(),
        };
        let info = terminal.info();
        self.terminals.push(terminal);
        let id = term_id(&address);
        self.calls.push(FakeCall::Spawn { term: id.clone() });
        self.raise(&address, TermEvent::Created(info));
        Ok(id)
    }

    pub(crate) fn remote_twin(&mut self, local: &TermId) -> TermId {
        let local = &self.terminals[self.index_of(local)];
        let address = format!("work/{}", local.address);
        let id = term_id(&address);
        if self.index(&address).is_some() {
            return id;
        }
        let twin = Terminal {
            address: address.clone(),
            cwd: local.cwd.clone(),
            argv: local.argv.clone(),
            labels: local.labels.clone(),
            cols: local.cols,
            rows: local.rows,
            running: true,
            fidelity: local.fidelity,
            output_bytes: 0,
            delayed: false,
            failing_opens: 0,
            active: None,
            activations: 0,
            typed: Vec::new(),
            automatic: Vec::new(),
        };
        let info = twin.info();
        self.terminals.push(twin);
        self.raise(&address, TermEvent::Created(info));
        id
    }

    pub(crate) fn kill(&mut self, address: &str) {
        self.calls.push(FakeCall::Kill {
            term: term_id(address),
        });
        if let Some(index) = self.index(address) {
            self.finish(index, None);
        }
    }

    pub(crate) fn exit(&mut self, term: &TermId, exit_code: Option<i32>) {
        let index = self.index_of(term);
        self.finish(index, exit_code);
    }

    /// The program ended: every attachment closes, in order behind its pending output.
    fn finish(&mut self, index: usize, exit_code: Option<i32>) {
        if !self.terminals[index].running {
            return;
        }
        let address = self.terminals[index].address.clone();
        self.terminals[index].running = false;
        self.terminals[index].active = None;
        for id in self.attachment_ids(&address) {
            let att = self.attachments.get_mut(&id).expect("attachment");
            if matches!(att.state, State::Closed | State::Draining) {
                continue;
            }
            att.close_halves();
            att.state = State::Draining;
            att.queue.clear();
            att.queued_input = 0;
            let generation = att.generation;
            att.pipe(generation, AttachEvent::Closed(CloseReason::Exited), false);
            self.deliver(id);
        }
        self.raise(
            &address,
            TermEvent::Exited {
                id: term_id(&address),
                exit_code,
            },
        );
    }

    pub(crate) fn set_cwd(&mut self, term: &TermId, cwd: &str) {
        let index = self.index_of(term);
        self.terminals[index].cwd = cwd.to_string();
        let info = self.terminals[index].info();
        let address = self.terminals[index].address.clone();
        self.raise(&address, TermEvent::Changed(info));
    }

    pub(crate) fn set_fidelity(&mut self, term: &TermId, fidelity: Fidelity) {
        let index = self.index_of(term);
        self.terminals[index].fidelity = if self.crashed {
            Fidelity::Inexact
        } else {
            fidelity
        };
    }

    // ---- the pipe ----

    fn paint_for(&self, index: usize) -> Vec<u8> {
        let terminal = &self.terminals[index];
        paint_bytes(
            terminal.cols,
            terminal.rows,
            terminal.fidelity,
            terminal.output_bytes,
        )
    }

    /// Moves what the pipe may release to the outbox: nothing while the terminal's output is
    /// delayed, `Data` only within credit, anything else once it reaches the head.
    fn deliver(&mut self, id: u64) {
        let byte_credit = self.config.capabilities.byte_credit;
        let drop_byte = self.config.mutation == Some(Mutation::DropByte);
        let Some(att) = self.attachments.get_mut(&id) else {
            return;
        };
        let delayed = self
            .terminals
            .iter()
            .find(|terminal| terminal.address == att.address)
            .is_some_and(|terminal| terminal.delayed);
        if delayed {
            return;
        }
        while let Some(mut item) = att.pipe.pop_front() {
            let generation = item.generation;
            if let AttachEvent::Data(bytes) = &mut item.event {
                if drop_byte && !att.mutated && !item.paint && bytes.len() >= DROP_BYTE_THRESHOLD {
                    bytes.remove(bytes.len() / 2);
                    att.mutated = true;
                }
                if byte_credit {
                    if att.credit == 0 {
                        att.pipe.push_front(item);
                        break;
                    }
                    let take =
                        usize::try_from(att.credit).map_or(bytes.len(), |c| c.min(bytes.len()));
                    att.credit -= take as u64;
                    if take < bytes.len() {
                        let rest = bytes.split_off(take);
                        let head = std::mem::replace(bytes, rest);
                        att.pipe.push_front(item);
                        att.push_out(generation, AttachEvent::Data(head));
                        continue;
                    }
                }
            }
            att.push_out(generation, item.event);
        }
        if att.state == State::Draining && att.pipe.is_empty() {
            att.state = State::Closed;
        }
    }

    /// Drops the pipe's `Data`, keeping its other items, and says so: `Resynced` in signal
    /// mode, then a paint.
    fn resync(&mut self, id: u64) {
        let signal = self.config.resync == ResyncMode::Signal;
        let index = self
            .index(&self.attachments[&id].address)
            .expect("terminal");
        let paint = self.paint_for(index);
        let att = self.attachments.get_mut(&id).expect("attachment");
        let generation = att.generation;
        att.pipe
            .retain(|item| !matches!(item.event, AttachEvent::Data(_)));
        if signal {
            att.pipe(generation, AttachEvent::Resynced, false);
        }
        att.pipe(generation, AttachEvent::Data(paint), true);
    }

    pub(crate) fn force_resync(&mut self, att: FakeAttachmentRef) {
        if self
            .attachments
            .get(&att.0)
            .is_some_and(|a| a.state == State::Ready)
        {
            self.resync(att.0);
            self.deliver(att.0);
        }
    }

    pub(crate) fn emit(&mut self, term: &TermId, bytes: &[u8]) {
        let index = self.index_of(term);
        self.emit_at(index, bytes);
    }

    fn emit_at(&mut self, index: usize, bytes: &[u8]) {
        if bytes.is_empty() || !self.terminals[index].running {
            return;
        }
        self.terminals[index].output_bytes += bytes.len() as u64;
        let address = self.terminals[index].address.clone();
        let chunk_size = self.config.chunk_size.max(1);
        let limit = self.config.backend_queue_limit;
        for id in self.attachment_ids(&address) {
            let att = self.attachments.get_mut(&id).expect("attachment");
            if att.state != State::Ready {
                continue;
            }
            let generation = att.generation;
            for chunk in bytes.chunks(chunk_size) {
                att.pipe(generation, AttachEvent::Data(chunk.to_vec()), false);
            }
            if att.pipe_data_bytes() > limit {
                self.resync(id);
            }
            self.deliver(id);
        }
    }

    // ---- the PTY ----

    fn pipe_resized(&mut self, id: u64, cols: u16, rows: u16) {
        let mutation = self.config.mutation;
        let att = self.attachments.get_mut(&id).expect("attachment");
        let generation = att.generation;
        let event = AttachEvent::Resized { cols, rows };
        match mutation {
            Some(Mutation::DropResized) if !att.mutated => {
                att.mutated = true;
                return;
            }
            Some(Mutation::MoveResized) if !att.mutated => {
                let is_data = |event: &AttachEvent| matches!(event, AttachEvent::Data(_));
                if let Some(at) = att.pipe.iter().rposition(|item| is_data(&item.event)) {
                    att.mutated = true;
                    att.pipe.insert(
                        at,
                        Piped {
                            generation,
                            event,
                            paint: false,
                        },
                    );
                    return;
                }
                if let Some(at) = att.outbox.iter().rposition(|(_, event)| is_data(event)) {
                    att.mutated = true;
                    att.outbox
                        .insert(at, (BackendGeneration(generation), event));
                    att.wake();
                    return;
                }
            }
            _ => {}
        }
        att.pipe(generation, event, false);
    }

    /// Sets the PTY size. A change reaches every attachment with a ready display half.
    fn set_pty(&mut self, index: usize, cols: u16, rows: u16) {
        let terminal = &mut self.terminals[index];
        if (terminal.cols, terminal.rows) == (cols, rows) {
            return;
        }
        terminal.cols = cols;
        terminal.rows = rows;
        let address = terminal.address.clone();
        let ordered = self.config.ordered_resizes;
        let paint_after = self.config.paint_after_resize;
        for id in self.attachment_ids(&address) {
            if !self.attachments[&id].display_open {
                continue;
            }
            if ordered {
                self.pipe_resized(id, cols, rows);
            }
            if paint_after {
                let paint = self.paint_for(index);
                let att = self.attachments.get_mut(&id).expect("attachment");
                let generation = att.generation;
                att.pipe(generation, AttachEvent::Data(paint), true);
            }
            self.deliver(id);
        }
    }

    /// An attachment stops being the active viewer; with none left active the PTY goes back
    /// to its headless size.
    fn leave_active(&mut self, id: u64, restore_size: bool) {
        let address = self.attachments[&id].address.clone();
        let Some(index) = self.index(&address) else {
            return;
        };
        if self.terminals[index].active != Some(id) {
            return;
        }
        self.terminals[index].active = None;
        if restore_size {
            let (cols, rows) = self.config.headless_size;
            self.set_pty(index, cols, rows);
        }
    }

    fn become_active(&mut self, id: u64) {
        let att = &self.attachments[&id];
        let ((cols, rows), _) = att.requested;
        let index = self.index(&att.address).expect("terminal");
        self.terminals[index].active = Some(id);
        self.terminals[index].activations += 1;
        self.set_pty(index, cols, rows);
    }

    // ---- attachments ----

    fn started(&self, index: usize) -> AttachEvent {
        let terminal = &self.terminals[index];
        AttachEvent::Started {
            cols: terminal.cols,
            rows: terminal.rows,
            fidelity: terminal.fidelity,
            ordered_resizes: self.config.ordered_resizes,
        }
    }

    pub(crate) fn attach(
        &mut self,
        address: &str,
        options: AttachOptions,
    ) -> BackendResult<FakeAttachmentRef> {
        let index = self.index(address).expect("a live terminal");
        let reference = FakeAttachmentRef(self.next_attachment);
        self.next_attachment += 1;
        self.calls.push(FakeCall::Attach {
            att: reference,
            term: term_id(address),
            options: options.clone(),
        });
        if self.terminals[index].failing_opens > 0 {
            self.terminals[index].failing_opens -= 1;
            return Err(BackendError::Failed(
                "the input half failed to open".to_string(),
            ));
        }
        let terminal = &self.terminals[index];
        let paint = self.paint_for(index);
        let started = self.started(index);
        let mut att = Att {
            address: address.to_string(),
            observe: options.observe,
            state: State::Ready,
            generation: 1,
            credit: u64::from(self.config.capabilities.initial_credit),
            requested: ((options.cols, options.rows), options.display),
            display_open: true,
            input_open: !options.observe,
            fidelity: terminal.fidelity,
            pipe: VecDeque::new(),
            outbox: VecDeque::new(),
            waker: None,
            queue: VecDeque::new(),
            queued_input: 0,
            attempts: 0,
            mutated: false,
        };
        att.pipe(1, started, false);
        att.pipe(1, AttachEvent::Data(paint), true);
        self.attachments.insert(reference.0, att);
        self.deliver(reference.0);
        Ok(reference)
    }

    pub(crate) fn ordered_resizes(&self) -> bool {
        self.config.ordered_resizes
    }

    pub(crate) fn fidelity(&self, id: u64) -> Fidelity {
        self.attachments[&id].fidelity
    }

    pub(crate) fn pop_event(
        &mut self,
        id: u64,
        waker: &Waker,
    ) -> Option<Option<(BackendGeneration, AttachEvent)>> {
        let att = self.attachments.get_mut(&id)?;
        if let Some(item) = att.outbox.pop_front() {
            return Some(Some(item));
        }
        if matches!(att.state, State::Closed | State::Draining) && att.pipe.is_empty() {
            return Some(None);
        }
        att.waker = Some(waker.clone());
        None
    }

    /// What a call does to an attachment: nothing at all, wait for the reopen, or apply now.
    fn gate(&self, id: u64) -> BackendResult<Gate> {
        let att = &self.attachments[&id];
        match att.state {
            State::Closed | State::Draining => Err(BackendError::Closed),
            _ if att.observe => Ok(Gate::Skip),
            State::Reopening => Ok(Gate::Queue),
            State::Ready => Ok(Gate::Apply),
        }
    }

    fn queue_request(&mut self, id: u64, request: Request) -> BackendResult<()> {
        let limit = self.config.reopen_queue_limit;
        let input_limit = self.config.reopen_input_limit;
        let att = self.attachments.get(&id).expect("attachment");
        let index = self.index(&att.address).expect("terminal");
        let activations = self.terminals[index].activations;
        let att = self.attachments.get_mut(&id).expect("attachment");
        let input = match &request {
            Request::Write(bytes) => bytes.len(),
            _ => 0,
        };
        if att.queue.len() >= limit || att.queued_input + input > input_limit {
            return Err(BackendError::Failed("reopen queue full".to_string()));
        }
        att.queued_input += input;
        att.queue.push_back(Queued {
            request,
            activations,
        });
        Ok(())
    }

    fn apply(&mut self, id: u64, request: Request) {
        match request {
            Request::Write(bytes) => {
                self.become_active(id);
                let index = self
                    .index(&self.attachments[&id].address)
                    .expect("terminal");
                self.terminals[index].typed.extend_from_slice(&bytes);
                self.emit_at(index, &bytes);
            }
            Request::Focus => self.become_active(id),
            Request::Resize(cols, rows) => {
                let att = self.attachments.get_mut(&id).expect("attachment");
                att.requested.0 = (cols, rows);
                let address = att.address.clone();
                let index = self.index(&address).expect("terminal");
                if self.terminals[index].active == Some(id) {
                    self.set_pty(index, cols, rows);
                }
            }
            Request::SetDisplay(display) => {
                let queries = self.config.capabilities.display_queries;
                let att = self.attachments.get_mut(&id).expect("attachment");
                att.requested.1 = display.clone();
                if queries {
                    let generation = att.generation;
                    att.pipe(generation, AttachEvent::Facts(display), false);
                    self.deliver(id);
                }
            }
        }
    }

    /// A request queued before another attachment became the active viewer. Typed bytes were
    /// accepted while this attachment owned the terminal, so they are still written, without
    /// making it active. Every other request would change the size, display or focus the new
    /// owner holds, and is dropped.
    fn apply_superseded(&mut self, id: u64, request: Request) {
        if let Request::Write(bytes) = request {
            let index = self
                .index(&self.attachments[&id].address)
                .expect("terminal");
            self.terminals[index].typed.extend_from_slice(&bytes);
            self.emit_at(index, &bytes);
        }
    }

    fn request(&mut self, id: u64, request: Request) -> BackendResult<()> {
        match self.gate(id)? {
            Gate::Skip => Ok(()),
            Gate::Queue => self.queue_request(id, request),
            Gate::Apply => {
                self.apply(id, request);
                Ok(())
            }
        }
    }

    pub(crate) fn write(&mut self, att: u64, bytes: &[u8]) -> BackendResult<()> {
        self.calls.push(FakeCall::Write {
            att: FakeAttachmentRef(att),
            bytes: bytes.to_vec(),
        });
        self.request(att, Request::Write(bytes.to_vec()))
    }

    pub(crate) fn write_automatic(
        &mut self,
        att: u64,
        generation: BackendGeneration,
        bytes: &[u8],
    ) -> BackendResult<()> {
        self.calls.push(FakeCall::WriteAutomatic {
            att: FakeAttachmentRef(att),
            bytes: bytes.to_vec(),
        });
        let current = self.attachments[&att].generation == generation.0;
        if self.gate(att)? == Gate::Apply && current && self.config.capabilities.automatic_replies {
            let index = self
                .index(&self.attachments[&att].address)
                .expect("terminal");
            self.terminals[index].automatic.push(bytes.to_vec());
        }
        Ok(())
    }

    pub(crate) fn resize(&mut self, att: u64, cols: u16, rows: u16) -> BackendResult<()> {
        self.calls.push(FakeCall::Resize {
            att: FakeAttachmentRef(att),
            cols,
            rows,
        });
        self.request(att, Request::Resize(cols, rows))
    }

    pub(crate) fn focus(&mut self, att: u64) -> BackendResult<()> {
        self.calls.push(FakeCall::Focus {
            att: FakeAttachmentRef(att),
        });
        self.request(att, Request::Focus)
    }

    pub(crate) fn set_display(&mut self, att: u64, display: Display) -> BackendResult<()> {
        self.calls.push(FakeCall::SetDisplay {
            att: FakeAttachmentRef(att),
            display: display.clone(),
        });
        self.request(att, Request::SetDisplay(display))
    }

    pub(crate) fn credit(&mut self, att: u64, bytes: u32) -> BackendResult<()> {
        self.calls.push(FakeCall::Credit {
            att: FakeAttachmentRef(att),
            bytes,
        });
        let state = self.attachments[&att].state;
        if state == State::Closed {
            return Err(BackendError::Closed);
        }
        if matches!(state, State::Ready | State::Draining) && self.config.capabilities.byte_credit {
            let entry = self.attachments.get_mut(&att).expect("attachment");
            entry.credit += u64::from(bytes);
            self.deliver(att);
        }
        Ok(())
    }

    pub(crate) fn detach(&mut self, att: u64) {
        self.calls.push(FakeCall::Detach {
            att: FakeAttachmentRef(att),
        });
        self.close(att, Some(CloseReason::Detached));
    }

    /// The viewer went away: halves close, the PTY gives up its size and the stream ends once
    /// it drains. `reason` is what the stream still says.
    pub(crate) fn close(&mut self, id: u64, reason: Option<CloseReason>) {
        let Some(att) = self.attachments.get_mut(&id) else {
            return;
        };
        if att.state == State::Closed {
            return;
        }
        att.close_halves();
        att.state = State::Closed;
        att.pipe.clear();
        att.queue.clear();
        att.queued_input = 0;
        att.wake();
        self.leave_active(id, true);
        if let Some(reason) = reason {
            let att = self.attachments.get_mut(&id).expect("attachment");
            let generation = att.generation;
            att.push_out(generation, AttachEvent::Closed(reason));
        }
    }

    // ---- faults ----

    pub(crate) fn delay_output(&mut self, term: &TermId) {
        let index = self.index_of(term);
        self.terminals[index].delayed = true;
    }

    pub(crate) fn release_output(&mut self, term: &TermId) {
        let index = self.index_of(term);
        self.terminals[index].delayed = false;
        let address = self.terminals[index].address.clone();
        for id in self.attachment_ids(&address) {
            self.deliver(id);
        }
    }

    pub(crate) fn fail_next_opens(&mut self, term: &TermId, count: u32) {
        let index = self.index_of(term);
        self.terminals[index].failing_opens = count;
    }

    pub(crate) fn fail_half(&mut self, att: FakeAttachmentRef, half: Half) {
        let Some(entry) = self.attachments.get(&att.0) else {
            return;
        };
        if entry.observe && half == Half::Input {
            return;
        }
        self.begin_reopen(att.0, true);
    }

    /// Both halves close and the undelivered pipe is discarded, the outbox kept. A reopen then
    /// waits for `step`.
    fn begin_reopen(&mut self, id: u64, restore_size: bool) {
        let att = self.attachments.get_mut(&id).expect("attachment");
        if att.state != State::Ready {
            return;
        }
        att.close_halves();
        att.pipe.clear();
        att.state = State::Reopening;
        att.attempts = 0;
        self.leave_active(id, restore_size);
    }

    fn restart_all(&mut self) {
        for id in self.attachments.keys().copied().collect::<Vec<_>>() {
            self.begin_reopen(id, false);
        }
        let live: Vec<String> = self
            .terminals
            .iter()
            .filter(|terminal| terminal.running)
            .map(|terminal| terminal.address.clone())
            .collect();
        for address in live {
            self.raise(
                &address,
                TermEvent::Restarted {
                    id: term_id(&address),
                },
            );
        }
    }

    pub(crate) fn restart_backend(&mut self) {
        self.restart_all();
    }

    pub(crate) fn crash_backend(&mut self) {
        self.crashed = true;
        for terminal in &mut self.terminals {
            terminal.fidelity = Fidelity::Inexact;
        }
        self.restart_all();
    }

    pub(crate) fn set_late_old_generation_events(&mut self, on: bool) {
        self.config.late_old_generation_events = on;
    }

    pub(crate) fn set_paint_after_resize(&mut self, on: bool) {
        self.config.paint_after_resize = on;
    }

    /// One reopen attempt for each reopening attachment, in id order.
    pub(crate) fn step(&mut self) -> bool {
        let mut happened = false;
        let reopening: Vec<u64> = self
            .attachments
            .iter()
            .filter(|(_, att)| att.state == State::Reopening)
            .map(|(id, _)| *id)
            .collect();
        for id in reopening {
            happened = true;
            let index = self
                .index(&self.attachments[&id].address)
                .expect("terminal");
            if self.terminals[index].failing_opens > 0 {
                self.terminals[index].failing_opens -= 1;
                let max = self.config.max_reopen_attempts;
                let att = self.attachments.get_mut(&id).expect("attachment");
                att.attempts += 1;
                if att.attempts >= max {
                    let generation = att.generation;
                    att.pipe(generation, AttachEvent::Lost, false);
                    att.state = State::Closed;
                    att.queue.clear();
                    att.queued_input = 0;
                    self.deliver(id);
                    let att = self.attachments.get_mut(&id).expect("attachment");
                    att.wake();
                }
                continue;
            }
            self.reopen(id, index);
        }
        happened
    }

    fn reopen(&mut self, id: u64, index: usize) {
        let late = self.config.late_old_generation_events;
        let credit = u64::from(self.config.capabilities.initial_credit);
        let started = self.started(index);
        let paint = self.paint_for(index);
        let (cols, rows) = (self.terminals[index].cols, self.terminals[index].rows);
        let fidelity = self.terminals[index].fidelity;
        let att = self.attachments.get_mut(&id).expect("attachment");
        let old = att.generation;
        let new = old + 1;
        att.generation = new;
        att.display_open = true;
        att.input_open = !att.observe;
        att.credit = credit;
        att.state = State::Ready;
        att.attempts = 0;
        att.fidelity = fidelity;
        att.pipe(new, started, false);
        att.pipe(new, AttachEvent::Data(paint), true);
        if late {
            att.pipe(old, AttachEvent::Data(b"<late>".to_vec()), false);
            att.pipe(old, AttachEvent::Resized { cols, rows }, false);
        }
        let queued: Vec<Queued> = att.queue.drain(..).collect();
        att.queued_input = 0;
        self.deliver(id);
        let activations = self.terminals[index].activations;
        for queued in queued {
            if queued.activations == activations {
                self.apply(id, queued.request);
            } else {
                self.apply_superseded(id, queued.request);
            }
        }
        self.deliver(id);
    }

    // ---- observation ----

    pub(crate) fn pty_size(&self, term: &TermId) -> Option<(u16, u16)> {
        let terminal = &self.terminals[address_of(term).and_then(|a| self.index(a))?];
        Some((terminal.cols, terminal.rows))
    }

    pub(crate) fn typed_input(&self, term: &TermId) -> Vec<u8> {
        self.terminals[self.index_of(term)].typed.clone()
    }

    pub(crate) fn automatic_input(&self, term: &TermId) -> Vec<Vec<u8>> {
        self.terminals[self.index_of(term)].automatic.clone()
    }

    pub(crate) fn calls(&self) -> Vec<FakeCall> {
        self.calls.clone()
    }

    pub(crate) fn attachments_of(&self, term: &TermId) -> Vec<FakeAttachmentRef> {
        let address = &self.terminals[self.index_of(term)].address;
        self.attachment_ids(address)
            .into_iter()
            .map(FakeAttachmentRef)
            .collect()
    }

    pub(crate) fn open_halves(&self, term: &TermId) -> usize {
        let address = &self.terminals[self.index_of(term)].address;
        self.attachments
            .values()
            .filter(|att| &att.address == address)
            .map(Att::halves_open)
            .sum()
    }

    pub(crate) fn pending_output(&self, term: &TermId) -> usize {
        let address = &self.terminals[self.index_of(term)].address;
        self.attachments
            .values()
            .filter(|att| &att.address == address)
            .map(Att::pipe_data_bytes)
            .sum()
    }

    pub(crate) fn requested(&self, att: FakeAttachmentRef) -> ((u16, u16), Display) {
        self.attachments[&att.0].requested.clone()
    }

    pub(crate) fn filtered_events(&self) -> usize {
        self.filtered_events
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    Skip,
    Queue,
    Apply,
}
