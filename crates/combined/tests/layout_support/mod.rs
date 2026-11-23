//! Test support for the layout sync: an in-test byte map standing in for the file store, and a
//! loop that runs the sync's effects against it.

#![allow(dead_code)]

use std::collections::{BTreeMap, VecDeque};

use combined::core::{
    encode_layout, ConnectionId, LayoutEffect, LayoutOp, LayoutRead, LayoutTiming, LayoutWritten,
    Layouts, MutateError,
};
use combined::model::{
    ContentHash, LayoutRevision, Millis, PanelId, SpaceId, SpaceLayout, Split, SplitDir, SplitNode,
    Tile, TileId,
};
use serde_json::{json, Map};
use sha2::{Digest, Sha256};

pub fn hash(bytes: &[u8]) -> ContentHash {
    ContentHash(Sha256::digest(bytes).into())
}

pub fn t(id: &str) -> TileId {
    TileId::new(id).unwrap()
}

pub fn p(id: &str) -> PanelId {
    PanelId::new(id).unwrap()
}

pub fn space(name: &str) -> SpaceId {
    SpaceId(name.to_string())
}

pub fn tile(id: &str, panels: &[&str], active: Option<&str>) -> SplitNode {
    SplitNode::Tile(Tile {
        id: t(id),
        panels: panels.iter().map(|panel| p(panel)).collect(),
        active: active.map(p),
        extra: Map::new(),
    })
}

pub fn split(ratio: f64, a: SplitNode, b: SplitNode) -> SplitNode {
    SplitNode::Split(Split {
        dir: SplitDir::H,
        ratio,
        a: Box::new(a),
        b: Box::new(b),
        extra: Map::new(),
    })
}

pub fn layout(root: SplitNode) -> SpaceLayout {
    SpaceLayout {
        version: 1,
        root,
        groups: json!([]),
        extra: Map::new(),
    }
}

pub fn bytes_of(layout: &SpaceLayout) -> Vec<u8> {
    encode_layout(layout).unwrap()
}

pub fn set_active(tile: &str, panel: &str) -> LayoutOp {
    LayoutOp::SetActive {
        tile: t(tile),
        panel: p(panel),
    }
}

pub fn set_ratio(leaves: &[&str], ratio: f64) -> LayoutOp {
    LayoutOp::SetRatio {
        leaves: leaves.iter().map(|id| t(id)).collect(),
        ratio,
    }
}

pub fn close(tile: &str) -> LayoutOp {
    LayoutOp::CloseTile { tile: t(tile) }
}

pub const MAIN: ConnectionId = ConnectionId(1);

/// A sync wired to a byte map. Each write is recorded in `writes`.
pub struct Sim {
    pub layouts: Layouts,
    pub disk: BTreeMap<SpaceId, Vec<u8>>,
    pub writes: Vec<(SpaceId, Vec<u8>, usize)>,
    pub reads: usize,
    /// Outcomes to give the next writes instead of storing them.
    pub write_outcomes: VecDeque<LayoutWritten>,
    /// While set, every read fails with this detail, whatever `disk` holds.
    pub failing_reads: Option<String>,
}

impl Sim {
    pub fn new(timing: LayoutTiming) -> Sim {
        Sim {
            layouts: Layouts::new(timing),
            disk: BTreeMap::new(),
            writes: Vec::new(),
            reads: 0,
            write_outcomes: VecDeque::new(),
            failing_reads: None,
        }
    }

    /// A default-timing sync over `layout` stored for `main`, watched by [`MAIN`] and read.
    pub fn open(layout: &SpaceLayout) -> Sim {
        let mut sim = Sim::new(LayoutTiming::default());
        sim.disk.insert(space("main"), bytes_of(layout));
        sim.watch(MAIN, "main", Millis(0));
        sim
    }

    pub fn watch(&mut self, conn: ConnectionId, name: &str, now: Millis) {
        self.layouts.watch(conn, space(name), 1, true, now).unwrap();
        self.settle(now);
        self.layouts.take_replies();
        self.layouts.take_changed();
    }

    /// Runs effects until none are left.
    pub fn settle(&mut self, now: Millis) {
        loop {
            let effects = self.layouts.take_effects();
            if effects.is_empty() {
                return;
            }
            for effect in effects {
                match effect {
                    LayoutEffect::Watch { .. } => {}
                    LayoutEffect::Read { space } => {
                        self.reads += 1;
                        let read = match (&self.failing_reads, self.disk.get(&space)) {
                            (Some(detail), _) => LayoutRead::Failed(detail.clone()),
                            (None, None) => LayoutRead::Missing,
                            (None, Some(bytes)) => LayoutRead::Found {
                                hash: hash(bytes),
                                bytes: bytes.clone(),
                                read_only: false,
                            },
                        };
                        self.layouts.apply_read(&space, read, now);
                    }
                    LayoutEffect::Write {
                        space,
                        bytes,
                        op_count,
                    } => {
                        self.writes.push((space.clone(), bytes.clone(), op_count));
                        let outcome = self.write_outcomes.pop_front().unwrap_or_else(|| {
                            let outcome = LayoutWritten::Ok {
                                hash: hash(&bytes),
                                read_only: false,
                            };
                            self.disk.insert(space.clone(), bytes);
                            outcome
                        });
                        self.layouts.apply_write(&space, outcome, now);
                    }
                }
            }
        }
    }

    pub fn revision(&self, name: &str) -> LayoutRevision {
        self.layouts.status(&space(name)).unwrap().layout_revision
    }

    pub fn current(&self, name: &str) -> SpaceLayout {
        self.layouts.status(&space(name)).unwrap().layout
    }

    /// [`MAIN`] mutates `main` at the current revision.
    pub fn mutate(&mut self, ops: Vec<LayoutOp>, now: Millis) -> Result<(), MutateError> {
        let base = self.revision("main");
        self.layouts.mutate(MAIN, &space("main"), 7, base, ops, now)
    }

    /// Stores `bytes` as an outside edit and runs the change through the sync.
    pub fn outside_write(&mut self, bytes: Vec<u8>, now: Millis) {
        self.disk.insert(space("main"), bytes);
        self.layouts.apply_change(&space("main"));
        self.settle(now);
    }

    pub fn written(&self) -> usize {
        self.writes.len()
    }
}
