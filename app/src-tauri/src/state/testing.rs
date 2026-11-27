#![cfg(test)]
//! Test support for the client state layer: a scratch directory and sample values. Nothing here touches the real app data directory.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::model::{ClientSpaceState, LastSpace};

pub const ID_A: &str = "6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6c";
pub const ID_B: &str = "0f8c2a6e-4b1d-4c5e-9a7b-3d2e1f0a9b8c";

/// A new directory `<parent>/<prefix>-<8 random hex digits>`, mode 0700. As with `mkdtemp`, a
/// name that exists (a killed run's leftover: pids are reused) is retried, never reused.
/// A copy of `combined::testing::fresh_dir`: the app's own code doesn't depend on `combined`,
/// and `hosts`, which has another copy, is a peer layer.
fn fresh_dir(parent: &Path, prefix: &str) -> PathBuf {
    use std::hash::{BuildHasher, Hasher};
    use std::os::unix::fs::DirBuilderExt;
    for _ in 0..16 {
        let suffix = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish() as u32;
        let path = parent.join(format!("{prefix}-{suffix:08x}"));
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            created => {
                created.unwrap_or_else(|error| panic!("creating {}: {error}", path.display()));
                return path;
            }
        }
    }
    panic!("no free {prefix}-* name under {}", parent.display());
}

pub struct TestDir(PathBuf);

impl TestDir {
    pub fn new() -> Self {
        TestDir(fresh_dir(&std::env::temp_dir(), "combine-state"))
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
