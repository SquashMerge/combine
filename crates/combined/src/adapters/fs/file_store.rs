//! The per-space file store on disk, at `<space>/.workspace/<path>`.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;

use super::safe_write::write_atomic;
use crate::model::SpaceId;
use crate::ports::{FileStore, StoreError, StoreRevision, StoredFile};

/// Files under each space's `.workspace/` folder. Revisions are kept in memory: each space's
/// starts at 0 and goes up by 1 on every successful write by this store.
#[derive(Default)]
pub struct FsFileStore {
    revisions: Mutex<BTreeMap<SpaceId, u64>>,
}

impl FsFileStore {
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

fn file_path(space: &SpaceId, path: &str) -> Result<PathBuf, StoreError> {
    if !valid_path(path) {
        return Err(StoreError::InvalidPath);
    }
    let folder = Path::new(&space.0);
    if !folder.is_absolute() {
        return Err(StoreError::Unavailable(format!(
            "space {} is not an absolute folder",
            space.0
        )));
    }
    Ok(folder.join(".workspace").join(path))
}

fn unavailable(error: io::Error) -> StoreError {
    StoreError::Unavailable(error.to_string())
}

#[async_trait]
impl FileStore for FsFileStore {
    async fn read(&self, space: &SpaceId, path: &str) -> Result<Option<StoredFile>, StoreError> {
        let file = file_path(space, path)?;
        let revisions = self
            .revisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let bytes = match std::fs::read(&file) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(unavailable(error)),
        };
        Ok(Some(StoredFile {
            bytes,
            revision: StoreRevision(revisions.get(space).copied().unwrap_or(0)),
        }))
    }

    async fn write(
        &self,
        space: &SpaceId,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<StoreRevision, StoreError> {
        let file = file_path(space, path)?;
        let mut revisions = self
            .revisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(unavailable)?;
        }
        write_atomic(&file, &bytes, 0o644).map_err(unavailable)?;
        let revision = revisions.entry(space.clone()).or_insert(0);
        *revision += 1;
        Ok(StoreRevision(*revision))
    }

    async fn revision(&self, space: &SpaceId) -> Result<StoreRevision, StoreError> {
        let revisions = self
            .revisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Ok(StoreRevision(revisions.get(space).copied().unwrap_or(0)))
    }
}
