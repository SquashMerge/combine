//! Spaces: terminals grouped by the git root, worktree or folder they started in.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use combine_protocol::TermId;

use crate::model::{RootKind, Space, SpaceId, TermEvent, TermInfo};
use crate::ports::{GitEntry, GitProbe};

struct Placement {
    terminal: TermId,
    space: SpaceId,
    kind: RootKind,
}

/// Assigns each terminal to a space the first time it is seen, and never moves it.
pub struct Spaces {
    probe: Box<dyn GitProbe>,
    placements: Vec<Placement>,
}

impl Spaces {
    pub fn new(probe: Box<dyn GitProbe>) -> Spaces {
        Spaces {
            probe,
            placements: Vec::new(),
        }
    }

    /// Rebuilds the placements in `list`'s order. A terminal seen before keeps its space, a new
    /// one is located, and the first of any duplicate IDs wins. Terminals missing from `list`
    /// are dropped.
    pub fn apply_list(&mut self, list: &[TermInfo]) {
        let mut previous = std::mem::take(&mut self.placements);
        for info in list {
            if self.contains_terminal(&info.id) {
                continue;
            }
            match previous
                .iter()
                .position(|placement| placement.terminal == info.id)
            {
                Some(index) => self.placements.push(previous.swap_remove(index)),
                None => self.place(info),
            }
        }
    }

    /// `Created` and `Changed` assign a terminal not seen before and never move one that was.
    /// `Lost` removes it. `Exited` and `Restarted` leave its space alone.
    pub fn apply_event(&mut self, event: &TermEvent) {
        match event {
            TermEvent::Created(info) | TermEvent::Changed(info) => self.assign(info),
            TermEvent::Lost { id } => {
                self.placements
                    .retain(|placement| placement.terminal != *id);
            }
            TermEvent::Exited { .. } | TermEvent::Restarted { .. } => {}
        }
    }

    pub fn space_of(&self, id: &TermId) -> Option<&SpaceId> {
        self.placements
            .iter()
            .find(|placement| placement.terminal == *id)
            .map(|placement| &placement.space)
    }

    /// Whether any terminal is placed in `space`.
    pub fn contains(&self, space: &SpaceId) -> bool {
        self.placements
            .iter()
            .any(|placement| placement.space == *space)
    }

    /// The spaces that hold terminals, sorted by ID. Each space's terminals keep list order.
    pub fn spaces(&self) -> Vec<Space> {
        let mut grouped: BTreeMap<&SpaceId, Space> = BTreeMap::new();
        for placement in self.placements.iter() {
            grouped
                .entry(&placement.space)
                .or_insert_with(|| Space {
                    id: placement.space.clone(),
                    kind: placement.kind,
                    terminals: Vec::new(),
                })
                .terminals
                .push(placement.terminal.clone());
        }
        grouped.into_values().collect()
    }

    fn assign(&mut self, info: &TermInfo) {
        if !self.contains_terminal(&info.id) {
            self.place(info);
        }
    }

    fn contains_terminal(&self, id: &TermId) -> bool {
        self.placements
            .iter()
            .any(|placement| placement.terminal == *id)
    }

    fn place(&mut self, info: &TermInfo) {
        let (space, kind) = self.locate(&info.cwd);
        self.placements.push(Placement {
            terminal: info.id.clone(),
            space,
            kind,
        });
    }

    /// Walks the directory's lexical ancestors, nearest first, for a `.git` entry.
    fn locate(&self, cwd: &str) -> (SpaceId, RootKind) {
        let path: PathBuf = Path::new(cwd).components().collect();
        if !path.is_absolute() {
            return (SpaceId(cwd.to_string()), RootKind::Folder);
        }
        for dir in path.ancestors() {
            let Some(name) = dir.to_str() else { continue };
            match self.probe.git_entry(name) {
                GitEntry::Directory => return (SpaceId(name.to_string()), RootKind::Repository),
                GitEntry::File => return (SpaceId(name.to_string()), RootKind::Worktree),
                GitEntry::Absent => {}
            }
        }
        (
            SpaceId(path.to_string_lossy().into_owned()),
            RootKind::Folder,
        )
    }
}
