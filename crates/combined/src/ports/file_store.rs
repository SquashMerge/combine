//! The per-space file store, minimal surface.

use std::fmt;

use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::model::{ContentHash, SpaceId};

/// One counter per space; starts at 0 and goes up by 1 on each successful write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StoreRevision(pub u64);

/// A stored file and the revision it was read at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    pub bytes: Vec<u8>,
    pub revision: StoreRevision,
    /// The hash of `bytes`.
    pub hash: ContentHash,
    /// True when the bytes came from the fallback copy, not from the project.
    pub read_only: bool,
}

/// How a successful write ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOutcome {
    pub revision: StoreRevision,
    /// The hash of the bytes written.
    pub hash: ContentHash,
    /// True when the project could not be written and the bytes went to the fallback copy.
    pub read_only: bool,
}

/// A file under a watched space changed, by this store or by anyone else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub space: SpaceId,
    /// Relative to `<space>/.workspace/`, as for [`FileStore::read`].
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    InvalidPath,
    Unavailable(String),
    /// The project can't be written and the store has no fallback.
    ReadOnly(String),
    /// A read found a file larger than the caller's `max_bytes`. The store read no more than
    /// `max_bytes + 1` bytes of it; `size` is at least that, and more when the store knows.
    TooLarge {
        size: u64,
        /// True when the file is the fallback copy, as for [`StoredFile::read_only`].
        read_only: bool,
    },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::InvalidPath => write!(f, "invalid path"),
            StoreError::Unavailable(detail) => write!(f, "store unavailable: {detail}"),
            StoreError::ReadOnly(detail) => write!(f, "store is read-only: {detail}"),
            StoreError::TooLarge { size, .. } => write!(f, "the file is too large: {size} bytes"),
        }
    }
}

impl std::error::Error for StoreError {}

/// `'/'`-separated, non-empty, relative, with no empty, `.` or `..` segment.
pub fn valid_store_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\0')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// Files under a space's `.workspace/` directory.
#[async_trait]
pub trait FileStore: Send + Sync {
    /// `path` is relative to `<space>/.workspace/`: '/'-separated, non-empty, no `.`/`..`
    /// segments, not absolute. A file larger than `max_bytes` is [`StoreError::TooLarge`]: the
    /// store never holds more than `max_bytes + 1` of its bytes, even if it grows meanwhile.
    async fn read(
        &self,
        space: &SpaceId,
        path: &str,
        max_bytes: usize,
    ) -> Result<Option<StoredFile>, StoreError>;
    async fn write(
        &self,
        space: &SpaceId,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<WriteOutcome, StoreError>;
    async fn revision(&self, space: &SpaceId) -> Result<StoreRevision, StoreError>;
    /// Starts reporting changes under the space's `.workspace/` on [`FileStore::changes`].
    async fn watch(&self, space: &SpaceId) -> Result<(), StoreError>;
    /// The stream of changes in the spaces being watched. The first call gets it; later calls
    /// get an empty stream.
    fn changes(&self) -> BoxStream<'static, FileChange>;
}
