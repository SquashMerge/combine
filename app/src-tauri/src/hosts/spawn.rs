//! Where a host's spawn traffic goes: template lists, started terminals and refusals.

use combine_protocol::{ErrorCode, HostId, RequestId, Spawned, Templates};

/// One thing a host's daemon said about spawning, or the end of the session that could say it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnReply {
    Templates(Templates),
    Spawned(Spawned),
    /// The daemon refused a spawn request. `message` is trimmed external text.
    Error {
        request: RequestId,
        code: ErrorCode,
        message: String,
    },
    /// The session ended, so replies to requests still in flight will never come.
    Disconnected,
}

/// Where a host's spawn replies go. It mirrors the spawn method of
/// [`HostEvents`](super::supervisor::HostEvents).
pub trait SpawnSink: Send + Sync {
    fn spawn_reply(&self, host_id: &HostId, reply: SpawnReply);
}
