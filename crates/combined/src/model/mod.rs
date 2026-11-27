//! Plain data shared by core, ports and adapters. No I/O.

mod content_hash;
mod layout;
mod panel;
mod space;
mod terminal;
mod time;

pub use content_hash::ContentHash;
pub use layout::{
    InvalidId, LayoutRevision, PanelId, SpaceLayout, Split, SplitDir, SplitNode, Tile, TileId,
    ACTIVE_TERMINAL_TAB,
};
pub use panel::{Lifetime, ViewPanelFile};
pub use space::{RootKind, Space, SpaceId};
pub use terminal::{SpawnSpec, TermEvent, TermInfo, TermState};
pub use time::Millis;
