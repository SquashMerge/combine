//! The state machine: client messages and backend events in, queued messages and effects out.
//!
//! `Core` is synchronous and single-owner. It reads no clock (time arrives as [`Millis`]), does
//! no I/O and never waits. The caller feeds it inputs, drains [`Core::take_effects`] after each
//! call and runs what it finds, and writes what [`Core::pull`] returns to the connection.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use combine_protocol::{
    Attach, AttachRequestOptions, Attached, AttachmentId, AutomaticReply, BackendGeneration,
    ClientMessage, CloseReason, Cursor, DaemonMessage, Display, Epoch, ErrorCode, ErrorMessage,
    Gap, Ownership, OwnershipGeneration, RequestId, Resumed, TermId,
};

use super::config::{validate, ConfigError, CoreConfig};
use super::effects::{BackendCommand, ConnectionId, Effect, RefusedCommand};
use super::log::{AckError, Appended, EventLog, LogEnd, LogLimits, LogStats};
use super::ownership::{OwnershipChange, TerminalOwnership};
use crate::model::Millis;
use crate::ports::{AttachEvent, AttachOptions, BackendError, BackendResult, Capabilities};

/// Who owns a terminal's input, and under which generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipState {
    pub generation: OwnershipGeneration,
    pub owner: Option<AttachmentId>,
}

const UNKNOWN_ATTACHMENT: &str = "unknown attachment";
const WATCH_ONLY: &str = "the attachment is watch-only";
const STALE_GENERATION: &str = "stale ownership generation";
const ZERO_SIZE: &str = "the size must be at least 1 column and 1 row";
const NOT_SENT: &str = "the cursor names an event that was not sent";
const UNEXPECTED_HELLO: &str = "unexpected hello";
const UNKNOWN_TERMINAL: &str = "unknown terminal";
const ATTACH_FAILED: &str = "the backend could not attach";
const REFUSED_INPUT: &str = "the backend refused input for this attachment";
const REFUSED_RESIZE: &str = "the backend refused the resize for this attachment";
const REFUSED_FOCUS: &str = "the backend refused the focus for this attachment";
const REFUSED_DISPLAY: &str = "the backend refused the display change for this attachment";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Opening,
    Attached,
    Live {
        generation: BackendGeneration,
        epoch: Epoch,
    },
    Ended(LogEnd),
}

struct Attachment {
    request: RequestId,
    terminal: TermId,
    connection: Option<ConnectionId>,
    state: State,
    watch_only: bool,
    log: EventLog,
    cols: u16,
    rows: u16,
    display: Display,
    grace_deadline: Option<Millis>,
    gap_appended_at: Option<Millis>,
    detach_sent: bool,
}

impl Attachment {
    fn joined(&self) -> bool {
        matches!(self.state, State::Attached | State::Live { .. })
    }
}

#[derive(Default)]
struct Connection {
    control: VecDeque<DaemonMessage>,
    last_served: Option<AttachmentId>,
}

pub struct Core {
    config: CoreConfig,
    capabilities: Capabilities,
    next_id: u64,
    next_connection: u64,
    next_focus: u64,
    connections: BTreeMap<ConnectionId, Connection>,
    attachments: BTreeMap<AttachmentId, Attachment>,
    ownerships: BTreeMap<String, TerminalOwnership>,
    effects: Vec<Effect>,
    /// Connections whose control queue overflowed during the current call. They are dropped when
    /// it finishes, so a hand-off never announces in the middle of another announce.
    overflowed: BTreeSet<ConnectionId>,
}

fn take(counter: &mut u64) -> u64 {
    let value = *counter;
    *counter += 1;
    value
}

fn error(
    code: ErrorCode,
    message: &str,
    request: Option<RequestId>,
    attachment: Option<AttachmentId>,
) -> DaemonMessage {
    DaemonMessage::Error(ErrorMessage {
        code,
        message: message.to_string(),
        request,
        attachment,
    })
}

impl Core {
    pub fn new(config: CoreConfig, capabilities: Capabilities) -> Result<Core, ConfigError> {
        validate(&config, &capabilities)?;
        Ok(Core {
            next_id: config.id_base,
            config,
            capabilities,
            next_connection: 1,
            next_focus: 1,
            connections: BTreeMap::new(),
            attachments: BTreeMap::new(),
            ownerships: BTreeMap::new(),
            effects: Vec::new(),
            overflowed: BTreeSet::new(),
        })
    }

