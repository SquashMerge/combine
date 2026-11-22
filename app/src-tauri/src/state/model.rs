//! The on-disk shape of a client state file, version 1.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientStateFile {
    pub version: u32,
    pub last_space: Option<LastSpace>,
    pub spaces: BTreeMap<String, SpaceState>,
}

impl Default for ClientStateFile {
    fn default() -> Self {
        ClientStateFile {
            version: VERSION,
            last_space: None,
            spaces: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LastSpace {
    pub space_id: String,
    pub selected_at_ms: u64,
}

/// What the client remembers about one space. `group_state` is opaque: stored and returned, never read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceState {
    pub space_id: String,
    pub focused_tile: Option<String>,
    pub active_terminal: Option<String>,
    pub active_tabs: BTreeMap<String, String>,
    pub group_state: Value,
}
