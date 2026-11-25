//! Layout files: parsing, validation, encoding and the typed ops that change a layout. No I/O.
//!
//! A pending change is a list of [`LayoutOp`]s. Besides the new layout, applying them reports
//! which [`FieldPath`]s they touched, in the coordinates of the file as last read from disk, so
//! the layout sync can tell whether an outside edit overlaps them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::{Map, Number, Value};

use crate::model::{InvalidId, PanelId, SpaceLayout, Split, SplitDir, SplitNode, Tile, TileId};

/// The largest encoded layout the daemon accepts or writes.
pub const LAYOUT_MAX_BYTES: usize = 256 * 1024;
/// The most nodes on one path from the root to a tile.
pub const LAYOUT_MAX_DEPTH: usize = 64;
const SUPPORTED_VERSION: u32 = 1;

/// Why a layout, or a change to it, was refused. The text is shown to the user.
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutError {
    Malformed(String),
    MissingVersion,
    UnsupportedVersion(u64),
    TooLarge(usize),
    TooDeep,
    InvalidId(InvalidId),
    RatioOutOfRange(f64),
    DuplicateTile(TileId),
    DuplicatePanel(PanelId),
    ActiveNotInPanels(TileId),
    EmptyTileHasActive(TileId),
    Encode(String),
    UnknownTile(TileId),
    UnknownPanel(PanelId),
    UnknownSplit,
    PanelNotInTile(TileId, PanelId),
    TileIdInUse(TileId),
    CannotCloseRoot,
    PanelInLayout(PanelId),
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LayoutError::Malformed(why) => write!(f, "the layout is malformed: {why}"),
            LayoutError::MissingVersion => write!(f, "the layout has no version"),
            LayoutError::UnsupportedVersion(version) => {
                write!(
                    f,
                    "layout version {version} isn't supported (expected {SUPPORTED_VERSION})"
                )
            }
            LayoutError::TooLarge(bytes) => {
                write!(
                    f,
                    "the layout is {bytes} bytes; the limit is {LAYOUT_MAX_BYTES}"
                )
            }
            LayoutError::TooDeep => {
                write!(
                    f,
                    "the layout nests more than {LAYOUT_MAX_DEPTH} levels deep"
                )
            }
            LayoutError::InvalidId(why) => write!(f, "invalid ID: {why}"),
            LayoutError::RatioOutOfRange(ratio) => {
                write!(
                    f,
                    "split ratio {ratio} must be greater than 0 and less than 1"
                )
            }
            LayoutError::DuplicateTile(id) => write!(f, "tile {} appears twice", id.as_str()),
            LayoutError::DuplicatePanel(id) => write!(f, "panel {} appears twice", id.as_str()),
            LayoutError::ActiveNotInPanels(id) => {
                write!(
                    f,
                    "tile {} has an active panel that isn't one of its panels",
                    id.as_str()
                )
            }
            LayoutError::EmptyTileHasActive(id) => {
                write!(
                    f,
                    "tile {} has no panels but has an active panel",
                    id.as_str()
                )
            }
            LayoutError::Encode(why) => write!(f, "can't encode the layout: {why}"),
            LayoutError::UnknownTile(id) => write!(f, "there is no tile {}", id.as_str()),
            LayoutError::UnknownPanel(id) => {
                write!(f, "there is no panel {} in the layout", id.as_str())
            }
            LayoutError::UnknownSplit => write!(f, "there is no split with those tiles"),
            LayoutError::PanelNotInTile(tile, panel) => {
                write!(
                    f,
                    "panel {} isn't in tile {}",
                    panel.as_str(),
                    tile.as_str()
                )
            }
            LayoutError::TileIdInUse(id) => write!(f, "tile {} already exists", id.as_str()),
            LayoutError::CannotCloseRoot => write!(f, "the only tile can't be closed"),
            LayoutError::PanelInLayout(id) => {
                write!(f, "panel {} is already in the layout", id.as_str())
            }
        }
    }
}

