//! A space's tiling layout: the tree of splits and tiles stored in `layout.json`.

use std::fmt;

use serde_json::{Map, Value};

const MAX_ID_BYTES: usize = 256;

/// Why a tile or panel ID was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidId {
    Empty,
    TooLong,
    ControlCharacter,
}

impl fmt::Display for InvalidId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvalidId::Empty => write!(f, "an ID can't be empty"),
            InvalidId::TooLong => write!(f, "an ID can be at most {MAX_ID_BYTES} bytes"),
            InvalidId::ControlCharacter => write!(f, "an ID can't contain control characters"),
        }
    }
}

fn check_id(text: &str) -> Result<(), InvalidId> {
    if text.is_empty() {
        Err(InvalidId::Empty)
    } else if text.len() > MAX_ID_BYTES {
        Err(InvalidId::TooLong)
    } else if text.chars().any(char::is_control) {
        Err(InvalidId::ControlCharacter)
    } else {
        Ok(())
    }
}

/// An opaque tile ID: non-empty, at most 256 bytes, no control characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId(String);

impl TileId {
    pub fn new(text: impl Into<String>) -> Result<TileId, InvalidId> {
        let text = text.into();
        check_id(&text)?;
        Ok(TileId(text))
    }

    /// The only tile of a default layout.
    pub fn main() -> TileId {
        TileId("main".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An opaque panel ID with the same rules as [`TileId`]. The layout stores it whether or not a
/// panel by that ID exists.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PanelId(String);

impl PanelId {
    pub fn new(text: impl Into<String>) -> Result<PanelId, InvalidId> {
        let text = text.into();
        check_id(&text)?;
        Ok(PanelId(text))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `H` puts `a` left of `b`; `V` puts `a` above `b`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SplitDir {
    H,
    V,
}

/// Two children side by side. `ratio` is `a`'s share of the space.
#[derive(Debug, Clone, PartialEq)]
pub struct Split {
    pub dir: SplitDir,
    pub ratio: f64,
    pub a: Box<SplitNode>,
    pub b: Box<SplitNode>,
    /// Keys this version doesn't know, kept so a rewrite doesn't lose them.
    pub extra: Map<String, Value>,
}

/// A leaf that holds panels and shows one of them.
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub id: TileId,
    pub panels: Vec<PanelId>,
    pub active: Option<PanelId>,
    pub extra: Map<String, Value>,
}

/// In JSON, tagged `"type": "split"` or `"type": "tile"`.
#[derive(Debug, Clone, PartialEq)]
pub enum SplitNode {
    Split(Split),
    Tile(Tile),
}

/// The contents of one `layout.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceLayout {
    pub version: u32,
    pub root: SplitNode,
    /// Kept untouched. Defaults to `[]` and is always written.
    pub groups: Value,
    pub extra: Map<String, Value>,
}

/// Counts the accepted changes to a space's layout, so clients can tell a stale one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct LayoutRevision(pub u64);
