//! Drives a daemon [`Core`] against a [`Target`]'s backend, with simulated clients.
//!
//! Time is virtual: [`Harness::advance`] moves it. Everything else runs in real time on the
//! target's runtime, and a wait that gives up fails the case with check `settle`.

use std::fmt;

use combine_protocol::{
    Attach, AttachRequestOptions, AttachmentId, ClientMessage, Cursor, DaemonMessage, Detach,
    Display, ErrorMessage, Focus, Input, OwnershipGeneration, RequestOwnership, Resize, Resume,
    SetDisplay, TermId, MAX_INPUT_CHUNK,
};
use combined::core::{BackendCommand, ConnectionId, Core, CoreConfig, Driver, LogStats};
use combined::model::Millis;
use futures::future::{poll_fn, select, Either};

use super::client::{ClientSim, ReadPolicy};
use super::generator::{Nonces, Payload};
use super::target::{Target, Unsupported};

/// Bytes an automatic read takes from the daemon in one round.
const READ_BYTES: usize = 1_048_576;

/// A broken expectation. `check` is a stable id (`payload-length`), which the mutation tests
/// key on. Waits that give up use `settle`; plumbing errors use `harness`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub check: &'static str,
    pub message: String,
}

impl Failure {
    pub fn new(check: &'static str, message: impl Into<String>) -> Self {
        Failure {
            check,
            message: message.into(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.check, self.message)
    }
}

impl From<Unsupported> for Failure {
    fn from(unsupported: Unsupported) -> Self {
        Failure::new("unsupported", unsupported.0)
    }
}

pub type Outcome = Result<(), Failure>;

/// `ensure!(check_id, cond, "fmt", args)` returns `Err(Failure { check: check_id, .. })` from
/// the enclosing function when `cond` is false.
#[allow(unused_macros)]
macro_rules! ensure {
    ($check:expr, $cond:expr, $($message:tt)+) => {
        if !$cond {
            return Err($crate::contract::harness::Failure::new($check, format!($($message)+)));
        }
    };
}

pub struct Harness<'t, T: Target> {
    pub target: &'t mut T,
    pub driver: Driver,
    pub now: Millis,
    pub config: CoreConfig,
    /// Connections core asked to close, in the order it asked.
    pub disconnected: Vec<ConnectionId>,
    pub nonces: Nonces,
}

impl<'t, T: Target> Harness<'t, T> {
    pub fn new(target: &'t mut T, config: CoreConfig) -> Result<Self, Failure> {
        let backend = target.backend();
        let core = Core::new(config.clone(), backend.capabilities())
            .map_err(|error| Failure::new("harness", format!("core config: {error}")))?;
        let mut driver = Driver::new(core, backend);
        driver.record_commands(true);
        Ok(Harness {
            target,
            driver,
            now: Millis(0),
            config,
            disconnected: Vec::new(),
            nonces: Nonces::new(),
        })
    }

    /// Bounds small enough for a case to reach them: a 512 KiB, 512-event log.
    pub fn small_caps() -> CoreConfig {
        CoreConfig {
            log_max_bytes: 524_288,
            log_max_events: 512,
            credit_reserve: 16_384,
            credit_min_grant: 16_384,
            ..CoreConfig::default()
        }
    }

    pub fn client(&mut self) -> ClientSim {
        let mut client = ClientSim::new();
        client.connection = Some(self.driver.core_mut().connect());
        client
    }

    /// Starts a terminal in the target's scratch directory `dir_name`.
    pub async fn spawn(&mut self, dir_name: &str, cols: u16, rows: u16) -> Result<TermId, Failure> {
        let cwd = self.target.scratch_dir(dir_name);
        let spec = self.target.spawn_spec(&cwd, cols, rows);
        self.driver
            .backend()
            .spawn(spec)
            .await
            .map_err(|error| Failure::new("harness", format!("spawn failed: {error}")))
    }

