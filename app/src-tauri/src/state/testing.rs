#![cfg(test)]
//! Test support for the client state layer: a scratch directory and sample values. Nothing here touches the real app data directory.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::json;

use super::model::{ClientSpaceState, LastSpace};

pub const ID_A: &str = "6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6c";
pub const ID_B: &str = "0f8c2a6e-4b1d-4c5e-9a7b-3d2e1f0a9b8c";

static NEXT_DIR: AtomicU32 = AtomicU32::new(0);

pub struct TestDir(PathBuf);

impl TestDir {
    pub fn new() -> Self {
        let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("combine-state-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).expect("create test dir");
        TestDir(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn space_state(space_id: &str) -> ClientSpaceState {
    ClientSpaceState {
        space_id: space_id.to_string(),
        focused_tile: Some("t1".to_string()),
        active_terminal: Some("7".to_string()),
        active_tabs: BTreeMap::from([("t1".to_string(), "p2".to_string())]),
        group_state: json!({ "opaque": [1, 2] }),
    }
}

pub fn last_space(space_id: &str, selected_at_ms: u64) -> LastSpace {
    LastSpace {
        space_id: space_id.to_string(),
        selected_at_ms,
    }
}
