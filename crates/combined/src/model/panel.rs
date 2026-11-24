//! Panels the daemon stores: where one lives and what a view panel's `panel.json` holds.

use combine_protocol::{CreatedBy, Renderer};

/// Where a panel lives: in the project, or in the daemon's own data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifetime {
    Persistent,
    Scratch,
}

/// The fields of a view panel's `panel.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewPanelFile {
    pub renderer: Renderer,
    pub title: Option<String>,
    pub idempotency_key: Option<String>,
    pub created_by: Option<CreatedBy>,
}
