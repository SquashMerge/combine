//! A space's layout on the wire: the tile tree, the ops that change it and the
//! snapshot the daemon sends.
//!
//! The tree is the daemon's `layout.json` without its `groups` and unknown
//! keys, which the daemon keeps to itself. IDs are opaque strings the daemon
//! validates, not the codec.

use serde::{Deserialize, Serialize};

/// `h` puts `a` left of `b`; `v` puts `a` above `b`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    H,
    V,
}

/// A split ratio. The wire format has no NaN, so the derived equality is total
/// for every value that can cross it, which is what lets messages stay `Eq`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Ratio(pub f64);

impl Eq for Ratio {}

/// One node of the tile tree, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutNode {
    /// Two children. `ratio` is `a`'s share.
    Split {
        dir: SplitDirection,
        ratio: Ratio,
        a: Box<LayoutNode>,
        b: Box<LayoutNode>,
    },
    /// A leaf holding panels, showing `active`.
    Tile {
        id: String,
        panels: Vec<String>,
        active: Option<String>,
    },
}

/// A batch of ops the daemon threw away because an outside edit conflicted
/// with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DroppedNotice {
    pub op_count: u32,
    pub message: String,
}

/// What a client is told about a space's layout.
///
/// `layout_revision` counts accepted changes; a `mutate_layout` carries the
/// revision it was made against. `error` is set while `layout.json` can't be
/// used, and `root` is then the last good layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutSnapshot {
    pub space: String,
    pub layout_revision: u64,
    pub root: LayoutNode,
    pub read_only: bool,
    pub error: Option<String>,
    pub dropped: Option<DroppedNotice>,
}

/// One change to a layout, tagged by `op`. A split is addressed by the set of
/// tile IDs under it, in any order. `new_tile` is chosen by the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LayoutOp {
    SetActive {
        tile: String,
        panel: String,
    },
    SetRatio {
        leaves: Vec<String>,
        ratio: Ratio,
    },
    MovePanel {
        panel: String,
        to_tile: String,
        index: u32,
    },
    SplitTile {
        tile: String,
        dir: SplitDirection,
        new_tile: String,
        ratio: Ratio,
        panel: Option<String>,
    },
    CloseTile {
        tile: String,
    },
}
