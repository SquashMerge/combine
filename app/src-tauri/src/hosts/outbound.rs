//! The client's outbound channel: one slot per connected host, through which anything in the
//! app sends a message to that host's daemon while the session's reader is blocked.
//!
//! A session registers its writer's channel and gets a [`Registration`]. Dropping it removes
//! the slot, but only when no newer session has replaced it.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use combine_protocol::{ClientMessage, HostId, RequestId, WireMessage};

/// How many request kinds a slot remembers. The oldest is evicted first.
const MAX_KINDS: usize = 1024;

/// What a request was for, so its reply can be routed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Terminals,
    Layout,
    Spawn,
    Attach,
    Resume,
    Panel,
}

/// What the writer thread receives.
#[derive(Debug, PartialEq, Eq)]
pub enum Outgoing {
    Message(ClientMessage),
    Stop,
}

/// Why a message wasn't queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    NotConnected,
    /// It can't be encoded as one frame. It is refused here, where its sender hears of it,
    /// rather than stopping the writer with its request unanswered.
    Unencodable(String),
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::NotConnected => f.write_str("not connected"),
            SendError::Unencodable(reason) => write!(f, "cannot send the message: {reason}"),
        }
    }
}

impl std::error::Error for SendError {}

/// Refuses a message the writer couldn't encode.
fn encodable(message: &ClientMessage) -> Result<(), SendError> {
    message
        .encode()
        .map(drop)
        .map_err(|error| SendError::Unencodable(error.to_string()))
}

struct SessionSlot {
    generation: u64,
    tx: Sender<Outgoing>,
    next_request: u64,
    kinds: BTreeMap<u64, RequestKind>,
}

#[derive(Default)]
struct State {
    next_generation: u64,
    slots: BTreeMap<String, SessionSlot>,
}

#[derive(Default)]
pub struct Outbound {
    state: Mutex<State>,
}

impl Outbound {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Makes `tx` the host's channel, replacing any older session's.
    pub fn register(self: &Arc<Self>, host_id: &HostId, tx: Sender<Outgoing>) -> Registration {
        let mut state = self.state();
        state.next_generation += 1;
        let generation = state.next_generation;
        state.slots.insert(
            host_id.0.clone(),
            SessionSlot {
                generation,
                tx,
                next_request: 1,
                kinds: BTreeMap::new(),
            },
        );
        Registration {
            outbound: Arc::clone(self),
            host_id: host_id.clone(),
            generation,
        }
    }

    /// Sends the message `build` makes for a fresh request ID, and remembers its `kind`.
    pub fn send(
        &self,
        host_id: &HostId,
        kind: RequestKind,
        build: impl FnOnce(RequestId) -> ClientMessage,
    ) -> Result<RequestId, SendError> {
        let mut state = self.state();
        let slot = state
            .slots
            .get_mut(&host_id.0)
            .ok_or(SendError::NotConnected)?;
        let request = RequestId(slot.next_request);
        let message = build(request);
        encodable(&message)?;
        slot.tx
            .send(Outgoing::Message(message))
            .map_err(|_| SendError::NotConnected)?;
        slot.next_request += 1;
        slot.kinds.insert(request.0, kind);
        while slot.kinds.len() > MAX_KINDS {
            slot.kinds.pop_first();
        }
        Ok(request)
    }

    /// Sends a message whose reply nobody routes.
    pub fn send_untracked(
        &self,
        host_id: &HostId,
        message: ClientMessage,
    ) -> Result<(), SendError> {
        encodable(&message)?;
        let state = self.state();
        let slot = state.slots.get(&host_id.0).ok_or(SendError::NotConnected)?;
        slot.tx
            .send(Outgoing::Message(message))
            .map_err(|_| SendError::NotConnected)
    }

    pub fn kind_of(&self, host_id: &HostId, request: RequestId) -> Option<RequestKind> {
        self.state()
            .slots
            .get(&host_id.0)
            .and_then(|slot| slot.kinds.get(&request.0).copied())
    }
}

/// Proof that a session is registered. Dropping it unregisters the session, unless a newer one
/// has taken the host's slot.
pub struct Registration {
    outbound: Arc<Outbound>,
    host_id: HostId,
    generation: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut state = self.outbound.state();
        let current = state
            .slots
            .get(&self.host_id.0)
            .is_some_and(|slot| slot.generation == self.generation);
        if current {
            state.slots.remove(&self.host_id.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use combine_protocol::{WatchLayout, WatchTerminals, MAX_FRAME_LEN};

    use super::super::testing::{host_id, HOST_A};
    use super::*;

    fn watch(request: RequestId) -> ClientMessage {
        ClientMessage::WatchTerminals(WatchTerminals { request })
    }

    #[test]
    fn kinds_are_capped_and_the_oldest_is_evicted() {
        let outbound = Arc::new(Outbound::default());
        let host = host_id(HOST_A);
        let (tx, _rx) = mpsc::channel();
        let _registration = outbound.register(&host, tx);
        let mut first = None;
        let mut last = None;
        for _ in 0..=MAX_KINDS {
            let request = outbound
                .send(&host, RequestKind::Terminals, watch)
                .expect("registered");
            first.get_or_insert(request);
            last = Some(request);
        }
        assert_eq!(outbound.kind_of(&host, first.expect("sent")), None);
        assert_eq!(
            outbound.kind_of(&host, last.expect("sent")),
            Some(RequestKind::Terminals)
        );
    }

    #[test]
    fn an_unencodable_message_is_refused_and_the_slot_keeps_working() {
        let outbound = Arc::new(Outbound::default());
        let host = host_id(HOST_A);
        let (tx, rx) = mpsc::channel();
        let _registration = outbound.register(&host, tx);
        let space = "x".repeat(MAX_FRAME_LEN as usize);
        let refused = outbound.send(&host, RequestKind::Layout, |request| {
            ClientMessage::WatchLayout(WatchLayout {
                request,
                space: space.clone(),
            })
        });
        assert!(
            matches!(refused, Err(SendError::Unencodable(_))),
            "{refused:?}"
        );
        let untracked = outbound.send_untracked(
            &host,
            ClientMessage::WatchLayout(WatchLayout {
                request: RequestId(9),
                space,
            }),
        );
        assert!(matches!(untracked, Err(SendError::Unencodable(_))));
        assert!(rx.try_recv().is_err(), "nothing reached the writer");
        let sent = outbound
            .send(&host, RequestKind::Terminals, watch)
            .expect("the slot still works");
        assert_eq!(sent, RequestId(1));
        assert_eq!(rx.try_recv(), Ok(Outgoing::Message(watch(sent))));
    }
}
