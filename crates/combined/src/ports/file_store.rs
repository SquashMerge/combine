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

/// What a file was when the daemon last read or wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMark {
    Missing,
    Hash(ContentHash),
    TooLarge,
}

impl From<Option<ContentHash>> for FileMark {
    fn from(hash: Option<ContentHash>) -> FileMark {
        match hash {
            Some(hash) => FileMark::Hash(hash),
            None => FileMark::Missing,
        }
    }
}

impl FileMark {
    /// Whether `read` shows the file as this mark says. A read that failed for any other reason
    /// never matches. The fallback flag is ignored: writes follow the fallback copy.
    pub fn matches(&self, read: &Result<Option<StoredFile>, StoreError>) -> bool {
        match (self, read) {
            (FileMark::Missing, Ok(None)) => true,
            (FileMark::Hash(hash), Ok(Some(file))) => *hash == file.hash,
            (FileMark::TooLarge, Err(StoreError::TooLarge { .. })) => true,
            (FileMark::Missing | FileMark::Hash(_) | FileMark::TooLarge, _) => false,
        }
    }

    /// As [`FileMark::matches`], but false for any read of the fallback copy.
    pub fn matches_project(&self, read: &Result<Option<StoredFile>, StoreError>) -> bool {
        let fallback = match read {
            Ok(Some(file)) => file.read_only,
            Err(StoreError::TooLarge { read_only, .. }) => *read_only,
            Ok(None)
            | Err(StoreError::InvalidPath | StoreError::Unavailable(_) | StoreError::ReadOnly(_)) => {
                false
            }
        };
        !fallback && self.matches(read)
    }
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

/// What a directory entry is, by its own type: a symlink is never followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

/// One entry of a listed folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedEntry {
    pub name: String,
    pub kind: EntryKind,
}

/// The entries of a folder, sorted by name in byte order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirListing {
    pub entries: Vec<ListedEntry>,
    /// True when the folder had more entries than the caller's `max_entries`.
    pub truncated: bool,
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
    /// As [`FileStore::write`], but only to the project: the fallback copy is never touched, and
    /// a project that can't be written is [`StoreError::ReadOnly`].
    async fn write_project(
        &self,
        space: &SpaceId,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<WriteOutcome, StoreError>;
    /// Removes the project's file `path`; a missing file is `Ok`. The fallback copy is never
    /// touched.
    async fn remove_file(&self, space: &SpaceId, path: &str) -> Result<(), StoreError> {
        let _ = (space, path);
        Err(StoreError::Unavailable(
            "this store can't remove files".into(),
        ))
    }
    /// Removes the project's folder `path` only when it is empty; a non-empty or missing folder
    /// is `Ok`.
    async fn remove_dir(&self, space: &SpaceId, path: &str) -> Result<(), StoreError> {
        let _ = (space, path);
        Err(StoreError::Unavailable(
            "this store can't remove files".into(),
        ))
    }
    async fn revision(&self, space: &SpaceId) -> Result<StoreRevision, StoreError>;
    /// Lists the folder `path`, relative to `<space>/.workspace/` and checked as for
    /// [`FileStore::read`]: an invalid path is [`StoreError::InvalidPath`]. `Ok(None)` when
    /// `.workspace`, a folder on the way or the folder itself doesn't exist. A symlink at any of
    /// those is [`StoreError::Unavailable`]. Names that aren't UTF-8 are left out. The listing
    /// holds at most `max_entries` entries, sorted by name in byte order, and `truncated` is true
    /// when more than `max_entries` UTF-8 entries were seen; the entries kept are then whichever
    /// the store saw first. Each kind is the entry's own. It never consults the fallback copy:
    /// it lists only the project's folder.
    async fn list_dir(
        &self,
        space: &SpaceId,
        path: &str,
        max_entries: usize,
    ) -> Result<Option<DirListing>, StoreError>;
    /// Starts reporting changes under the space's `.workspace/` on [`FileStore::changes`].
    async fn watch(&self, space: &SpaceId) -> Result<(), StoreError>;
    /// The stream of changes in the spaces being watched. The first call gets it; later calls
    /// get an empty stream.
    fn changes(&self) -> BoxStream<'static, FileChange>;
}
