//! The host's terminals and their spaces, as `terminals` snapshots report them.

use std::collections::BTreeMap;

use combine_protocol::{RequestId, SpaceInfo, SpaceKind, TerminalInfo, TerminalState, Terminals};

use super::spaces::Spaces;
use crate::model::{RootKind, Space, TermEvent, TermInfo, TermState};
use crate::ports::GitProbe;

/// What the backend last told the daemon about its terminals, and whether that
/// changed since [`take_changed`](Self::take_changed) last said so.
pub struct TerminalList {
    spaces: Spaces,
    terminals: BTreeMap<String, TermInfo>,
    changed: bool,
}

impl TerminalList {
    pub fn new(probe: Box<dyn GitProbe>) -> TerminalList {
        TerminalList {
            spaces: Spaces::new(probe),
            terminals: BTreeMap::new(),
            changed: false,
        }
    }

    /// Replaces the table with the backend's list.
    pub fn apply_list(&mut self, list: &[TermInfo]) {
        self.spaces.apply_list(list);
        self.terminals = list
            .iter()
            .map(|info| (info.id.0.clone(), info.clone()))
            .collect();
        self.changed = true;
    }

    /// `Created` and `Changed` insert or replace a terminal, `Exited` keeps it
    /// with its exit code and `Lost` removes it. `Restarted` changes nothing.
    pub fn apply_event(&mut self, event: &TermEvent) {
        self.spaces.apply_event(event);
        match event {
            TermEvent::Created(info) | TermEvent::Changed(info) => {
                self.terminals.insert(info.id.0.clone(), info.clone());
            }
            TermEvent::Exited { id, exit_code } => {
                if let Some(info) = self.terminals.get_mut(&id.0) {
                    info.state = TermState::Exited {
                        exit_code: *exit_code,
                    };
                }
            }
            TermEvent::Lost { id } => {
                self.terminals.remove(&id.0);
            }
            TermEvent::Restarted { .. } => return,
        }
        self.changed = true;
    }

    /// Whether anything changed since the last call.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// A full snapshot: terminals sorted by ID, and the spaces that hold them.
    pub fn snapshot(&self, request: Option<RequestId>) -> Terminals {
        Terminals {
            request,
            terminals: self.terminals.values().map(terminal_info).collect(),
            spaces: self.spaces.spaces().into_iter().map(space_info).collect(),
        }
    }
}

fn terminal_info(info: &TermInfo) -> TerminalInfo {
    let (state, exit_code) = match info.state {
        TermState::Running => (TerminalState::Running, None),
        TermState::Exited { exit_code } => (TerminalState::Exited, exit_code),
    };
    TerminalInfo {
        id: info.id.clone(),
        cwd: info.cwd.clone(),
        argv: info.argv.clone(),
        state,
        exit_code,
        labels: info.labels.clone(),
        cols: info.cols,
        rows: info.rows,
    }
}

fn space_info(space: Space) -> SpaceInfo {
    SpaceInfo {
        id: space.id.0,
        kind: match space.kind {
            RootKind::Repository => SpaceKind::Repository,
            RootKind::Worktree => SpaceKind::Worktree,
            RootKind::Folder => SpaceKind::Folder,
        },
        terminals: space.terminals,
    }
}