impl std::error::Error for LayoutError {}

impl From<InvalidId> for LayoutError {
    fn from(why: InvalidId) -> LayoutError {
        LayoutError::InvalidId(why)
    }
}

/// A new layout with a single empty tile, used until the first change is accepted.
pub fn default_layout() -> SpaceLayout {
    SpaceLayout {
        version: SUPPORTED_VERSION,
        root: SplitNode::Tile(Tile {
            id: TileId::main(),
            panels: Vec::new(),
            active: None,
            extra: Map::new(),
        }),
        groups: Value::Array(Vec::new()),
        extra: Map::new(),
    }
}

/// Reads the structure of a layout. Unknown keys go to `extra`; `groups` is kept as it is.
/// Whether the layout is acceptable beyond its shape is [`validate_layout`]'s question.
pub fn parse_layout(value: &Value) -> Result<SpaceLayout, LayoutError> {
    let object = value
        .as_object()
        .ok_or_else(|| LayoutError::Malformed("the top level must be an object".to_string()))?;
    let version = match object.get("version") {
        None => return Err(LayoutError::MissingVersion),
        Some(version) => version
            .as_u64()
            .ok_or_else(|| LayoutError::Malformed("version must be a whole number".to_string()))?,
    };
    if version != u64::from(SUPPORTED_VERSION) {
        return Err(LayoutError::UnsupportedVersion(version));
    }
    let root = object
        .get("root")
        .ok_or_else(|| LayoutError::Malformed("root is missing".to_string()))?;
    let groups = object
        .get("groups")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let mut extra = object.clone();
    for known in ["version", "root", "groups"] {
        extra.remove(known);
    }
    Ok(SpaceLayout {
        version: SUPPORTED_VERSION,
        root: parse_node(root, 1)?,
        groups,
        extra,
    })
}

fn parse_node(value: &Value, depth: usize) -> Result<SplitNode, LayoutError> {
    if depth > LAYOUT_MAX_DEPTH {
        return Err(LayoutError::TooDeep);
    }
    let object = value
        .as_object()
        .ok_or_else(|| LayoutError::Malformed("a node must be an object".to_string()))?;
    let mut extra = object.clone();
    extra.remove("type");
    match object.get("type").and_then(Value::as_str) {
        Some("split") => {
            for known in ["dir", "ratio", "a", "b"] {
                extra.remove(known);
            }
            let dir = match object.get("dir").and_then(Value::as_str) {
                Some("h") => SplitDir::H,
                Some("v") => SplitDir::V,
                _ => {
                    return Err(LayoutError::Malformed(
                        "a split's dir must be \"h\" or \"v\"".to_string(),
                    ))
                }
            };
            let ratio = object.get("ratio").and_then(Value::as_f64).ok_or_else(|| {
                LayoutError::Malformed("a split's ratio must be a number".to_string())
            })?;
            let child = |key: &str| -> Result<Box<SplitNode>, LayoutError> {
                let child = object
                    .get(key)
                    .ok_or_else(|| LayoutError::Malformed(format!("a split is missing {key}")))?;
                Ok(Box::new(parse_node(child, depth + 1)?))
            };
            Ok(SplitNode::Split(Split {
                dir,
                ratio,
                a: child("a")?,
                b: child("b")?,
                extra,
            }))
        }
        Some("tile") => {
            for known in ["id", "panels", "active"] {
                extra.remove(known);
            }
            let id = object.get("id").and_then(Value::as_str).ok_or_else(|| {
                LayoutError::Malformed("a tile's id must be a string".to_string())
            })?;
            let panels = object
                .get("panels")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    LayoutError::Malformed("a tile's panels must be a list".to_string())
                })?
                .iter()
                .map(|panel| {
                    let text = panel.as_str().ok_or_else(|| {
                        LayoutError::Malformed("a panel ID must be a string".to_string())
                    })?;
                    Ok(PanelId::new(text)?)
                })
                .collect::<Result<Vec<_>, LayoutError>>()?;
            let active = match object.get("active") {
                None | Some(Value::Null) => None,
                Some(Value::String(text)) => Some(PanelId::new(text.as_str())?),
                Some(_) => {
                    return Err(LayoutError::Malformed(
                        "a tile's active must be a panel ID or null".to_string(),
                    ))
                }
            };
            Ok(SplitNode::Tile(Tile {
                id: TileId::new(id)?,
                panels,
                active,
                extra,
            }))
        }
        _ => Err(LayoutError::Malformed(
            "a node's type must be \"split\" or \"tile\"".to_string(),
        )),
    }
}

