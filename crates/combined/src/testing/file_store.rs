#![cfg(feature = "test-support")]
//! An in-memory [`FileStore`] with one revision counter per space.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::model::{ContentHash, SpaceId};
use crate::ports::{
    valid_store_path, FileChange, FileStore, StoreError, StoreRevision, StoredFile, WriteOutcome,
};

#[derive(Default)]
struct SpaceFiles {
    revision: u64,
    files: BTreeMap<String, Vec<u8>>,
    /// Where writes go while the space is read-only.
    fallback: BTreeMap<String, Vec<u8>>,
    read_only: bool,
    watched: bool,
    write_counts: BTreeMap<String, u64>,
    read_counts: BTreeMap<String, u64>,
    fail_next_write: Option<StoreError>,
}

struct State {
    spaces: BTreeMap<SpaceId, SpaceFiles>,
    changes: UnboundedSender<FileChange>,
}

impl State {
    /// Queues a change for a watched space. Called with the state's lock held, so a change is
    /// in the stream before the call that caused it returns.
    fn notify(&self, space: &SpaceId, path: &str) {
        if self.spaces.get(space).is_some_and(|entry| entry.watched) {
            let _ = self.changes.send(FileChange {
                space: space.clone(),
                path: path.to_string(),
            });
        }
    }
}

/// Files by space and path. A write bumps its space's revision; reads report the revision the
/// space has at the time. A write to a watched space queues a [`FileChange`], and so does
/// [`MemoryFileStore::write_outside`] and [`MemoryFileStore::remove_outside`], which stand for
/// someone else editing the file.
pub struct MemoryFileStore {
    state: Mutex<State>,
    changes: Mutex<Option<UnboundedReceiver<FileChange>>>,
    fallback: bool,
}

impl Default for MemoryFileStore {
    fn default() -> Self {
        let (changes_tx, changes_rx) = mpsc::unbounded_channel();
        MemoryFileStore {
            state: Mutex::new(State {
                spaces: BTreeMap::new(),
                changes: changes_tx,
            }),
            changes: Mutex::new(Some(changes_rx)),
            fallback: true,
        }
    }
}

fn content_hash(bytes: &[u8]) -> ContentHash {
    ContentHash(Sha256::digest(bytes).into())
}

impl MemoryFileStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A read-only space then answers writes with `ReadOnly`, as a store with no fallback does.
    pub fn without_fallback(mut self) -> Self {
        self.fallback = false;
        self
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// While set, writes to `space` go to the fallback copy, or fail with `ReadOnly` without one.
    pub fn set_read_only(&self, space: &SpaceId, read_only: bool) {
        self.state()
            .spaces
            .entry(space.clone())
            .or_default()
            .read_only = read_only;
    }

    /// The next write to `space` fails with `error`.
    pub fn fail_next_write(&self, space: &SpaceId, error: StoreError) {
        self.state()
            .spaces
            .entry(space.clone())
            .or_default()
            .fail_next_write = Some(error);
    }

    /// Replaces a file as someone else would, without the store's own bookkeeping, and queues a
    /// change.
    pub fn write_outside(&self, space: &SpaceId, path: &str, bytes: Vec<u8>) {
        let mut state = self.state();
        state
            .spaces
            .entry(space.clone())
            .or_default()
            .files
            .insert(path.to_string(), bytes);
        state.notify(space, path);
    }

    /// Deletes a file as someone else would, and queues a change.
    pub fn remove_outside(&self, space: &SpaceId, path: &str) {
        let mut state = self.state();
        if let Some(entry) = state.spaces.get_mut(space) {
            entry.files.remove(path);
        }
        state.notify(space, path);
    }

    /// The project's copy of a file.
    pub fn bytes(&self, space: &SpaceId, path: &str) -> Option<Vec<u8>> {
        let state = self.state();
        state.spaces.get(space)?.files.get(path).cloned()
    }

    /// The fallback copy of a file.
    pub fn fallback_bytes(&self, space: &SpaceId, path: &str) -> Option<Vec<u8>> {
        let state = self.state();
        state.spaces.get(space)?.fallback.get(path).cloned()
    }

    /// How many writes to the file succeeded, to the project or the fallback.
    pub fn write_count(&self, space: &SpaceId, path: &str) -> u64 {
        let state = self.state();
        state
            .spaces
            .get(space)
            .and_then(|entry| entry.write_counts.get(path))
            .copied()
            .unwrap_or(0)
    }

    /// How many reads of the file reached the store, found or not.
    pub fn read_count(&self, space: &SpaceId, path: &str) -> u64 {
        let state = self.state();
        state
            .spaces
            .get(space)
            .and_then(|entry| entry.read_counts.get(path))
            .copied()
            .unwrap_or(0)
    }
}

#[async_trait]
impl FileStore for MemoryFileStore {
    async fn read(&self, space: &SpaceId, path: &str) -> Result<Option<StoredFile>, StoreError> {
        if !valid_store_path(path) {
            return Err(StoreError::InvalidPath);
        }
        let mut state = self.state();
        let Some(entry) = state.spaces.get_mut(space) else {
            return Ok(None);
        };
        *entry.read_counts.entry(path.to_string()).or_default() += 1;
        let revision = StoreRevision(entry.revision);
        let (bytes, read_only) = match entry.fallback.get(path) {
            Some(bytes) => (bytes, true),
            None => match entry.files.get(path) {
                Some(bytes) => (bytes, false),
                None => return Ok(None),
            },
        };
        Ok(Some(StoredFile {
            bytes: bytes.clone(),
            revision,
            hash: content_hash(bytes),
            read_only,
        }))
    }

    async fn write(
        &self,
        space: &SpaceId,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<WriteOutcome, StoreError> {
        if !valid_store_path(path) {
            return Err(StoreError::InvalidPath);
        }
        let mut state = self.state();
        let entry = state.spaces.entry(space.clone()).or_default();
        if let Some(error) = entry.fail_next_write.take() {
            return Err(error);
        }
        if entry.read_only && !self.fallback {
            return Err(StoreError::ReadOnly(format!("{} is read-only", space.0)));
        }
        let hash = content_hash(&bytes);
        let read_only = entry.read_only;
        entry.revision += 1;
        let revision = StoreRevision(entry.revision);
        *entry.write_counts.entry(path.to_string()).or_default() += 1;
        if read_only {
            entry.fallback.insert(path.to_string(), bytes);
        } else {
            entry.files.insert(path.to_string(), bytes);
            state.notify(space, path);
        }
        Ok(WriteOutcome {
            revision,
            hash,
            read_only,
        })
    }

    async fn revision(&self, space: &SpaceId) -> Result<StoreRevision, StoreError> {
        let state = self.state();
        Ok(StoreRevision(
            state.spaces.get(space).map_or(0, |entry| entry.revision),
        ))
    }

    async fn watch(&self, space: &SpaceId) -> Result<(), StoreError> {
        self.state()
            .spaces
            .entry(space.clone())
            .or_default()
            .watched = true;
        Ok(())
    }

    fn changes(&self) -> BoxStream<'static, FileChange> {
        let receiver = self
            .changes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match receiver {
            Some(receiver) => stream::unfold(receiver, |mut receiver| async move {
                receiver.recv().await.map(|change| (change, receiver))
            })
            .boxed(),
            None => stream::empty().boxed(),
        }
    }
}