    async fn effects(&mut self) {
        self.driver.run_effects(self.now).await;
        self.disconnected.extend(self.driver.take_disconnects());
    }

    /// Handles everything the backend has ready, and returns how many events that took.
    async fn pump(&mut self) -> usize {
        self.effects().await;
        let handled = self.driver.pump_ready(self.now).await;
        self.disconnected.extend(self.driver.take_disconnects());
        handled
    }

    async fn deliver(&mut self, connection: ConnectionId, message: ClientMessage) {
        self.driver
            .core_mut()
            .client_message(connection, message, self.now);
        self.effects().await;
    }

    pub async fn send(&mut self, c: &mut ClientSim, message: ClientMessage) -> Outcome {
        let connection = c
            .connection
            .ok_or_else(|| Failure::new("harness", "the client has no connection"))?;
        self.deliver(connection, message).await;
        Ok(())
    }

    /// Sends the acks the client's policy allows and the replies it produced. True if any went.
    async fn flush(&mut self, c: &mut ClientSim) -> bool {
        let mut messages = c.take_acks();
        messages.append(&mut c.outgoing);
        let Some(connection) = c.connection else {
            return false;
        };
        let any = !messages.is_empty();
        for message in messages {
            self.deliver(connection, message).await;
        }
        any
    }

    /// An automatic read: applies what the client has waiting and what the daemon has queued,
    /// acks, and sends the replies.
    async fn serve(&mut self, c: &mut ClientSim) -> bool {
        if c.read != ReadPolicy::Auto {
            return false;
        }
        let Some(connection) = c.connection else {
            return false;
        };
        let mut batch: Vec<DaemonMessage> = c.inbox.drain(..).collect();
        batch.extend(
            self.driver
                .core_mut()
                .pull(connection, READ_BYTES, self.now),
        );
        for message in &batch {
            c.apply(message);
        }
        let sent = self.flush(c).await;
        sent || !batch.is_empty()
    }

    /// One settle round. True if anything moved.
    async fn round(&mut self, clients: &mut [&mut ClientSim]) -> bool {
        let now = self.now;
        let mut moved = false;
        {
            let Harness { target, driver, .. } = &mut *self;
            let polled = poll_fn(|cx| driver.poll_event(cx, now));
            let idle = target.idle();
            futures::pin_mut!(polled, idle);
            match select(polled, idle).await {
                Either::Left(_) => moved = true,
                Either::Right((stepped, _)) => moved |= stepped,
            }
        }
        moved |= self.pump().await > 0;
        for client in clients.iter_mut() {
            moved |= self.serve(client).await;
        }
        moved
    }

    /// Runs rounds until nothing has moved for `quiet_rounds` rounds in a row.
    pub async fn settle(&mut self, clients: &mut [&mut ClientSim]) -> Outcome {
        let quiet_rounds = self.target.quiet_rounds();
        let max_rounds = self.target.max_rounds();
        let mut quiet = 0;
        for _ in 0..max_rounds {
            if self.round(clients).await {
                quiet = 0;
            } else {
                quiet += 1;
                if quiet >= quiet_rounds {
                    return Ok(());
                }
            }
        }
        Err(Failure::new(
            "settle",
            format!("still moving after {max_rounds} rounds"),
        ))
    }

    /// Runs rounds until `pred` holds. Fails if everything goes quiet first, or after
    /// `max_rounds`.
    pub async fn settle_until(
        &mut self,
        clients: &mut [&mut ClientSim],
        what: &str,
        mut pred: impl FnMut(&[&mut ClientSim]) -> bool,
    ) -> Outcome {
        let quiet_rounds = self.target.quiet_rounds();
        let max_rounds = self.target.max_rounds();
        let mut quiet = 0;
        for _ in 0..max_rounds {
            if pred(&*clients) {
                return Ok(());
            }
            if self.round(clients).await {
                quiet = 0;
            } else {
                quiet += 1;
                if quiet >= quiet_rounds {
                    return if pred(&*clients) {
                        Ok(())
                    } else {
                        Err(Failure::new("settle", format!("waited for {what}")))
                    };
                }
            }
        }
        Err(Failure::new(
            "settle",
            format!("waited for {what} through {max_rounds} rounds"),
        ))
    }