/// Checks everything the daemon requires of a layout: version 1, at most [`LAYOUT_MAX_BYTES`]
/// encoded, at most [`LAYOUT_MAX_DEPTH`] deep, finite ratios strictly between 0 and 1, unique
/// tile IDs and unique panel IDs, and an active panel that matches each tile's panels.
pub fn validate_layout(layout: &SpaceLayout) -> Result<(), LayoutError> {
    if layout.version != SUPPORTED_VERSION {
        return Err(LayoutError::UnsupportedVersion(u64::from(layout.version)));
    }
    check_node(&layout.root, 1, &mut BTreeSet::new(), &mut BTreeSet::new())?;
    let size = encode_layout(layout)?.len();
    if size > LAYOUT_MAX_BYTES {
        return Err(LayoutError::TooLarge(size));
    }
    Ok(())
}

fn check_node(
    node: &SplitNode,
    depth: usize,
    tiles: &mut BTreeSet<TileId>,
    panels: &mut BTreeSet<PanelId>,
) -> Result<(), LayoutError> {
    if depth > LAYOUT_MAX_DEPTH {
        return Err(LayoutError::TooDeep);
    }
    match node {
        SplitNode::Split(split) => {
            if !(split.ratio.is_finite() && split.ratio > 0.0 && split.ratio < 1.0) {
                return Err(LayoutError::RatioOutOfRange(split.ratio));
            }
            check_node(&split.a, depth + 1, tiles, panels)?;
            check_node(&split.b, depth + 1, tiles, panels)
        }
        SplitNode::Tile(tile) => {
            if !tiles.insert(tile.id.clone()) {
                return Err(LayoutError::DuplicateTile(tile.id.clone()));
            }
            for panel in &tile.panels {
                if !panels.insert(panel.clone()) {
                    return Err(LayoutError::DuplicatePanel(panel.clone()));
                }
            }
            match &tile.active {
                None if tile.panels.is_empty() => Ok(()),
                Some(_) if tile.panels.is_empty() => {
                    Err(LayoutError::EmptyTileHasActive(tile.id.clone()))
                }
                Some(active) if tile.panels.contains(active) => Ok(()),
                _ => Err(LayoutError::ActiveNotInPanels(tile.id.clone())),
            }
        }
    }
}