    pub fn config(&self) -> &CoreConfig {
        &self.config
    }

    /// The last value the ID counter issued, so the next incarnation can start above it. Before
    /// anything is issued it is one below `id_base`.
    pub fn high_water(&self) -> u64 {
        self.next_id.saturating_sub(1)
    }

    pub fn connect(&mut self) -> ConnectionId {
        let id = ConnectionId(take(&mut self.next_connection));
        self.connections.insert(id, Connection::default());
        id
    }

    /// Drops the connection and its queued control messages. Its attachments are kept unbound
    /// for the grace period, except a finished gap's, which go at once, and those still opening,
    /// which are abandoned.
    pub fn disconnect(&mut self, connection: ConnectionId, now: Millis) {
        self.drop_connection(connection, now);
        self.drop_overflowed(now);
    }

    /// Drops every connection whose control queue overflowed. A drop can hand ownership off and
    /// overflow another queue, so this runs until none is left.
    fn drop_overflowed(&mut self, now: Millis) {
        while let Some(connection) = self.overflowed.pop_first() {
            self.drop_connection(connection, now);
        }
    }

    fn drop_connection(&mut self, connection: ConnectionId, now: Millis) {
        if self.connections.remove(&connection).is_none() {
            return;
        }
        let bound: Vec<AttachmentId> = self
            .attachments
            .iter()
            .filter(|(_, a)| a.connection == Some(connection))
            .map(|(id, _)| *id)
            .collect();
        // Unbind every attachment before any hand-off runs, so ownership never passes to a
        // sibling that is leaving on the same connection.
        let mut unbound = Vec::new();
        for id in bound {
            let Some(attachment) = self.attachments.get_mut(&id) else {
                continue;
            };
            match attachment.state {
                State::Opening => {
                    self.attachments.remove(&id);
                }
                State::Ended(LogEnd::Gap) => unbound.push((id, true)),
                _ => {
                    attachment.connection = None;
                    attachment.grace_deadline = Some(now.saturating_add(self.config.grace));
                    unbound.push((id, false));
                }
            }
        }
        // Owners go last: by then every sibling is already ineligible.
        unbound.sort_by_key(|(id, _)| self.is_owner(*id));
        for (id, finished) in unbound {
            if finished {
                self.finish(id);
            } else {
                self.refresh_eligibility(id);
            }
        }
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    pub fn attachment_stats(&self, attachment: AttachmentId) -> Option<LogStats> {
        self.attachments.get(&attachment).map(|a| a.log.stats())
    }

    pub fn ownership(&self, terminal: &TermId) -> Option<OwnershipState> {
        self.ownerships
            .get(&terminal.0)
            .map(|ownership| ownership.state().clone())
    }

    /// Whether `pull` would return anything for the connection.
    pub fn has_output(&self, connection: ConnectionId) -> bool {
        let Some(state) = self.connections.get(&connection) else {
            return false;
        };
        !state.control.is_empty()
            || self
                .attachments
                .values()
                .any(|a| a.connection == Some(connection) && a.log.has_sendable())
    }

    /// Takes the connection's queued output: every control message first, then stream messages
    /// round-robin over its attachments, starting after the one served last.
    ///
    /// Stream messages keep coming while their summed cost is below `max_bytes`, and at least
    /// one comes if any is sendable. Sent entries stay in their logs until acked.
    pub fn pull(
        &mut self,
        connection: ConnectionId,
        max_bytes: usize,
        now: Millis,
    ) -> Vec<DaemonMessage> {
        let out = self.pull_messages(connection, max_bytes);
        self.drop_overflowed(now);
        out
    }

    fn pull_messages(&mut self, connection: ConnectionId, max_bytes: usize) -> Vec<DaemonMessage> {
        let Some(state) = self.connections.get_mut(&connection) else {
            return Vec::new();
        };
        let mut out: Vec<DaemonMessage> = state.control.drain(..).collect();
        let mut total = 0usize;
        let mut emitted = 0usize;
        while emitted == 0 || total < max_bytes {
            let Some(id) = self.next_to_serve(connection) else {
                break;
            };
            let Some(attachment) = self.attachments.get_mut(&id) else {
                break;
            };
            let cost = attachment.log.next_cost().unwrap_or_default();
            let Some(message) = attachment.log.next_message() else {
                break;
            };
            total += cost;
            emitted += 1;
            if let Some(state) = self.connections.get_mut(&connection) {
                state.last_served = Some(id);
            }
            let gap = matches!(message, DaemonMessage::Gap(_));
            out.push(message);
            if gap {
                self.finish(id);
            }
        }
        out
    }

    fn next_to_serve(&self, connection: ConnectionId) -> Option<AttachmentId> {
        let last = self.connections.get(&connection)?.last_served;
        let mut ready = self
            .attachments
            .iter()
            .filter(|(_, a)| a.connection == Some(connection) && a.log.has_sendable())
            .map(|(id, _)| *id);
        let first = ready.next()?;
        let after_last = std::iter::once(first)
            .chain(ready)
            .find(|id| last.is_none_or(|last| *id > last));
        Some(after_last.unwrap_or(first))
    }

    /// Finishes unbound attachments whose grace ran out, then disconnects each bound client that
    /// has left a final gap unsent for longer than the gap timeout.
    pub fn tick(&mut self, now: Millis) {
        self.expire(now);
        self.drop_overflowed(now);
    }

    fn expire(&mut self, now: Millis) {
        let expired: Vec<AttachmentId> = self
            .attachments
            .iter()
            .filter(|(_, a)| a.connection.is_none())
            .filter(|(_, a)| a.grace_deadline.is_some_and(|deadline| deadline <= now))
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.finish(id);
        }
        let stuck: BTreeSet<ConnectionId> = self
            .attachments
            .values()
            .filter(|a| a.state == State::Ended(LogEnd::Gap) && !a.log.gap_sent())
            .filter(|a| {
                a.gap_appended_at
                    .is_some_and(|at| at.saturating_add(self.config.gap_timeout) <= now)
            })
            .filter_map(|a| a.connection)
            .collect();
        for connection in stuck {
            self.effects.push(Effect::Disconnect(connection));
            self.drop_connection(connection, now);
        }
    }

