#![cfg(feature = "test-support")]
//! An in-memory [`ScratchStore`].

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::model::ContentHash;
use crate::ports::{
    ScratchDir, ScratchListing, ScratchStore, StoreError, StoreRevision, StoredFile, WriteOutcome,
};

#[derive(Default)]
struct State {
    dirs: BTreeMap<ScratchDir, BTreeMap<String, Vec<u8>>>,
    revision: u64,
    write_counts: BTreeMap<(ScratchDir, String), u64>,
    remove_counts: BTreeMap<ScratchDir, u64>,
    fail_next_write: Option<StoreError>,
}

/// Scratch folders held in memory. A folder exists while it holds a file.
#[derive(Default)]
pub struct MemoryScratchStore {
    state: Mutex<State>,
}

fn content_hash(bytes: &[u8]) -> ContentHash {
    ContentHash(Sha256::digest(bytes).into())
}

impl MemoryScratchStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A file of a folder.
    pub fn bytes(&self, dir: &ScratchDir, file: &str) -> Option<Vec<u8>> {
        self.state().dirs.get(dir)?.get(file).cloned()
    }

    /// The folders that exist, sorted.
    pub fn dirs(&self) -> Vec<ScratchDir> {
        self.state().dirs.keys().cloned().collect()
    }

    /// Puts a file by hand, to seed an invalid one. It counts as no write.
    pub fn put(&self, dir: &ScratchDir, file: &str, bytes: Vec<u8>) {
        self.state()
            .dirs
            .entry(dir.clone())
            .or_default()
            .insert(file.to_string(), bytes);
    }

    /// How many writes of the file succeeded.
    pub fn write_count(&self, dir: &ScratchDir, file: &str) -> u64 {
        self.state()
            .write_counts
            .get(&(dir.clone(), file.to_string()))
            .copied()
            .unwrap_or(0)
    }

    /// How many removals of the folder were asked for, whether or not it existed.
    pub fn remove_count(&self, dir: &ScratchDir) -> u64 {
        self.state().remove_counts.get(dir).copied().unwrap_or(0)
    }

    /// The next write fails with `error`.
    pub fn fail_next_write(&self, error: StoreError) {
        self.state().fail_next_write = Some(error);
    }
}

#[async_trait]
impl ScratchStore for MemoryScratchStore {
    async fn list(&self, max_dirs: usize) -> Result<ScratchListing, StoreError> {
        let state = self.state();
        Ok(ScratchListing {
            truncated: state.dirs.len() > max_dirs,
            dirs: state.dirs.keys().take(max_dirs).cloned().collect(),
        })
    }

    async fn read(
        &self,
        dir: &ScratchDir,
        file: &str,
        max_bytes: usize,
    ) -> Result<Option<StoredFile>, StoreError> {
        let state = self.state();
        let Some(bytes) = state.dirs.get(dir).and_then(|files| files.get(file)) else {
            return Ok(None);
        };
        if bytes.len() > max_bytes {
            return Err(StoreError::TooLarge {
                size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                read_only: false,
            });
        }
        Ok(Some(StoredFile {
            bytes: bytes.clone(),
            revision: StoreRevision(state.revision),
            hash: content_hash(bytes),
            read_only: false,
        }))
    }

    async fn write(
        &self,
        dir: &ScratchDir,
        file: &str,
        bytes: Vec<u8>,
    ) -> Result<WriteOutcome, StoreError> {
        let mut state = self.state();
        if let Some(error) = state.fail_next_write.take() {
            return Err(error);
        }
        let hash = content_hash(&bytes);
        state
            .dirs
            .entry(dir.clone())
            .or_default()
            .insert(file.to_string(), bytes);
        state.revision += 1;
        *state
            .write_counts
            .entry((dir.clone(), file.to_string()))
            .or_default() += 1;
        Ok(WriteOutcome {
            revision: StoreRevision(state.revision),
            hash,
            read_only: false,
        })
    }

    async fn remove(&self, dir: &ScratchDir) -> Result<(), StoreError> {
        let mut state = self.state();
        state.dirs.remove(dir);
        *state.remove_counts.entry(dir.clone()).or_default() += 1;
        Ok(())
    }
}