    /// Attaches, and returns once the client has applied `attached` and `Started`, and the
    /// run has settled. An error answer is a `Failure` with check `attach`.
    pub async fn attach(
        &mut self,
        c: &mut ClientSim,
        term: &TermId,
        options: AttachRequestOptions,
    ) -> Result<AttachmentId, Failure> {
        match self.try_attach(c, term, options).await? {
            Ok(attachment) => Ok(attachment),
            Err(error) => Err(Failure::new(
                "attach",
                format!("attach was refused: {:?} ({})", error.code, error.message),
            )),
        }
    }

    /// Like `attach`, but hands back the daemon's error instead of failing on it.
    pub async fn try_attach(
        &mut self,
        c: &mut ClientSim,
        term: &TermId,
        options: AttachRequestOptions,
    ) -> Result<Result<AttachmentId, ErrorMessage>, Failure> {
        let request = c.next_request();
        c.expect_attach(request, term.clone());
        self.send(
            c,
            ClientMessage::Attach(Attach {
                request,
                terminal: term.clone(),
                options,
                instance: None,
            }),
        )
        .await?;
        let saved = std::mem::replace(&mut c.read, ReadPolicy::Auto);
        let waited = self
            .settle_until(
                &mut [&mut *c],
                "attached and started",
                |clients| match clients[0].attach_answer(request) {
                    None => false,
                    Some(Err(_)) => true,
                    Some(Ok(attachment)) => clients[0]
                        .view(attachment)
                        .is_some_and(|view| !view.started.is_empty()),
                },
            )
            .await;
        let settled = match waited {
            Ok(()) => self.settle(&mut [&mut *c]).await,
            failed => failed,
        };
        c.read = saved;
        settled?;
        c.attach_answer(request)
            .ok_or_else(|| Failure::new("harness", "the attach was never answered"))
    }

    pub async fn type_input(
        &mut self,
        c: &mut ClientSim,
        attachment: AttachmentId,
        bytes: &[u8],
    ) -> Outcome {
        for chunk in bytes.chunks(MAX_INPUT_CHUNK) {
            let input = Input {
                attachment,
                bytes: chunk.to_vec(),
            };
            self.send(c, ClientMessage::Input(input)).await?;
        }
        Ok(())
    }

    /// Resizes under the ownership generation the client last heard of.
    pub async fn resize(
        &mut self,
        c: &mut ClientSim,
        attachment: AttachmentId,
        cols: u16,
        rows: u16,
    ) -> Outcome {
        let ownership_generation = c.generation(attachment).ok_or_else(|| {
            Failure::new(
                "harness",
                "the client has heard no ownership for the attachment",
            )
        })?;
        let resize = Resize {
            attachment,
            cols,
            rows,
            ownership_generation,
        };
        self.send(c, ClientMessage::Resize(resize)).await
    }

    pub async fn focus(&mut self, c: &mut ClientSim, attachment: AttachmentId) -> Outcome {
        self.send(c, ClientMessage::Focus(Focus { attachment }))
            .await
    }

    pub async fn set_display(
        &mut self,
        c: &mut ClientSim,
        attachment: AttachmentId,
        display: Display,
    ) -> Outcome {
        let set = SetDisplay {
            attachment,
            display,
        };
        self.send(c, ClientMessage::SetDisplay(set)).await
    }

    pub async fn request_ownership(
        &mut self,
        c: &mut ClientSim,
        attachment: AttachmentId,
    ) -> Outcome {
        self.send(
            c,
            ClientMessage::RequestOwnership(RequestOwnership { attachment }),
        )
        .await
    }