    // ---- output queue ----

    /// Queues a control message. An overflow only marks the connection: it is dropped when the
    /// current call ends, never in the middle of a loop over members.
    fn push_control(&mut self, connection: ConnectionId, message: DaemonMessage) {
        let max = self.config.max_control_messages;
        let Some(state) = self.connections.get_mut(&connection) else {
            return;
        };
        if state.control.len() >= max {
            if self.overflowed.insert(connection) {
                self.effects.push(Effect::Disconnect(connection));
            }
            return;
        }
        state.control.push_back(message);
    }

    fn send_error(
        &mut self,
        connection: ConnectionId,
        code: ErrorCode,
        message: &str,
        request: Option<RequestId>,
        attachment: Option<AttachmentId>,
    ) {
        self.push_control(connection, error(code, message, request, attachment));
    }

    fn command(&mut self, command: BackendCommand) {
        self.effects.push(Effect::Backend(command));
    }

    // ---- client messages ----

    pub fn client_message(
        &mut self,
        connection: ConnectionId,
        message: ClientMessage,
        now: Millis,
    ) {
        self.dispatch(connection, message, now);
        self.drop_overflowed(now);
    }

    fn dispatch(&mut self, connection: ConnectionId, message: ClientMessage, now: Millis) {
        if !self.connections.contains_key(&connection) {
            return;
        }
        match message {
            ClientMessage::Hello(_) => self.send_error(
                connection,
                ErrorCode::InvalidRequest,
                UNEXPECTED_HELLO,
                None,
                None,
            ),
            ClientMessage::Attach(attach) => self.on_attach(connection, attach),
            ClientMessage::Resume(resume) => {
                self.on_resume(connection, resume.request, resume.cursor)
            }
            ClientMessage::Detach(detach) => self.on_detach(connection, detach.attachment, now),
            ClientMessage::Ack(ack) => self.on_ack(connection, ack.cursor),
            ClientMessage::Resize(resize) => self.on_resize(
                connection,
                resize.attachment,
                resize.cols,
                resize.rows,
                resize.ownership_generation,
            ),
            ClientMessage::Focus(focus) => self.on_focus(connection, focus.attachment),
            ClientMessage::SetDisplay(set) => {
                self.on_set_display(connection, set.attachment, set.display)
            }
            ClientMessage::RequestOwnership(request) => {
                self.on_request_ownership(connection, request.attachment)
            }
            ClientMessage::Input(input) => self.on_input(connection, input.attachment, input.bytes),
            ClientMessage::AutomaticReply(reply) => self.on_automatic_reply(connection, reply),
        }
    }