/// The file's bytes: pretty-printed JSON and a trailing newline.
pub fn encode_layout(layout: &SpaceLayout) -> Result<Vec<u8>, LayoutError> {
    let mut object = layout.extra.clone();
    object.insert("version".to_string(), Value::from(layout.version));
    object.insert("root".to_string(), node_value(&layout.root)?);
    object.insert("groups".to_string(), layout.groups.clone());
    let mut bytes = serde_json::to_vec_pretty(&Value::Object(object))
        .map_err(|why| LayoutError::Encode(why.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn node_value(node: &SplitNode) -> Result<Value, LayoutError> {
    match node {
        SplitNode::Split(split) => {
            let mut object = split.extra.clone();
            object.insert("type".to_string(), Value::from("split"));
            let dir = match split.dir {
                SplitDir::H => "h",
                SplitDir::V => "v",
            };
            object.insert("dir".to_string(), Value::from(dir));
            let ratio = Number::from_f64(split.ratio)
                .ok_or_else(|| LayoutError::Encode("a split ratio isn't finite".to_string()))?;
            object.insert("ratio".to_string(), Value::Number(ratio));
            object.insert("a".to_string(), node_value(&split.a)?);
            object.insert("b".to_string(), node_value(&split.b)?);
            Ok(Value::Object(object))
        }
        SplitNode::Tile(tile) => {
            let mut object = tile.extra.clone();
            object.insert("type".to_string(), Value::from("tile"));
            object.insert("id".to_string(), Value::from(tile.id.as_str()));
            let panels = tile
                .panels
                .iter()
                .map(|panel| Value::from(panel.as_str()))
                .collect();
            object.insert("panels".to_string(), Value::Array(panels));
            let active = tile
                .active
                .as_ref()
                .map_or(Value::Null, |panel| Value::from(panel.as_str()));
            object.insert("active".to_string(), active);
            Ok(Value::Object(object))
        }
    }
}

/// Names a split by the tiles under it, which is unique within a layout and survives a change
/// to the split's own ratio.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SplitKey(pub BTreeSet<TileId>);

/// A part of a layout that changes independently of the others.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FieldPath {
    /// The splits and tiles present, each split's direction and where each tile sits.
    Structure,
    Ratio(SplitKey),
    TilePanels(TileId),
    TileActive(TileId),
    Groups,
    Extra,
}

/// Maps each split's current key to its key in the file as last read, or to `None` when a
/// pending [`LayoutOp::SplitTile`] created it.
pub type Origins = BTreeMap<SplitKey, Option<SplitKey>>;

/// One change to a layout. New tile IDs come from the client, so core needs no randomness.
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutOp {
    SetActive {
        tile: TileId,
        panel: PanelId,
    },
    SetRatio {
        leaves: BTreeSet<TileId>,
        ratio: f64,
    },
    MovePanel {
        panel: PanelId,
        to_tile: TileId,
        index: usize,
    },
    SplitTile {
        tile: TileId,
        dir: SplitDir,
        new_tile: TileId,
        ratio: f64,
        panel: Option<PanelId>,
    },
    CloseTile {
        tile: TileId,
    },
    /// Puts a panel ID that isn't in the tree at the end of a tile's `panels`: the tile `tile`
    /// names, else the first tile in tree order (also when `tile` is None). With `activate`, or in
    /// an empty tile, the panel becomes the tile's `active`. Never creates, splits or closes a tile.
    AddPanel {
        panel: PanelId,
        tile: Option<TileId>,
        activate: bool,
    },
    /// Takes a panel ID out of the tile holding it; that tile's `active` moves as `remove_panel`
    /// does.
    RemovePanel {
        panel: PanelId,
    },
}

/// The result of [`apply_ops`].
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedOps {
    pub layout: SpaceLayout,
    pub touched: BTreeSet<FieldPath>,
    pub origins: Origins,
}

/// The origin map of a layout with nothing pending: every split is its own origin.
pub fn identity_origins(layout: &SpaceLayout) -> Origins {
    let mut flat = Flat::default();
    flatten(&layout.root, &mut flat);
    flat.splits
        .into_keys()
        .map(|key| (key.clone(), Some(key)))
        .collect()
}

/// Applies the ops in order to a copy of `layout`, then validates the result. Any failure
/// leaves nothing changed. `origins` is the map for `layout` and comes back updated.
pub fn apply_ops(
    layout: &SpaceLayout,
    origins: &Origins,
    ops: &[LayoutOp],
) -> Result<AppliedOps, LayoutError> {
    let mut layout = layout.clone();
    let mut origins = origins.clone();
    let mut touched = BTreeSet::new();
    for op in ops {
        touched.extend(touched_with_origins(&layout, &origins, op));
        origins = next_origins(&layout, &origins, op);
        apply_op(&mut layout, op)?;
        if depth(&layout.root) > LAYOUT_MAX_DEPTH {
            return Err(LayoutError::TooDeep);
        }
    }
    validate_layout(&layout)?;
    Ok(AppliedOps {
        layout,
        touched,
        origins,
    })
}

/// The fields `op` touches when applied to `layout`, with a split's ratio named by its origin
/// key. A ratio change to a split a pending op created touches nothing more than that op did.
pub fn touched_with_origins(
    layout: &SpaceLayout,
    origins: &Origins,
    op: &LayoutOp,
) -> BTreeSet<FieldPath> {
    let mut touched = BTreeSet::new();
    match op {
        LayoutOp::SetActive { tile, .. } => {
            touched.insert(FieldPath::TileActive(tile.clone()));
        }
        LayoutOp::SetRatio { leaves, .. } => {
            if let Some(Some(origin)) = origins.get(&SplitKey(leaves.clone())) {
                touched.insert(FieldPath::Ratio(origin.clone()));
            }
        }
        LayoutOp::MovePanel { panel, to_tile, .. } => {
            if let Some(source) = tile_holding(&layout.root, panel) {
                touched.insert(FieldPath::TilePanels(source.id.clone()));
                touched.insert(FieldPath::TileActive(source.id.clone()));
            }
            touched.insert(FieldPath::TilePanels(to_tile.clone()));
            touched.insert(FieldPath::TileActive(to_tile.clone()));
        }
        LayoutOp::SplitTile { tile, panel, .. } => {
            touched.insert(FieldPath::Structure);
            if panel.is_some() {
                touched.insert(FieldPath::TilePanels(tile.clone()));
                touched.insert(FieldPath::TileActive(tile.clone()));
            }
        }
        LayoutOp::CloseTile { tile } => {
            touched.insert(FieldPath::Structure);
            touched.insert(FieldPath::TilePanels(tile.clone()));
            touched.insert(FieldPath::TileActive(tile.clone()));
        }
        LayoutOp::AddPanel { tile, .. } => {
            let target = target_tile(&layout.root, tile.as_ref()).id.clone();
            touched.insert(FieldPath::TilePanels(target.clone()));
            touched.insert(FieldPath::TileActive(target));
        }
        LayoutOp::RemovePanel { panel } => {
            if let Some(source) = tile_holding(&layout.root, panel) {
                touched.insert(FieldPath::TilePanels(source.id.clone()));
                touched.insert(FieldPath::TileActive(source.id.clone()));
            }
        }
    }
    touched
}

fn next_origins(layout: &SpaceLayout, origins: &Origins, op: &LayoutOp) -> Origins {
    match op {
        LayoutOp::SplitTile { tile, new_tile, .. } => {
            let mut next: Origins = origins
                .iter()
                .map(|(key, origin)| {
                    let mut key = key.clone();
                    if key.0.contains(tile) {
                        key.0.insert(new_tile.clone());
                    }
                    (key, origin.clone())
                })
                .collect();
            let pair = [tile.clone(), new_tile.clone()].into_iter().collect();
            next.insert(SplitKey(pair), None);
            next
        }
        LayoutOp::CloseTile { tile } => {
            let parent = parent_leaves(&layout.root, tile).map(SplitKey);
            origins
                .iter()
                .filter(|(key, _)| Some(*key) != parent.as_ref())
                .map(|(key, origin)| {
                    let mut key = key.clone();
                    key.0.remove(tile);
                    (key, origin.clone())
                })
                .collect()
        }
        _ => origins.clone(),
    }
}

fn apply_op(layout: &mut SpaceLayout, op: &LayoutOp) -> Result<(), LayoutError> {
    match op {
        LayoutOp::SetActive { tile: id, panel } => {
            let tile = find_tile_mut(&mut layout.root, id)
                .ok_or_else(|| LayoutError::UnknownTile(id.clone()))?;
            if !tile.panels.contains(panel) {
                return Err(LayoutError::PanelNotInTile(id.clone(), panel.clone()));
            }
            tile.active = Some(panel.clone());
        }
        LayoutOp::SetRatio { leaves, ratio } => {
            let split =
                find_split_mut(&mut layout.root, leaves).ok_or(LayoutError::UnknownSplit)?;
            split.ratio = *ratio;
        }
        LayoutOp::MovePanel {
            panel,
            to_tile,
            index,
        } => {
            let source = tile_holding(&layout.root, panel)
                .map(|tile| tile.id.clone())
                .ok_or_else(|| LayoutError::UnknownPanel(panel.clone()))?;
            if find_tile_mut(&mut layout.root, to_tile).is_none() {
                return Err(LayoutError::UnknownTile(to_tile.clone()));
            }
            if let Some(tile) = find_tile_mut(&mut layout.root, &source) {
                remove_panel(tile, panel);
            }
            if let Some(tile) = find_tile_mut(&mut layout.root, to_tile) {
                tile.panels
                    .insert((*index).min(tile.panels.len()), panel.clone());
                tile.active = Some(panel.clone());
            }
        }
        LayoutOp::SplitTile {
            tile: id,
            dir,
            new_tile,
            ratio,
            panel,
        } => split_tile(layout, id, *dir, new_tile, *ratio, panel.as_ref())?,
        LayoutOp::CloseTile { tile } => close_tile(&mut layout.root, tile)?,
        LayoutOp::AddPanel {
            panel,
            tile,
            activate,
        } => {
            if tile_holding(&layout.root, panel).is_some() {
                return Err(LayoutError::PanelInLayout(panel.clone()));
            }
            let target = target_tile(&layout.root, tile.as_ref()).id.clone();
            if let Some(tile) = find_tile_mut(&mut layout.root, &target) {
                tile.panels.push(panel.clone());
                if *activate || tile.active.is_none() {
                    tile.active = Some(panel.clone());
                }
            }
        }
        LayoutOp::RemovePanel { panel } => {
            let source = tile_holding(&layout.root, panel)
                .map(|tile| tile.id.clone())
                .ok_or_else(|| LayoutError::UnknownPanel(panel.clone()))?;
            if let Some(tile) = find_tile_mut(&mut layout.root, &source) {
                remove_panel(tile, panel);
            }
        }
    }
    Ok(())
}

fn split_tile(
    layout: &mut SpaceLayout,
    id: &TileId,
    dir: SplitDir,
    new_tile: &TileId,
    ratio: f64,
    panel: Option<&PanelId>,
) -> Result<(), LayoutError> {
    let mut flat = Flat::default();
    flatten(&layout.root, &mut flat);
    if !flat.tiles.contains_key(id) {
        return Err(LayoutError::UnknownTile(id.clone()));
    }
    if flat.tiles.contains_key(new_tile) {
        return Err(LayoutError::TileIdInUse(new_tile.clone()));
    }
    let node = find_tile_node_mut(&mut layout.root, id)
        .ok_or_else(|| LayoutError::UnknownTile(id.clone()))?;
    let SplitNode::Tile(tile) = node else {
        return Err(LayoutError::UnknownTile(id.clone()));
    };
    let mut created = Tile {
        id: new_tile.clone(),
        panels: Vec::new(),
        active: None,
        extra: Map::new(),
    };
    if let Some(panel) = panel {
        if !tile.panels.contains(panel) {
            return Err(LayoutError::PanelNotInTile(id.clone(), panel.clone()));
        }
        remove_panel(tile, panel);
        created.panels.push(panel.clone());
        created.active = Some(panel.clone());
    }
    let kept = tile.clone();
    *node = SplitNode::Split(Split {
        dir,
        ratio,
        a: Box::new(SplitNode::Tile(kept)),
        b: Box::new(SplitNode::Tile(created)),
        extra: Map::new(),
    });
    Ok(())
}

fn close_tile(root: &mut SplitNode, id: &TileId) -> Result<(), LayoutError> {
    if matches!(root, SplitNode::Tile(tile) if &tile.id == id) {
        return Err(LayoutError::CannotCloseRoot);
    }
    if close_in(root, id) {
        Ok(())
    } else {
        Err(LayoutError::UnknownTile(id.clone()))
    }
}

fn close_in(node: &mut SplitNode, id: &TileId) -> bool {
    let SplitNode::Split(split) = node else {
        return false;
    };
    let sibling = if is_tile(&split.a, id) {
        Some((*split.b).clone())
    } else if is_tile(&split.b, id) {
        Some((*split.a).clone())
    } else {
        None
    };
    if let Some(sibling) = sibling {
        *node = sibling;
        return true;
    }
    let SplitNode::Split(split) = node else {
        return false;
    };
    close_in(&mut split.a, id) || close_in(&mut split.b, id)
}

fn is_tile(node: &SplitNode, id: &TileId) -> bool {
    matches!(node, SplitNode::Tile(tile) if &tile.id == id)
}

/// Removes `panel`. If it was the active one, the panel now at its index becomes active, else
/// the previous one, else none.
fn remove_panel(tile: &mut Tile, panel: &PanelId) {
    let Some(index) = tile.panels.iter().position(|candidate| candidate == panel) else {
        return;
    };
    tile.panels.remove(index);
    if tile.active.as_ref() == Some(panel) {
        tile.active = tile
            .panels
            .get(index)
            .or_else(|| {
                index
                    .checked_sub(1)
                    .and_then(|previous| tile.panels.get(previous))
            })
            .cloned();
    }
}

fn find_tile_node_mut<'a>(node: &'a mut SplitNode, id: &TileId) -> Option<&'a mut SplitNode> {
    if is_tile(node, id) {
        return Some(node);
    }
    match node {
        SplitNode::Split(split) => {
            find_tile_node_mut(&mut split.a, id).or_else(|| find_tile_node_mut(&mut split.b, id))
        }
        SplitNode::Tile(_) => None,
    }
}