    pub async fn detach(&mut self, c: &mut ClientSim, attachment: AttachmentId) -> Outcome {
        self.send(c, ClientMessage::Detach(Detach { attachment }))
            .await
    }

    /// The program prints `bytes`, and the daemon takes in what the backend has ready.
    pub async fn emit(&mut self, term: &TermId, bytes: &[u8]) -> Outcome {
        self.target.emit(term, bytes).await?;
        self.pump().await;
        Ok(())
    }

    /// Emits a fresh marker and settles until every automatic client has applied it. Each
    /// client measures from it. Returns the marker's bytes.
    pub async fn mark(
        &mut self,
        term: &TermId,
        clients: &mut [&mut ClientSim],
    ) -> Result<Vec<u8>, Failure> {
        let nonce = self.nonces.next();
        let marker = T::marker(&nonce);
        for client in clients.iter_mut() {
            client.expect_marker(marker.clone());
        }
        self.emit(term, &marker).await?;
        let term = term.clone();
        self.settle_until(clients, "the marker", |clients| {
            clients.iter().all(|client| {
                client.read != ReadPolicy::Auto
                    || client.connection.is_none()
                    || client
                        .views
                        .values()
                        .filter(|view| view.terminal == term && view.live())
                        .all(|view| view.marker_seen)
            })
        })
        .await?;
        Ok(marker)
    }

    /// Emits the payload `piece` bytes at a time, settling after each piece.
    pub async fn emit_paced(
        &mut self,
        term: &TermId,
        payload: &Payload,
        piece: usize,
        clients: &mut [&mut ClientSim],
    ) -> Outcome {
        for chunk in payload.pieces(piece) {
            self.emit(term, chunk).await?;
            self.settle(clients).await?;
        }
        Ok(())
    }

    /// Takes the next message the daemon has for the client, applying it (and acking, under
    /// the client's policy) when `apply` is set. With `apply` false the message is consumed
    /// and lost, as if the connection died with it in flight.
    pub async fn read_step(&mut self, c: &mut ClientSim, apply: bool) -> Option<DaemonMessage> {
        self.pump().await;
        let message = match c.inbox.pop_front() {
            Some(message) => message,
            None => {
                let connection = c.connection?;
                let mut batch = self.driver.core_mut().pull(connection, 0, self.now);
                if batch.is_empty() {
                    return None;
                }
                let first = batch.remove(0);
                c.inbox.extend(batch);
                first
            }
        };
        if apply {
            c.apply(&message);
            self.flush(c).await;
        }
        Some(message)
    }

    /// Reads and applies messages until one satisfies `pred`; the client acks it under its
    /// policy.
    pub async fn read_until(
        &mut self,
        c: &mut ClientSim,
        mut pred: impl FnMut(&DaemonMessage) -> bool,
    ) -> Outcome {
        let max_rounds = self.target.max_rounds();
        for _ in 0..max_rounds {
            match self.read_step(c, true).await {
                Some(message) if pred(&message) => return Ok(()),
                Some(_) => {}
                None => {
                    self.round(&mut []).await;
                }
            }
        }
        Err(Failure::new(
            "settle",
            format!("no message satisfied the wait through {max_rounds} rounds"),
        ))
    }

    /// Closes the client's connection. Core keeps its attachments for the grace period.
    pub fn drop_connection(&mut self, c: &mut ClientSim) {
        if let Some(connection) = c.connection.take() {
            self.driver.core_mut().disconnect(connection, self.now);
        }
        c.inbox.clear();
        c.outgoing.clear();
    }

