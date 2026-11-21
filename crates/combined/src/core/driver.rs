//! Glue that runs a [`Core`] against a backend, on whatever runtime the caller has.
//!
//! The driver awaits port calls and polls attachment streams with the caller's own `Context`.
//! It uses only `futures`: no locks, threads or clocks.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::task::{Context, Poll};

use combine_protocol::AttachmentId;
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};

use super::effects::{BackendCommand, ConnectionId, Effect};
use super::machine::Core;
use crate::model::Millis;
use crate::ports::{AttachEvents, Attachment, BackendResult, TerminalBackend};

type Opening = BoxFuture<'static, (AttachmentId, BackendResult<Box<dyn Attachment>>)>;

pub struct Driver {
    core: Core,
    backend: Arc<dyn TerminalBackend>,
    attachments: BTreeMap<AttachmentId, Box<dyn Attachment>>,
    events: BTreeMap<AttachmentId, AttachEvents>,
    pending: FuturesUnordered<Opening>,
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

    pub fn attachment(&self, id: AttachmentId) -> Option<&dyn Attachment> {
        self.attachments
            .get(&id)
            .map(|attachment| attachment.as_ref())
    }

    /// Starts or stops recording each backend command, in the order it runs.
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
    /// whatever is already ready completes within the call.
    pub async fn run_effects(&mut self, now: Millis) {
        loop {
            let effects = self.core.take_effects();
            if effects.is_empty() {
                return;
            }
            for effect in effects {
                match effect {
                    Effect::Backend(command) => self.run(command, now).await,
                    Effect::Disconnect(connection) => self.disconnects.push(connection),
                }
            }
        }
    }

    async fn run(&mut self, command: BackendCommand, now: Millis) {
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
                if let Some(held) = self.attachments.remove(&attachment) {
                    let _ = held.detach().await;
                }
            }
            BackendCommand::Write { attachment, bytes } => {
                if let Some(held) = self.attachments.get_mut(&attachment) {
                    let _ = held.write(&bytes).await;
                }
            }
            BackendCommand::WriteAutomatic { attachment, bytes } => {
                if let Some(held) = self.attachments.get_mut(&attachment) {
                    let _ = held.write_automatic(&bytes).await;
                }
            }
            BackendCommand::Resize {
                attachment,
                cols,
                rows,
            } => {
                if let Some(held) = self.attachments.get_mut(&attachment) {
                    let _ = held.resize(cols, rows).await;
                }
            }
            BackendCommand::Focus { attachment } => {
                if let Some(held) = self.attachments.get_mut(&attachment) {
                    let _ = held.focus().await;
                }
            }
            BackendCommand::SetDisplay {
                attachment,
                display,
            } => {
                if let Some(held) = self.attachments.get_mut(&attachment) {
                    let _ = held.set_display(display).await;
                }
            }
            BackendCommand::Credit { attachment, bytes } => {
                if let Some(held) = self.attachments.get_mut(&attachment) {
                    let _ = held.credit(bytes).await;
                }
            }
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
                self.attachments.insert(id, held);
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