fn find_tile_mut<'a>(node: &'a mut SplitNode, id: &TileId) -> Option<&'a mut Tile> {
    match find_tile_node_mut(node, id)? {
        SplitNode::Tile(tile) => Some(tile),
        SplitNode::Split(_) => None,
    }
}

/// The first tile in tree order: depth first, a split's `a` before its `b`.
pub fn first_tile(node: &SplitNode) -> &Tile {
    match node {
        SplitNode::Tile(tile) => tile,
        SplitNode::Split(split) => first_tile(&split.a),
    }
}

/// The tile `hint` names, else the first tile in tree order.
pub fn target_tile<'a>(root: &'a SplitNode, hint: Option<&TileId>) -> &'a Tile {
    hint.and_then(|id| find_tile(root, id))
        .unwrap_or_else(|| first_tile(root))
}

fn find_tile<'a>(node: &'a SplitNode, id: &TileId) -> Option<&'a Tile> {
    match node {
        SplitNode::Tile(tile) => (&tile.id == id).then_some(tile),
        SplitNode::Split(split) => find_tile(&split.a, id).or_else(|| find_tile(&split.b, id)),
    }
}

/// The tile whose `panels` hold `panel`.
pub fn tile_holding<'a>(node: &'a SplitNode, panel: &PanelId) -> Option<&'a Tile> {
    match node {
        SplitNode::Tile(tile) => tile.panels.contains(panel).then_some(tile),
        SplitNode::Split(split) => {
            tile_holding(&split.a, panel).or_else(|| tile_holding(&split.b, panel))
        }
    }
}