    /// Whether the connection holds the attachment, and if so whether it has ended. An attachment
    /// still opening is not yet the client's: it has no ID the client knows.
    fn held(&self, connection: ConnectionId, id: AttachmentId) -> Option<bool> {
        let attachment = self.attachments.get(&id)?;
        if attachment.connection != Some(connection) || attachment.state == State::Opening {
            return None;
        }
        Some(matches!(attachment.state, State::Ended(_)))
    }

    fn unknown_attachment(&mut self, connection: ConnectionId, id: AttachmentId) {
        self.send_error(
            connection,
            ErrorCode::UnknownAttachment,
            UNKNOWN_ATTACHMENT,
            None,
            Some(id),
        );
    }

    /// The gate for a message the client may send about its own, live attachment. It answers
    /// `unknown_attachment` when the connection doesn't hold it and returns false; an ended
    /// attachment also returns false, silently.
    fn active(&mut self, connection: ConnectionId, id: AttachmentId) -> bool {
        match self.held(connection, id) {
            None => {
                self.unknown_attachment(connection, id);
                false
            }
            Some(ended) => !ended,
        }
    }

    fn watch_only_error(&mut self, connection: ConnectionId, id: AttachmentId) -> bool {
        if self.attachments[&id].watch_only {
            self.send_error(connection, ErrorCode::WatchOnly, WATCH_ONLY, None, Some(id));
            return true;
        }
        false
    }

    fn on_attach(&mut self, connection: ConnectionId, attach: Attach) {
        let Attach {
            request,
            terminal,
            options,
        } = attach;
        if options.cols == 0 || options.rows == 0 {
            self.send_error(
                connection,
                ErrorCode::InvalidRequest,
                ZERO_SIZE,
                Some(request),
                None,
            );
            return;
        }
        let id = AttachmentId(take(&mut self.next_id));
        let AttachRequestOptions {
            cols,
            rows,
            ref display,
            observe,
            ..
        } = options;
        let attachment = Attachment {
            request,
            terminal: terminal.clone(),
            connection: Some(connection),
            state: State::Opening,
            watch_only: observe,
            log: EventLog::new(
                id,
                LogLimits::from(&self.config),
                self.capabilities.byte_credit,
            ),
            cols,
            rows,
            display: display.clone(),
            grace_deadline: None,
            gap_appended_at: None,
            detach_sent: false,
        };
        self.attachments.insert(id, attachment);
        self.command(BackendCommand::Attach {
            attachment: id,
            terminal,
            options: AttachOptions::from(options),
        });
    }

    fn on_resume(&mut self, connection: ConnectionId, request: RequestId, cursor: Cursor) {
        let id = cursor.attachment;
        let resumed = match self.attachments.get_mut(&id) {
            Some(a) if a.state != State::Opening => Some(a.log.resume(cursor)),
            _ => None,
        };
        let replay_through = match resumed {
            Some(Ok(replay_through)) => replay_through,
            other => {
                self.push_control(
                    connection,
                    DaemonMessage::Gap(Gap {
                        attachment: id,
                        request: Some(request),
                    }),
                );
                if other.is_some() {
                    self.finish(id);
                }
                return;
            }
        };
        if let Some(attachment) = self.attachments.get_mut(&id) {
            attachment.connection = Some(connection);
            attachment.grace_deadline = None;
        }
        self.refresh_eligibility(id);
        self.grant_credit(id);
        self.push_control(
            connection,
            DaemonMessage::Resumed(Resumed {
                request,
                attachment: id,
                replay_through,
            }),
        );
        self.push_ownership(connection, id);
    }

