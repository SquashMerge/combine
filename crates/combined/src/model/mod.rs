//! Plain data shared by core, ports and adapters. No I/O.

mod space;
mod terminal;
mod time;

pub use space::{RootKind, Space, SpaceId};
pub use terminal::{SpawnSpec, TermEvent, TermInfo, TermState};
pub use time::Millis;