fn find_split_mut<'a>(node: &'a mut SplitNode, key: &BTreeSet<TileId>) -> Option<&'a mut Split> {
    let SplitNode::Split(split) = node else {
        return None;
    };
    let in_a = leaves(&split.a);
    let in_b = leaves(&split.b);
    if in_a.union(&in_b).cloned().collect::<BTreeSet<_>>() == *key {
        Some(split)
    } else if key.is_subset(&in_a) {
        find_split_mut(&mut split.a, key)
    } else if key.is_subset(&in_b) {
        find_split_mut(&mut split.b, key)
    } else {
        None
    }
}

fn parent_leaves(node: &SplitNode, id: &TileId) -> Option<BTreeSet<TileId>> {
    let SplitNode::Split(split) = node else {
        return None;
    };
    if is_tile(&split.a, id) || is_tile(&split.b, id) {
        return Some(leaves(node));
    }
    parent_leaves(&split.a, id).or_else(|| parent_leaves(&split.b, id))
}

fn leaves(node: &SplitNode) -> BTreeSet<TileId> {
    match node {
        SplitNode::Tile(tile) => [tile.id.clone()].into_iter().collect(),
        SplitNode::Split(split) => leaves(&split.a).union(&leaves(&split.b)).cloned().collect(),
    }
}