    fn push_ownership(&mut self, connection: ConnectionId, id: AttachmentId) {
        let Some(attachment) = self.attachments.get(&id) else {
            return;
        };
        let Some(ownership) = self.ownerships.get(&attachment.terminal.0) else {
            return;
        };
        let state = ownership.state().clone();
        self.push_control(
            connection,
            DaemonMessage::Ownership(Ownership {
                attachment: id,
                ownership_generation: state.generation,
                owner: state.owner,
            }),
        );
    }

    fn on_detach(&mut self, connection: ConnectionId, id: AttachmentId, now: Millis) {
        let Some(ended) = self.held(connection, id) else {
            self.unknown_attachment(connection, id);
            return;
        };
        if ended {
            self.finish(id);
            return;
        }
        let live = matches!(self.attachments[&id].state, State::Live { .. });
        if live {
            let result = self.append(id, &AttachEvent::Closed(CloseReason::Detached));
            self.settle(id, result, true, now);
        } else {
            if let Some(attachment) = self.attachments.get_mut(&id) {
                attachment.log.end_with_gap();
            }
            self.ended(id, LogEnd::Gap, now);
        }
    }

    fn on_ack(&mut self, connection: ConnectionId, cursor: Cursor) {
        let id = cursor.attachment;
        if !self.active_or_ended(connection, id) {
            return;
        }
        let Some(attachment) = self.attachments.get_mut(&id) else {
            return;
        };
        match attachment.log.ack(cursor) {
            Ok(true) => self.finish(id),
            Ok(false) => self.grant_credit(id),
            Err(AckError::NotSent) => self.send_error(
                connection,
                ErrorCode::InvalidRequest,
                NOT_SENT,
                None,
                Some(id),
            ),
        }
    }

    /// Like [`active`](Self::active), but an ended attachment passes too.
    fn active_or_ended(&mut self, connection: ConnectionId, id: AttachmentId) -> bool {
        if self.held(connection, id).is_none() {
            self.unknown_attachment(connection, id);
            return false;
        }
        true
    }

    fn on_resize(
        &mut self,
        connection: ConnectionId,
        id: AttachmentId,
        cols: u16,
        rows: u16,
        generation: OwnershipGeneration,
    ) {
        if !self.active(connection, id) || self.watch_only_error(connection, id) {
            return;
        }
        let Some(attachment) = self.attachments.get(&id) else {
            return;
        };
        let Some(ownership) = self.ownerships.get(&attachment.terminal.0) else {
            return;
        };
        if ownership.state().generation != generation {
            self.send_error(
                connection,
                ErrorCode::StaleGeneration,
                STALE_GENERATION,
                None,
                Some(id),
            );
            return;
        }
        if cols == 0 || rows == 0 {
            self.send_error(
                connection,
                ErrorCode::InvalidRequest,
                ZERO_SIZE,
                None,
                Some(id),
            );
            return;
        }
        let owner = ownership.is_owner(id);
        if let Some(attachment) = self.attachments.get_mut(&id) {
            attachment.cols = cols;
            attachment.rows = rows;
        }
        if owner {
            self.command(BackendCommand::Resize {
                attachment: id,
                cols,
                rows,
            });
        }
    }

    fn on_focus(&mut self, connection: ConnectionId, id: AttachmentId) {
        if !self.active(connection, id) {
            return;
        }
        let order = take(&mut self.next_focus);
        let Some(attachment) = self.attachments.get(&id) else {
            return;
        };
        let Some(ownership) = self.ownerships.get_mut(&attachment.terminal.0) else {
            return;
        };
        ownership.focus(id, order);
        if ownership.is_owner(id) {
            self.command(BackendCommand::Focus { attachment: id });
        }
    }

    fn on_set_display(&mut self, connection: ConnectionId, id: AttachmentId, display: Display) {
        if !self.active(connection, id) {
            return;
        }
        let Some(attachment) = self.attachments.get_mut(&id) else {
            return;
        };
        attachment.display = display.clone();
        if self.is_owner(id) && self.capabilities.display_queries {
            self.command(BackendCommand::SetDisplay {
                attachment: id,
                display,
            });
        }
    }

    fn is_owner(&self, id: AttachmentId) -> bool {
        self.attachments
            .get(&id)
            .and_then(|a| self.ownerships.get(&a.terminal.0))
            .is_some_and(|ownership| ownership.is_owner(id))
    }

