//! Display facts.

use serde::{Deserialize, Serialize};

/// What the client's terminal looks like. The field is `cursor_color` because
/// `cursor` already means the attachment cursor.
///
/// Colours are `#rrggbb` and the cell size is in pixels. Neither is validated
/// here; that is the daemon's job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Display {
    pub foreground: String,
    pub background: String,
    pub cursor_color: String,
    pub cell_width: u16,
    pub cell_height: u16,
}