fn depth(node: &SplitNode) -> usize {
    match node {
        SplitNode::Tile(_) => 1,
        SplitNode::Split(split) => 1 + depth(&split.a).max(depth(&split.b)),
    }
}

#[derive(Default)]
struct Flat<'a> {
    tiles: BTreeMap<&'a TileId, &'a Tile>,
    splits: BTreeMap<SplitKey, &'a Split>,
}

fn flatten<'a>(node: &'a SplitNode, flat: &mut Flat<'a>) -> BTreeSet<TileId> {
    match node {
        SplitNode::Tile(tile) => {
            flat.tiles.insert(&tile.id, tile);
            [tile.id.clone()].into_iter().collect()
        }
        SplitNode::Split(split) => {
            let mut under = flatten(&split.a, flat);
            under.extend(flatten(&split.b, flat));
            flat.splits.insert(SplitKey(under.clone()), split);
            under
        }
    }
}

fn same_shape(a: &SplitNode, b: &SplitNode) -> bool {
    match (a, b) {
        (SplitNode::Tile(a), SplitNode::Tile(b)) => a.id == b.id,
        (SplitNode::Split(a), SplitNode::Split(b)) => {
            a.dir == b.dir && same_shape(&a.a, &b.a) && same_shape(&a.b, &b.b)
        }
        _ => false,
    }
}