    fn on_request_ownership(&mut self, connection: ConnectionId, id: AttachmentId) {
        if !self.active(connection, id) || self.watch_only_error(connection, id) {
            return;
        }
        if !self.is_owner(id) {
            self.grant(id, None);
        }
    }

    fn on_input(&mut self, connection: ConnectionId, id: AttachmentId, bytes: Vec<u8>) {
        if !self.active(connection, id) || self.watch_only_error(connection, id) {
            return;
        }
        if bytes.is_empty() {
            return;
        }
        if self.is_owner(id) {
            self.command(BackendCommand::Write {
                attachment: id,
                bytes,
            });
        } else {
            self.grant(id, Some(bytes));
        }
    }

    /// Replies from the terminal's emulator are forwarded only from the live, current owner.
    fn on_automatic_reply(&mut self, connection: ConnectionId, reply: AutomaticReply) {
        let id = reply.attachment;
        let Some(attachment) = self.attachments.get(&id) else {
            return;
        };
        let State::Live {
            generation: backend_generation,
            epoch,
        } = attachment.state
        else {
            return;
        };
        let generation = self
            .ownerships
            .get(&attachment.terminal.0)
            .map(|ownership| ownership.state().generation);
        if attachment.connection != Some(connection)
            || attachment.watch_only
            || reply.epoch != epoch
            || generation != Some(reply.ownership_generation)
            || !self.is_owner(id)
            || !self.capabilities.automatic_replies
        {
            return;
        }
        self.command(BackendCommand::WriteAutomatic {
            attachment: id,
            generation: backend_generation,
            bytes: reply.bytes,
        });
    }

    // ---- backend inputs ----

    /// The backend refused a command core sent for an attachment. The attachment's client is
    /// told in Combine's own words and nothing else changes: the backend's text stays out, and
    /// a `Closed` failure says nothing because the attachment's stream reports the end.
    pub fn command_failed(
        &mut self,
        attachment: AttachmentId,
        command: RefusedCommand,
        failure: &BackendError,
        now: Millis,
    ) {
        if !matches!(failure, BackendError::Closed) {
            let text = match command {
                RefusedCommand::Input => REFUSED_INPUT,
                RefusedCommand::Resize => REFUSED_RESIZE,
                RefusedCommand::Focus => REFUSED_FOCUS,
                RefusedCommand::SetDisplay => REFUSED_DISPLAY,
            };
            let connection = self.attachments.get(&attachment).and_then(|a| a.connection);
            if let Some(connection) = connection {
                self.send_error(
                    connection,
                    ErrorCode::Internal,
                    text,
                    None,
                    Some(attachment),
                );
            }
        }
        self.drop_overflowed(now);
    }

    pub fn attach_result(
        &mut self,
        attachment: AttachmentId,
        result: BackendResult<()>,
        now: Millis,
    ) {
        self.settle_attach(attachment, result);
        self.drop_overflowed(now);
    }

    fn settle_attach(&mut self, attachment: AttachmentId, result: BackendResult<()>) {
        let opening = self
            .attachments
            .get(&attachment)
            .is_some_and(|a| a.state == State::Opening);
        if !opening {
            let known = self.attachments.contains_key(&attachment);
            if !known && result.is_ok() {
                self.command(BackendCommand::Detach { attachment });
            }
            return;
        }
        match result {
            Ok(()) => self.opened(attachment),
            Err(failure) => {
                let Some(opening) = self.attachments.remove(&attachment) else {
                    return;
                };
                let Some(connection) = opening.connection else {
                    return;
                };
                let (code, text) = match failure {
                    BackendError::UnknownTerminal(_) | BackendError::RemoteTerminal(_) => {
                        (ErrorCode::UnknownTerminal, UNKNOWN_TERMINAL)
                    }
                    _ => (ErrorCode::AttachFailed, ATTACH_FAILED),
                };
                self.send_error(connection, code, text, Some(opening.request), None);
            }
        }
    }

