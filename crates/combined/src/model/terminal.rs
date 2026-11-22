//! A terminal as a backend describes it.

use std::collections::BTreeMap;

use combine_protocol::TermId;

/// Whether a terminal's program is still running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermState {
    Running,
    Exited { exit_code: Option<i32> },
}

/// One terminal on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermInfo {
    pub id: TermId,
    /// The current directory, as the backend reports it.
    pub cwd: String,
    pub argv: Vec<String>,
    pub state: TermState,
    pub labels: BTreeMap<String, String>,
    pub cols: u16,
    pub rows: u16,
}

/// What to start. The spec's "cmd" is `argv`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    pub argv: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub labels: BTreeMap<String, String>,
    pub cols: u16,
    pub rows: u16,
}

/// A change after `watch()` was called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermEvent {
    Created(TermInfo),
    Changed(TermInfo),
    Exited { id: TermId, exit_code: Option<i32> },
    Lost { id: TermId },
    Restarted { id: TermId },
}
