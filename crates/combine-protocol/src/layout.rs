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

/// A split ratio: a finite number. The range (0, 1) is the daemon's check, so
/// an out-of-range ratio is an `invalid_request`, not a malformed frame. Being
/// finite is what makes the equality total, so messages can stay `Eq`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Ratio(f64);

impl Ratio {
    /// `None` for NaN and infinities.
    pub fn new(value: f64) -> Option<Ratio> {
        value.is_finite().then_some(Ratio(value))
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

impl Eq for Ratio {}

impl TryFrom<f64> for Ratio {
    type Error = &'static str;

    fn try_from(value: f64) -> Result<Ratio, Self::Error> {
        Ratio::new(value).ok_or("a ratio must be finite")
    }
}

impl From<Ratio> for f64 {
    fn from(ratio: Ratio) -> f64 {
        ratio.0
    }
}

/// One node of the tile tree, tagged by `type`.
///
/// It decodes through a flat struct rather than serde's internally tagged
/// form, which would buffer every unknown key and each nested node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", try_from = "RawNode")]
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

#[derive(Deserialize)]
struct RawNode {
    #[serde(rename = "type")]
    kind: String,
    dir: Option<SplitDirection>,
    ratio: Option<Ratio>,
    a: Option<Box<LayoutNode>>,
    b: Option<Box<LayoutNode>>,
    id: Option<String>,
    panels: Option<Vec<String>>,
    active: Option<String>,
}

fn missing(what: &str, field: &str) -> String {
    format!("{what} is missing `{field}`")
}

impl TryFrom<RawNode> for LayoutNode {
    type Error = String;

    fn try_from(raw: RawNode) -> Result<LayoutNode, String> {
        match raw.kind.as_str() {
            "split" => Ok(LayoutNode::Split {
                dir: raw.dir.ok_or_else(|| missing("a split", "dir"))?,
                ratio: raw.ratio.ok_or_else(|| missing("a split", "ratio"))?,
                a: raw.a.ok_or_else(|| missing("a split", "a"))?,
                b: raw.b.ok_or_else(|| missing("a split", "b"))?,
            }),
            "tile" => Ok(LayoutNode::Tile {
                id: raw.id.ok_or_else(|| missing("a tile", "id"))?,
                panels: raw.panels.ok_or_else(|| missing("a tile", "panels"))?,
                active: raw.active,
            }),
            other => Err(format!("unknown node type `{}`", clip(other))),
        }
    }
}

fn clip(text: &str) -> &str {
    &text[..crate::codec::floor_char_boundary(text, text.len().min(64))]
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
///
/// `add_panel` and `remove_panel` put a panel ID into the tree and take one
/// out; neither creates or closes a panel.
///
/// It decodes through a flat struct, like [`LayoutNode`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", try_from = "RawOp")]
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
    /// Puts a panel ID that isn't in the tree into it. `tile` is a hint naming
    /// the tile to add it to.
    AddPanel {
        panel: String,
        tile: Option<String>,
    },
    /// Takes a panel ID out of the tree. The panel itself stays.
    RemovePanel {
        panel: String,
    },
}

#[derive(Deserialize)]
struct RawOp {
    op: String,
    tile: Option<String>,
    panel: Option<String>,
    leaves: Option<Vec<String>>,
    ratio: Option<Ratio>,
    to_tile: Option<String>,
    index: Option<u32>,
    dir: Option<SplitDirection>,
    new_tile: Option<String>,
}

impl TryFrom<RawOp> for LayoutOp {
    type Error = String;

    fn try_from(raw: RawOp) -> Result<LayoutOp, String> {
        let name = raw.op.as_str();
        let need = |field: &str| missing(&format!("`{name}`"), field);
        match name {
            "set_active" => Ok(LayoutOp::SetActive {
                tile: raw.tile.ok_or_else(|| need("tile"))?,
                panel: raw.panel.ok_or_else(|| need("panel"))?,
            }),
            "set_ratio" => Ok(LayoutOp::SetRatio {
                leaves: raw.leaves.ok_or_else(|| need("leaves"))?,
                ratio: raw.ratio.ok_or_else(|| need("ratio"))?,
            }),
            "move_panel" => Ok(LayoutOp::MovePanel {
                panel: raw.panel.ok_or_else(|| need("panel"))?,
                to_tile: raw.to_tile.ok_or_else(|| need("to_tile"))?,
                index: raw.index.ok_or_else(|| need("index"))?,
            }),
            "split_tile" => Ok(LayoutOp::SplitTile {
                tile: raw.tile.ok_or_else(|| need("tile"))?,
                dir: raw.dir.ok_or_else(|| need("dir"))?,
                new_tile: raw.new_tile.ok_or_else(|| need("new_tile"))?,
                ratio: raw.ratio.ok_or_else(|| need("ratio"))?,
                panel: raw.panel,
            }),
            "close_tile" => Ok(LayoutOp::CloseTile {
                tile: raw.tile.ok_or_else(|| need("tile"))?,
            }),
            "add_panel" => Ok(LayoutOp::AddPanel {
                panel: raw.panel.ok_or_else(|| need("panel"))?,
                tile: raw.tile,
            }),
            "remove_panel" => Ok(LayoutOp::RemovePanel {
                panel: raw.panel.ok_or_else(|| need("panel"))?,
            }),
            other => Err(format!("unknown op `{}`", clip(other))),
        }
    }
}