    fn opened(&mut self, id: AttachmentId) {
        let Some(attachment) = self.attachments.get_mut(&id) else {
            return;
        };
        attachment.state = State::Attached;
        let (request, key, watch_only, connection) = (
            attachment.request,
            attachment.terminal.0.clone(),
            attachment.watch_only,
            attachment.connection,
        );
        let next_id = &mut self.next_id;
        self.ownerships
            .entry(key)
            .or_insert_with(|| TerminalOwnership::new(OwnershipGeneration(take(next_id))))
            .join(id, watch_only);
        let Some(connection) = connection else {
            return;
        };
        self.push_control(
            connection,
            DaemonMessage::Attached(Attached {
                request,
                attachment: id,
            }),
        );
        self.push_ownership(connection, id);
    }

    pub fn backend_event(
        &mut self,
        attachment: AttachmentId,
        generation: BackendGeneration,
        event: AttachEvent,
        now: Millis,
    ) {
        self.apply_event(attachment, generation, event, now);
        self.drop_overflowed(now);
    }

    fn apply_event(
        &mut self,
        attachment: AttachmentId,
        generation: BackendGeneration,
        event: AttachEvent,
        now: Millis,
    ) {
        let Some(held) = self.attachments.get(&attachment) else {
            self.command(BackendCommand::Detach { attachment });
            return;
        };
        let current = match held.state {
            State::Attached => None,
            State::Live {
                generation: current,
                ..
            } => Some(current),
            State::Opening | State::Ended(_) => return,
        };
        if let AttachEvent::Started {
            cols,
            rows,
            fidelity,
            ordered_resizes,
        } = event
        {
            if current.is_some_and(|current| generation <= current) {
                return;
            }
            let first = current.is_none();
            let Some(held) = self.attachments.get_mut(&attachment) else {
                return;
            };
            let result = held.log.start_epoch(
                generation,
                cols,
                rows,
                fidelity,
                ordered_resizes,
                self.capabilities.initial_credit,
            );
            match result {
                Appended::Yes => {
                    if let Some(epoch) = held.log.current_epoch() {
                        held.state = State::Live { generation, epoch };
                    }
                    if !first && self.is_owner(attachment) {
                        self.command(BackendCommand::Focus { attachment });
                    }
                    self.grant_credit(attachment);
                }
                Appended::Overflowed => self.ended(attachment, LogEnd::Gap, now),
                Appended::Ignored => {}
            }
            return;
        }
        if current != Some(generation) {
            return;
        }
        let ends = matches!(event, AttachEvent::Closed(_) | AttachEvent::Lost);
        let result = self.append(attachment, &event);
        self.settle(attachment, result, ends, now);
    }

    /// A stream that ends without a final event counts as the backend losing the terminal.
    pub fn backend_stream_ended(&mut self, attachment: AttachmentId, now: Millis) {
        self.end_stream(attachment, now);
        self.drop_overflowed(now);
    }

    fn end_stream(&mut self, attachment: AttachmentId, now: Millis) {
        let Some(held) = self.attachments.get_mut(&attachment) else {
            return;
        };
        match held.state {
            State::Live { generation, .. } => {
                self.backend_event(attachment, generation, AttachEvent::Lost, now)
            }
            State::Attached => {
                held.log.end_with_gap();
                self.ended(attachment, LogEnd::Gap, now);
            }
            State::Opening | State::Ended(_) => {}
        }
    }

    fn append(&mut self, id: AttachmentId, event: &AttachEvent) -> Appended {
        match self.attachments.get_mut(&id) {
            Some(attachment) => attachment.log.append(event),
            None => Appended::Ignored,
        }
    }

    /// Moves the attachment on after an append: a final event ends it, an overflow ends it with
    /// the gap.
    fn settle(&mut self, id: AttachmentId, result: Appended, final_event: bool, now: Millis) {
        match result {
            Appended::Yes if final_event => self.ended(id, LogEnd::Final, now),
            Appended::Overflowed => self.ended(id, LogEnd::Gap, now),
            Appended::Yes | Appended::Ignored => {}
        }
    }

    /// The attachment's stream is over. The backend object is released and ownership moves on.
    fn ended(&mut self, id: AttachmentId, end: LogEnd, now: Millis) {
        let Some(attachment) = self.attachments.get_mut(&id) else {
            return;
        };
        attachment.state = State::Ended(end);
        if end == LogEnd::Gap {
            attachment.gap_appended_at = Some(now);
        }
        let unbound = attachment.connection.is_none();
        self.release(id);
        self.refresh_eligibility(id);
        if end == LogEnd::Gap && unbound {
            self.finish(id);
        }
    }

