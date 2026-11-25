//! Who opened an MCP channel, as far as the terminal backend can say. Attribution, not
//! authentication (SPEC.md, Security model).

use combine_protocol::TermId;

/// Turns the terminal an agent's environment names into this daemon's terminal ID.
pub trait CallerIdentity: Send + Sync {
    /// `terminal_id` as this daemon's `TermId` when `terminal_socket` names the socket of the
    /// backend the daemon runs on (compared as paths, without touching the file system) and
    /// `terminal_id` is a local address; else `None`.
    fn terminal(&self, terminal_id: &str, terminal_socket: &str) -> Option<TermId>;
}