    /// Opens a new connection and resumes each live attachment from its last applied cursor,
    /// then settles. The old connection is left as it is.
    pub async fn reconnect(&mut self, c: &mut ClientSim) -> Outcome {
        let connection = self.driver.core_mut().connect();
        c.connection = Some(connection);
        c.inbox.clear();
        c.outgoing.clear();
        let held: Vec<Cursor> = c
            .views
            .values()
            .filter(|view| view.live())
            .filter_map(|view| view.last_applied)
            .collect();
        let mut requests = Vec::new();
        for cursor in held {
            let request = c.next_request();
            self.deliver(
                connection,
                ClientMessage::Resume(Resume {
                    request,
                    cursor,
                    instance: None,
                }),
            )
            .await;
            requests.push(request);
        }
        if !requests.is_empty() {
            self.pump().await;
            let queued = self.driver.core_mut().pull(connection, 0, self.now);
            c.inbox.extend(queued);
            while requests.iter().any(|request| !c.resume_answered(*request)) {
                let Some(message) = c.inbox.pop_front() else {
                    return Err(Failure::new("harness", "a resume was never answered"));
                };
                c.apply(&message);
            }
            self.flush(c).await;
        }
        self.settle(&mut [&mut *c]).await
    }

    /// Moves virtual time forward, lets core act on it and returns the connections it closed.
    pub async fn advance(&mut self, ms: u64) -> Result<Vec<ConnectionId>, Failure> {
        self.now = self.now.saturating_add(Millis(ms));
        self.driver.core_mut().tick(self.now);
        self.effects().await;
        Ok(std::mem::take(&mut self.disconnected))
    }

    /// Replaces the daemon with a new one over the same backend. IDs continue above the old
    /// ones, and every client loses its connection.
    pub async fn restart_daemon(&mut self, clients: &mut [&mut ClientSim]) -> Outcome {
        let config = CoreConfig {
            id_base: self.driver.core().high_water() + 1,
            ..self.config.clone()
        };
        let backend = self.target.backend();
        let core = Core::new(config.clone(), backend.capabilities())
            .map_err(|error| Failure::new("harness", format!("core config: {error}")))?;
        let mut driver = Driver::new(core, backend);
        driver.record_commands(true);
        self.driver = driver;
        self.config = config;
        for client in clients.iter_mut() {
            client.connection = None;
            client.inbox.clear();
            client.outgoing.clear();
        }
        Ok(())
    }

    pub fn stats(&self, attachment: AttachmentId) -> Option<LogStats> {
        self.driver.core().attachment_stats(attachment)
    }

    /// The backend commands run since the last call.
    /// The backend commands since the last call. The ownership generation on the commands that
    /// carry one is zeroed, so a case compares what was asked, not which generation it was.
    pub fn journal(&mut self) -> Vec<BackendCommand> {
        self.driver
            .take_journal()
            .into_iter()
            .map(|command| match command {
                BackendCommand::Write {
                    attachment, bytes, ..
                } => BackendCommand::Write {
                    attachment,
                    ownership: OwnershipGeneration(0),
                    bytes,
                },
                BackendCommand::Resize {
                    attachment,
                    cols,
                    rows,
                    ..
                } => BackendCommand::Resize {
                    attachment,
                    ownership: OwnershipGeneration(0),
                    cols,
                    rows,
                },
                BackendCommand::Focus { attachment, .. } => BackendCommand::Focus {
                    attachment,
                    ownership: OwnershipGeneration(0),
                },
                BackendCommand::SetDisplay {
                    attachment,
                    display,
                    ..
                } => BackendCommand::SetDisplay {
                    attachment,
                    ownership: OwnershipGeneration(0),
                    display,
                },
                other => other,
            })
            .collect()
    }

    /// Fails with check `client-invariant` if any client saw the stream break an invariant.
    pub fn finish(&self, clients: &[&ClientSim]) -> Outcome {
        let violations: Vec<&String> = clients
            .iter()
            .flat_map(|client| client.violations.iter())
            .collect();
        ensure!(
            "client-invariant",
            violations.is_empty(),
            "{} violation(s): {violations:?}",
            violations.len()
        );
        Ok(())
    }
}
