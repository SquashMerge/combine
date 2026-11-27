//! The host's terminals and their spaces, as `terminals` snapshots report them.

use std::path::{Component, Path, PathBuf};

use combine_protocol::{
    BackendInfo, RequestId, SpaceInfo, SpaceKind, TermId, TerminalInfo, TerminalState, Terminals,
};

use super::spaces::Spaces;
use crate::model::{RootKind, Space, SpaceId, TermEvent, TermInfo, TermState};
use crate::ports::GitProbe;

/// What the backend last told the daemon about its terminals, and whether that
/// changed since [`take_changed`](Self::take_changed) last said so.
pub struct TerminalList {
    spaces: Spaces,
    terminals: Vec<TermInfo>,
    backend: Option<BackendInfo>,
    changed: bool,
}

impl TerminalList {
    pub fn new(probe: Box<dyn GitProbe>) -> TerminalList {
        TerminalList {
            spaces: Spaces::new(probe),
            terminals: Vec::new(),
            backend: None,
            changed: false,
        }
    }

    /// Names the backend the snapshots report. It doesn't change what a watcher is owed.
    pub fn set_backend(&mut self, backend: BackendInfo) {
        self.backend = Some(backend);
    }

    /// Replaces the table with the backend's list, in its order. The first of any duplicate IDs wins.
    pub fn apply_list(&mut self, list: &[TermInfo]) {
        self.spaces.apply_list(list);
        self.terminals.clear();
        for info in list {
            if !self.terminals.iter().any(|known| known.id == info.id) {
                self.terminals.push(info.clone());
            }
        }
        self.changed = true;
    }

    /// `Created` and `Changed` replace a known terminal in place and append an unknown one,
    /// `Exited` keeps it with its exit code and `Lost` removes it. `Restarted` changes nothing.
    pub fn apply_event(&mut self, event: &TermEvent) {
        self.spaces.apply_event(event);
        match event {
            TermEvent::Created(info) | TermEvent::Changed(info) => {
                match self.terminals.iter_mut().find(|known| known.id == info.id) {
                    Some(known) => *known = info.clone(),
                    None => self.terminals.push(info.clone()),
                }
            }
            TermEvent::Exited { id, exit_code } => {
                if let Some(info) = self.terminals.iter_mut().find(|known| known.id == *id) {
                    info.state = TermState::Exited {
                        exit_code: *exit_code,
                    };
                }
            }
            TermEvent::Lost { id } => {
                self.terminals.retain(|known| known.id != *id);
            }
            TermEvent::Restarted { .. } => return,
        }
        self.changed = true;
    }

    /// Whether anything changed since the last call.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// Whether any listed terminal is in `space`.
    pub fn space_listed(&self, space: &SpaceId) -> bool {
        self.spaces.contains(space)
    }

    /// The IDs of the listed spaces, sorted.
    pub fn space_ids(&self) -> Vec<SpaceId> {
        self.spaces
            .spaces()
            .into_iter()
            .map(|space| space.id)
            .collect()
    }

    /// The space a listed terminal was placed in.
    pub fn space_of_terminal(&self, terminal: &TermId) -> Option<SpaceId> {
        self.spaces.space_of(terminal).cloned()
    }

    /// The nearest listed space that is `cwd` or an ancestor of it, compared by path components.
    /// `None` for a relative `cwd`, one with a `..` component, or one under no listed space.
    pub fn space_for_cwd(&self, cwd: &str) -> Option<SpaceId> {
        let path: PathBuf = Path::new(cwd).components().collect();
        if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
            return None;
        }
        self.space_ids()
            .into_iter()
            .filter(|space| {
                let root = Path::new(&space.0);
                root.is_absolute() && path.starts_with(root)
            })
            .max_by_key(|space| Path::new(&space.0).components().count())
    }

    /// A full snapshot: terminals in the backend's list order, and the spaces that hold them
    /// sorted by ID.
    pub fn snapshot(&self, request: Option<RequestId>) -> Terminals {
        Terminals {
            request,
            terminals: self.terminals.iter().map(terminal_info).collect(),
            spaces: self.spaces.spaces().into_iter().map(space_info).collect(),
            backend: self.backend.clone(),
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
