//! Glue that runs a [`Core`] against a backend, on whatever runtime the caller has.
//!
//! The driver starts attachment calls and polls attachment streams with the caller's own
//! `Context`. A call is polled once when it starts. One that isn't done stays in flight, and
//! that attachment's later calls queue behind it, in the order core issued them. Other
//! attachments, connections and the server loop carry on. [`Driver::poll_event`] polls the calls
//! in flight, so the caller's waker is registered on them. An automatic reply that queues is
//! checked with core again just before it starts, against the ownership generation core
//! authorized it under, which the command carries: one that is no longer current is dropped
//! without a word.
//! It uses only `futures`: no locks, threads or clocks.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::task::{Context, Poll};

use combine_protocol::AttachmentId;
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};

use super::effects::{BackendCommand, ConnectionId, Effect, RefusedCommand};
use super::machine::Core;
use crate::model::Millis;
use crate::ports::{AttachEvents, Attachment, BackendError, BackendResult, TerminalBackend};

/// The most commands one attachment may have queued behind a call in flight.
pub const MAX_QUEUED_COMMANDS: usize = 256;

const QUEUE_FULL: &str = "an earlier backend call on this attachment hasn't completed";

type Opening = BoxFuture<'static, (AttachmentId, BackendResult<Box<dyn Attachment>>)>;

/// One attachment's backend handle and the commands waiting for it.
struct Slot {
    /// `None` while a call for this attachment is in flight.
    held: Option<Box<dyn Attachment>>,
    queued: VecDeque<BackendCommand>,
    /// Core has issued the detach, so a failure of an earlier call is no longer reported.
    detaching: bool,
}

enum Finished {
    Command {
        attachment: AttachmentId,
        held: Box<dyn Attachment>,
        refused: Option<RefusedCommand>,
        result: BackendResult<()>,
    },
    Detached,
}

pub struct Driver {
    core: Core,
    backend: Arc<dyn TerminalBackend>,
    attachments: BTreeMap<AttachmentId, Slot>,
    events: BTreeMap<AttachmentId, AttachEvents>,
    pending: FuturesUnordered<Opening>,
    /// Calls in flight. At shutdown they, and whatever is queued behind them (a detach
    /// included), are dropped with the driver without running, so their handles are dropped
    /// rather than detached: the server's `close_all` removes each connection without draining
    /// backend calls. A hardening note only: nothing waits on them.
    running: FuturesUnordered<BoxFuture<'static, Finished>>,
    last_polled: Option<AttachmentId>,
    journal: Option<Vec<BackendCommand>>,
    disconnects: Vec<ConnectionId>,
}

impl Driver {
    pub fn new(core: Core, backend: Arc<dyn TerminalBackend>) -> Driver {
        Driver {
            core,
            backend,
            attachments: BTreeMap::new(),
            events: BTreeMap::new(),
            pending: FuturesUnordered::new(),
            running: FuturesUnordered::new(),
            last_polled: None,
            journal: None,
            disconnects: Vec::new(),
        }
    }

    pub fn core(&self) -> &Core {
        &self.core
    }

    pub fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    pub fn backend(&self) -> &Arc<dyn TerminalBackend> {
        &self.backend
    }

    /// The attachment's backend handle. `None` while a call for it is in flight, and once it has
    /// been detached.
    pub fn attachment(&self, id: AttachmentId) -> Option<&dyn Attachment> {
        self.attachments
            .get(&id)
            .and_then(|slot| slot.held.as_deref())
    }

    /// Starts or stops recording each backend command, in the order core issues them. A command
    /// queued behind a call in flight is recorded when it is issued, not when it runs, and one
    /// that never runs (refused by a full queue, or a queued automatic reply dropped before it
    /// starts) is still recorded. The two orders differ only on that slow path.
    pub fn record_commands(&mut self, on: bool) {
        match (on, self.journal.is_some()) {
            (true, false) => self.journal = Some(Vec::new()),
            (false, true) => self.journal = None,
            _ => {}
        }
    }