    /// Asks the backend to detach, once per attachment.
    fn release(&mut self, id: AttachmentId) {
        let Some(attachment) = self.attachments.get_mut(&id) else {
            return;
        };
        if attachment.detach_sent || attachment.state == State::Opening {
            return;
        }
        attachment.detach_sent = true;
        self.command(BackendCommand::Detach { attachment: id });
    }

    /// Removes the attachment, releasing the backend if that has not happened.
    fn finish(&mut self, id: AttachmentId) {
        self.release(id);
        let Some(attachment) = self.attachments.remove(&id) else {
            return;
        };
        let key = attachment.terminal;
        let Some(ownership) = self.ownerships.get_mut(&key.0) else {
            return;
        };
        let next_id = &mut self.next_id;
        let change = ownership.leave(id, &mut || OwnershipGeneration(take(next_id)));
        if ownership.is_empty() {
            self.ownerships.remove(&key.0);
        }
        if let Some(change) = change {
            self.announce(&key, change);
        }
    }

    fn grant_credit(&mut self, id: AttachmentId) {
        if !self.capabilities.byte_credit {
            return;
        }
        let Some(attachment) = self.attachments.get_mut(&id) else {
            return;
        };
        if !matches!(attachment.state, State::Live { .. }) {
            return;
        }
        while let Some(bytes) = attachment.log.take_credit() {
            self.effects.push(Effect::Backend(BackendCommand::Credit {
                attachment: id,
                bytes,
            }));
        }
    }

    // ---- ownership ----

    /// A member may own while it is bound and its pair is up.
    fn refresh_eligibility(&mut self, id: AttachmentId) {
        let Some(attachment) = self.attachments.get(&id) else {
            return;
        };
        let eligible = attachment.connection.is_some() && attachment.joined();
        let key = attachment.terminal.clone();
        let Some(ownership) = self.ownerships.get_mut(&key.0) else {
            return;
        };
        let next_id = &mut self.next_id;
        let change =
            ownership.set_eligible(id, eligible, &mut || OwnershipGeneration(take(next_id)));
        if let Some(change) = change {
            self.announce(&key, change);
        }
    }

    /// Makes `id` the owner. Any input that triggered the grant is written last.
    fn grant(&mut self, id: AttachmentId, write: Option<Vec<u8>>) {
        let Some(attachment) = self.attachments.get(&id) else {
            return;
        };
        let key = attachment.terminal.clone();
        let Some(ownership) = self.ownerships.get_mut(&key.0) else {
            return;
        };
        let generation = OwnershipGeneration(take(&mut self.next_id));
        let change = ownership.grant(id, generation);
        self.announce(&key, change);
        if let Some(bytes) = write {
            if self.is_owner(id) {
                self.command(BackendCommand::Write {
                    attachment: id,
                    bytes,
                });
            }
        }
    }

    /// Tells every bound member of the terminal about an ownership change, and sets a new
    /// owner up with the size and display it last asked for.
    fn announce(&mut self, terminal: &TermId, change: OwnershipChange) {
        let members: Vec<AttachmentId> = self
            .attachments
            .iter()
            .filter(|(_, a)| a.terminal == *terminal && a.connection.is_some() && a.joined())
            .map(|(id, _)| *id)
            .collect();
        for member in members {
            let Some(connection) = self.attachments.get(&member).and_then(|a| a.connection) else {
                continue;
            };
            self.push_control(
                connection,
                DaemonMessage::Ownership(Ownership {
                    attachment: member,
                    ownership_generation: change.state.generation,
                    owner: change.state.owner,
                }),
            );
        }
        let Some(owner) = change.new_owner else {
            return;
        };
        if !self.is_owner(owner) {
            return;
        }
        let Some(attachment) = self.attachments.get(&owner) else {
            return;
        };
        let (cols, rows, display) = (attachment.cols, attachment.rows, attachment.display.clone());
        if self.capabilities.display_queries {
            self.command(BackendCommand::SetDisplay {
                attachment: owner,
                display,
            });
        }
        self.command(BackendCommand::Resize {
            attachment: owner,
            cols,
            rows,
        });
        self.command(BackendCommand::Focus { attachment: owner });
    }
}