/// The fields that differ between the layout on disk and `new`. Splits and tiles are matched by
/// key and ID, so a field of one present in only one layout counts as `Structure`.
pub fn diff_fields(disk: &SpaceLayout, new: &SpaceLayout) -> BTreeSet<FieldPath> {
    let mut fields = BTreeSet::new();
    if !same_shape(&disk.root, &new.root) {
        fields.insert(FieldPath::Structure);
    }
    let (mut before, mut after) = (Flat::default(), Flat::default());
    flatten(&disk.root, &mut before);
    flatten(&new.root, &mut after);
    let mut extra_differs = disk.extra != new.extra;
    for (key, old) in &before.splits {
        if let Some(current) = after.splits.get(key) {
            if old.ratio != current.ratio {
                fields.insert(FieldPath::Ratio(key.clone()));
            }
            extra_differs |= old.extra != current.extra;
        }
    }
    for (id, old) in &before.tiles {
        if let Some(current) = after.tiles.get(id) {
            if old.panels != current.panels {
                fields.insert(FieldPath::TilePanels((*id).clone()));
            }
            if old.active != current.active {
                fields.insert(FieldPath::TileActive((*id).clone()));
            }
            extra_differs |= old.extra != current.extra;
        }
    }
    if disk.groups != new.groups {
        fields.insert(FieldPath::Groups);
    }
    if extra_differs {
        fields.insert(FieldPath::Extra);
    }
    fields
}