    pub fn take_journal(&mut self) -> Vec<BackendCommand> {
        self.journal
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// The connections core asked to have closed since the last call.
    pub fn take_disconnects(&mut self) -> Vec<ConnectionId> {
        std::mem::take(&mut self.disconnects)
    }

    /// Runs core's effects until it has none left. An attach is started without waiting for it;
    /// whatever is already ready completes within the call. Each attachment call is started and
    /// polled once: one that isn't done stays in flight, and that attachment's later calls queue
    /// behind it, in order, while other attachments carry on.
    pub async fn run_effects(&mut self, now: Millis) {
        loop {
            let effects = self.core.take_effects();
            if effects.is_empty() {
                return;
            }
            for effect in effects {
                match effect {
                    Effect::Backend(command) => self.run(command, now),
                    Effect::Disconnect(connection) => self.disconnects.push(connection),
                }
            }
        }
    }

    fn run(&mut self, command: BackendCommand, now: Millis) {
        if let Some(journal) = &mut self.journal {
            journal.push(command.clone());
        }
        match command {
            BackendCommand::Attach {
                attachment,
                terminal,
                options,
            } => {
                let backend = Arc::clone(&self.backend);
                self.pending.push(Box::pin(async move {
                    (attachment, backend.attach(&terminal, options).await)
                }));
                self.complete_ready_attaches(now);
            }
            BackendCommand::Detach { attachment } => {
                self.events.remove(&attachment);
                self.submit(attachment, BackendCommand::Detach { attachment }, now);
            }
            other => self.submit(other.attachment(), other, now),
        }
    }

    /// Starts the command if the attachment is idle, and queues it otherwise.
    ///
    /// Idle means the handle is held and nothing is queued. A slot never holds its handle with
    /// commands still queued: `start_queued` runs whenever a call hands the handle back
    /// (`finish`), and stops only when the queue is empty or a call stays in flight, which takes
    /// the handle. So a held handle alone would do; the empty-queue check keeps a command from
    /// overtaking queued ones should that ever change.
    fn submit(&mut self, attachment: AttachmentId, command: BackendCommand, now: Millis) {
        let Some(slot) = self.attachments.get_mut(&attachment) else {
            return;
        };
        if slot.held.is_some() && slot.queued.is_empty() {
            self.start(attachment, command, now);
            return;
        }
        let full = slot.queued.len() >= MAX_QUEUED_COMMANDS;
        match command {
            BackendCommand::Detach { .. } => {
                slot.detaching = true;
                slot.queued.push_back(command);
            }
            BackendCommand::Credit { bytes, .. } => {
                if let Some(BackendCommand::Credit { bytes: tail, .. }) = slot.queued.back_mut() {
                    *tail = tail.saturating_add(bytes);
                } else {
                    slot.queued.push_back(command);
                }
            }
            _ if !full => slot.queued.push_back(command),
            _ => {
                if let Some(kind) = refusal(&command) {
                    let failure = BackendError::Failed(QUEUE_FULL.into());
                    self.core.command_failed(attachment, kind, &failure, now);
                }
            }
        }
    }

    /// Starts the attachment's next queued calls, for as long as each completes at once. A
    /// queued automatic reply that core no longer counts as current is dropped.
    fn start_queued(&mut self, attachment: AttachmentId, now: Millis) {
        loop {
            let Some(slot) = self.attachments.get_mut(&attachment) else {
                return;
            };
            if slot.held.is_none() {
                return;
            }
            let Some(command) = slot.queued.pop_front() else {
                return;
            };
            if let BackendCommand::WriteAutomatic {
                generation,
                ownership,
                ..
            } = &command
            {
                if !self
                    .core
                    .automatic_reply_current(attachment, *generation, *ownership)
                {
                    continue;
                }
            }
            self.start(attachment, command, now);
        }
    }

    /// Takes the idle attachment's handle, calls it once and completes the call if it is ready.
    /// A call that isn't ready stays in flight in `running`.
    fn start(&mut self, attachment: AttachmentId, command: BackendCommand, now: Millis) {
        let Some(slot) = self.attachments.get_mut(&attachment) else {
            return;
        };
        let Some(held) = slot.held.take() else {
            return;
        };
        let refused = refusal(&command);
        let mut call = if matches!(command, BackendCommand::Detach { .. }) {
            self.attachments.remove(&attachment);
            Box::pin(async move {
                let _ = held.detach().await;
                Finished::Detached
            })
        } else {
            call(attachment, held, refused, command)
        };
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        match call.as_mut().poll(&mut cx) {
            Poll::Ready(finished) => self.complete(finished, now),
            Poll::Pending => self.running.push(call),
        }
    }

    /// Takes the handle back and reports a failure. It never starts another call.
    fn complete(&mut self, finished: Finished, now: Millis) {
        let Finished::Command {
            attachment,
            held,
            refused,
            result,
        } = finished
        else {
            return;
        };
        let Some(slot) = self.attachments.get_mut(&attachment) else {
            return;
        };
        slot.held = Some(held);
        if let (Some(kind), Err(failure), false) = (refused, result, slot.detaching) {
            self.core.command_failed(attachment, kind, &failure, now);
        }
    }

    /// Completes a call that finished in `running`, then starts what was queued behind it.
    fn finish(&mut self, finished: Finished, now: Millis) {
        let attachment = match &finished {
            Finished::Command { attachment, .. } => Some(*attachment),
            Finished::Detached => None,
        };
        self.complete(finished, now);
        if let Some(attachment) = attachment {
            self.start_queued(attachment, now);
        }
    }

    /// Completes the attaches that are ready, without registering a waker.
    fn complete_ready_attaches(&mut self, now: Millis) {
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        while let Poll::Ready(Some((id, result))) = self.pending.poll_next_unpin(&mut cx) {
            self.complete_attach(id, result, now);
        }
    }

    fn complete_attach(
        &mut self,
        id: AttachmentId,
        result: BackendResult<Box<dyn Attachment>>,
        now: Millis,
    ) {
        match result {
            Ok(mut held) => {
                self.events.insert(id, held.events());
                let slot = Slot {
                    held: Some(held),
                    queued: VecDeque::new(),
                    detaching: false,
                };
                self.attachments.insert(id, slot);
                self.core.attach_result(id, Ok(()), now);
            }
            Err(failure) => self.core.attach_result(id, Err(failure), now),
        }
    }

    /// Handles one ready completion or backend event and returns `Ready`, or returns `Pending`
    /// with the caller's waker registered on every source. The caller runs
    /// [`run_effects`](Self::run_effects) afterwards.
    pub fn poll_event(&mut self, cx: &mut Context<'_>, now: Millis) -> Poll<()> {
        if let Poll::Ready(Some((id, result))) = self.pending.poll_next_unpin(cx) {
            self.complete_attach(id, result, now);
            return Poll::Ready(());
        }
        if let Poll::Ready(Some(finished)) = self.running.poll_next_unpin(cx) {
            self.finish(finished, now);
            return Poll::Ready(());
        }
        let ids: Vec<AttachmentId> = self.events.keys().copied().collect();
        let split = ids.partition_point(|id| self.last_polled.is_some_and(|last| *id <= last));
        for id in ids[split..].iter().chain(&ids[..split]) {
            let Some(stream) = self.events.get_mut(id) else {
                continue;
            };
            match stream.poll_next_unpin(cx) {
                Poll::Ready(Some((generation, event))) => {
                    self.last_polled = Some(*id);
                    self.core.backend_event(*id, generation, event, now);
                    return Poll::Ready(());
                }
                Poll::Ready(None) => {
                    self.last_polled = Some(*id);
                    self.events.remove(id);
                    self.core.backend_stream_ended(*id, now);
                    return Poll::Ready(());
                }
                Poll::Pending => {}
            }
        }
        Poll::Pending
    }

    /// Handles everything that is ready now, running effects after each, and returns how many
    /// events that took.
    pub async fn pump_ready(&mut self, now: Millis) -> usize {
        let mut handled = 0;
        loop {
            let ready = {
                let mut cx = Context::from_waker(futures::task::noop_waker_ref());
                self.poll_event(&mut cx, now).is_ready()
            };
            if !ready {
                return handled;
            }
            handled += 1;
            self.run_effects(now).await;
        }
    }
}

/// The refusal core tells the client about if the backend fails this command.
fn refusal(command: &BackendCommand) -> Option<RefusedCommand> {
    match command {
        BackendCommand::Write { .. } => Some(RefusedCommand::Input),
        BackendCommand::Resize { .. } => Some(RefusedCommand::Resize),
        BackendCommand::Focus { .. } => Some(RefusedCommand::Focus),
        BackendCommand::SetDisplay { .. } => Some(RefusedCommand::SetDisplay),
        _ => None,
    }
}

/// A future that owns the handle for one command and gives it back with the result.
fn call(
    attachment: AttachmentId,
    mut held: Box<dyn Attachment>,
    refused: Option<RefusedCommand>,
    command: BackendCommand,
) -> BoxFuture<'static, Finished> {
    Box::pin(async move {
        let result = match command {
            BackendCommand::Write {
                ownership, bytes, ..
            } => held.write(ownership, &bytes).await,
            BackendCommand::WriteAutomatic {
                generation, bytes, ..
            } => held.write_automatic(generation, &bytes).await,
            BackendCommand::Resize {
                ownership,
                cols,
                rows,
                ..
            } => held.resize(ownership, cols, rows).await,
            BackendCommand::Focus { ownership, .. } => held.focus(ownership).await,
            BackendCommand::SetDisplay {
                ownership, display, ..
            } => held.set_display(ownership, display).await,
            BackendCommand::Credit { bytes, .. } => held.credit(bytes).await,
            BackendCommand::Attach { .. } | BackendCommand::Detach { .. } => Ok(()),
        };
        Finished::Command {
            attachment,
            held,
            refused,
            result,
        }
    })
}
