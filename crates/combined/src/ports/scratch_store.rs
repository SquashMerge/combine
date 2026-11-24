//! The daemon's own folders for scratch panels, outside every project.

use async_trait::async_trait;

use super::file_store::{StoreError, StoredFile, WriteOutcome};

/// One scratch panel's folder: its space's key and its panel ID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScratchDir {
    /// [`SpaceId::sha256_hex`](crate::model::SpaceId::sha256_hex) of the panel's space.
    pub key: String,
    pub panel: String,
}

/// The scratch folders found across every space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchListing {
    pub dirs: Vec<ScratchDir>,
    /// True when there were more folders than the caller's `max_dirs`.
    pub truncated: bool,
}

/// Files in scratch panel folders. Each file name is relative to its folder.
#[async_trait]
pub trait ScratchStore: Send + Sync {
    /// Lists the scratch folders of every space, at most `max_dirs` of them.
    async fn list(&self, max_dirs: usize) -> Result<ScratchListing, StoreError>;
    /// Reads `file` of the folder. A file larger than `max_bytes` is [`StoreError::TooLarge`].
    async fn read(
        &self,
        dir: &ScratchDir,
        file: &str,
        max_bytes: usize,
    ) -> Result<Option<StoredFile>, StoreError>;
    async fn write(
        &self,
        dir: &ScratchDir,
        file: &str,
        bytes: Vec<u8>,
    ) -> Result<WriteOutcome, StoreError>;
    /// Removes the whole folder. A missing folder is `Ok`.
    async fn remove(&self, dir: &ScratchDir) -> Result<(), StoreError>;
}
