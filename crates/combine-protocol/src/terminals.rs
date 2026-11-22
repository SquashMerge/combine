//! The terminal list on the wire: terminals and the spaces that group them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::TermId;

/// Whether a terminal's process is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalState {
    Running,
    Exited,
}

/// One terminal of the host. `exit_code` is `null` while it runs, and also
/// when it exited without a code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalInfo {
    pub id: TermId,
    pub cwd: String,
    pub argv: Vec<String>,
    pub state: TerminalState,
    pub exit_code: Option<i32>,
    pub labels: BTreeMap<String, String>,
    pub cols: u16,
    pub rows: u16,
}

/// What a space's folder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceKind {
    Repository,
    Worktree,
    Folder,
}

/// A group of terminals. `id` is the space's folder path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceInfo {
    pub id: String,
    pub kind: SpaceKind,
    pub terminals: Vec<TermId>,
}
