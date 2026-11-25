#![cfg(feature = "test-support")]
//! A caller identity for tests.

use std::sync::Mutex;

use combine_protocol::TermId;

use super::world::{is_remote, ID_PREFIX};
use crate::ports::CallerIdentity;

/// Accepts the terminals of one socket, which a test can change.
pub struct FakeCallerIdentity {
    socket: Mutex<Option<String>>,
}

impl FakeCallerIdentity {
    pub fn new(socket: Option<&str>) -> FakeCallerIdentity {
        FakeCallerIdentity {
            socket: Mutex::new(socket.map(str::to_owned)),
        }
    }

    pub fn set_socket(&self, socket: Option<&str>) {
        if let Ok(mut held) = self.socket.lock() {
            *held = socket.map(str::to_owned);
        }
    }
}

impl CallerIdentity for FakeCallerIdentity {
    fn terminal(&self, terminal_id: &str, terminal_socket: &str) -> Option<TermId> {
        let held = self.socket.lock().ok()?;
        if held.as_deref() != Some(terminal_socket) {
            return None;
        }
        if terminal_id.is_empty() || is_remote(terminal_id) {
            return None;
        }
        Some(TermId(format!("{ID_PREFIX}:{terminal_id}")))
    }
}
