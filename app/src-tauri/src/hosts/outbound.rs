//! The client's outbound channel: one slot per connected host, through which anything in the
//! app sends a message to that host's daemon while the session's reader is blocked.
//!
//! A session registers its writer's channel and gets a [`Registration`]. Dropping it removes
//! the slot, but only when no newer session has replaced it.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use combine_protocol::{ClientMessage, HostId, RequestId};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotConnected;

impl fmt::Display for NotConnected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not connected")
    }
}

impl std::error::Error for NotConnected {}

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
    ) -> Result<RequestId, NotConnected> {
        let mut state = self.state();
        let slot = state.slots.get_mut(&host_id.0).ok_or(NotConnected)?;
        let request = RequestId(slot.next_request);
        slot.tx
            .send(Outgoing::Message(build(request)))
            .map_err(|_| NotConnected)?;
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
    ) -> Result<(), NotConnected> {
        let state = self.state();
        let slot = state.slots.get(&host_id.0).ok_or(NotConnected)?;
        slot.tx
            .send(Outgoing::Message(message))
            .map_err(|_| NotConnected)
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

    use combine_protocol::WatchTerminals;

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
}
