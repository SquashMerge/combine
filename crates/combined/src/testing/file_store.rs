#![cfg(feature = "test-support")]
//! An in-memory [`FileStore`] with one revision counter per space.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;

use crate::model::SpaceId;
use crate::ports::{FileStore, StoreError, StoreRevision, StoredFile};

#[derive(Default)]
struct SpaceFiles {
    revision: u64,
    files: BTreeMap<String, Vec<u8>>,
}

/// Files by space and path. A write bumps its space's revision; reads report the revision the
/// space has at the time.
#[derive(Default)]
pub struct MemoryFileStore {
    spaces: Mutex<BTreeMap<SpaceId, SpaceFiles>>,
}

impl MemoryFileStore {
    pub fn new() -> Self {
        Self::default()
    }
}

/// `'/'`-separated, non-empty, relative, with no empty, `.` or `..` segment.
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\0')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

#[async_trait]
impl FileStore for MemoryFileStore {
    async fn read(&self, space: &SpaceId, path: &str) -> Result<Option<StoredFile>, StoreError> {
        if !valid_path(path) {
            return Err(StoreError::InvalidPath);
        }
        let spaces = self.spaces.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(spaces.get(space).and_then(|entry| {
            entry.files.get(path).map(|bytes| StoredFile {
                bytes: bytes.clone(),
                revision: StoreRevision(entry.revision),
            })
        }))
    }

    async fn write(
        &self,
        space: &SpaceId,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<StoreRevision, StoreError> {
        if !valid_path(path) {
            return Err(StoreError::InvalidPath);
        }
        let mut spaces = self.spaces.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = spaces.entry(space.clone()).or_default();
        entry.revision += 1;
        entry.files.insert(path.to_string(), bytes);
        Ok(StoreRevision(entry.revision))
    }

    async fn revision(&self, space: &SpaceId) -> Result<StoreRevision, StoreError> {
        let spaces = self.spaces.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(StoreRevision(
            spaces.get(space).map_or(0, |entry| entry.revision),
        ))
    }
}
