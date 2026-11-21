//! The per-space file store, minimal surface.

use std::fmt;

use async_trait::async_trait;

use crate::model::SpaceId;

/// One counter per space; starts at 0 and goes up by 1 on each successful write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StoreRevision(pub u64);

/// A stored file and the revision it was read at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    pub bytes: Vec<u8>,
    pub revision: StoreRevision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    InvalidPath,
    Unavailable(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::InvalidPath => write!(f, "invalid path"),
            StoreError::Unavailable(detail) => write!(f, "store unavailable: {detail}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// Files under a space's `.workspace/` directory.
#[async_trait]
pub trait FileStore: Send + Sync {
    /// `path` is relative to `<space>/.workspace/`: '/'-separated, non-empty, no `.`/`..`
    /// segments, not absolute.
    async fn read(&self, space: &SpaceId, path: &str) -> Result<Option<StoredFile>, StoreError>;
    async fn write(
        &self,
        space: &SpaceId,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<StoreRevision, StoreError>;
    async fn revision(&self, space: &SpaceId) -> Result<StoreRevision, StoreError>;
}
