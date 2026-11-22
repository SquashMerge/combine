//! The per-space file store on disk, at `<space>/.workspace/<path>`.
//!
//! A pulled repository can put a symlink at `.workspace`, at any directory under it or at the
//! file itself, so the store refuses every symlink on the way: each component is checked with
//! `symlink_metadata`, the verified directory is canonicalized and must still lie under the
//! canonical `.workspace`, and the read or the atomic write then uses that canonical path.
//!
//! This is not race-free. Std offers no descriptor-relative or `O_NOFOLLOW` open without
//! `unsafe` or a new dependency, so a process that can modify the space's folder while a call
//! runs can still swap a component for a symlink between the check and the open. The checks
//! close the case of a symlink already present in the working tree, not a concurrent attacker.

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

fn refused(what: &Path, why: &str) -> StoreError {
    StoreError::Unavailable(format!("{} {why}", what.display()))
}

/// Verifies `<space>/.workspace` and every directory down to the file's parent, and returns the
/// file's path inside the canonical parent. `None` means a component is missing, which only a
/// read without `create` reaches. A symlink at any component, including the file, is refused.
///
/// The checks and the later open are separate steps: see the module note on the race.
fn checked_file(space: &SpaceId, path: &str, create: bool) -> Result<Option<PathBuf>, StoreError> {
    file_path(space, path)?;
    let workspace = Path::new(&space.0).join(".workspace");
    let segments: Vec<&str> = path.split('/').collect();
    let (name, folders) = segments.split_last().ok_or(StoreError::InvalidPath)?;
    let mut dir = workspace.clone();
    for step in std::iter::once(None).chain(folders.iter().map(Some)) {
        match step {
            None if create => std::fs::create_dir_all(&dir).map_err(unavailable)?,
            None => {}
            Some(segment) => {
                dir.push(segment);
                if create {
                    match std::fs::create_dir(&dir) {
                        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => {
                            return Err(unavailable(error))
                        }
                        _ => {}
                    }
                }
            }
        }
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(refused(
                    &dir,
                    "is a symlink; the file store does not follow it",
                ))
            }
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(refused(&dir, "is not a directory")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(unavailable(error)),
        }
    }
    let root = std::fs::canonicalize(&workspace).map_err(unavailable)?;
    let parent = std::fs::canonicalize(&dir).map_err(unavailable)?;
    if !parent.starts_with(&root) {
        return Err(refused(&dir, "resolves outside .workspace"));
    }
    let file = parent.join(name);
    match std::fs::symlink_metadata(&file) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(refused(
                &file,
                "is a symlink; the file store does not follow it",
            ))
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(unavailable(error)),
    }
    Ok(Some(file))
}

#[async_trait]
impl FileStore for FsFileStore {
    async fn read(&self, space: &SpaceId, path: &str) -> Result<Option<StoredFile>, StoreError> {
        let revisions = self
            .revisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(file) = checked_file(space, path, false)? else {
            return Ok(None);
        };
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
        let mut revisions = self
            .revisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let file = checked_file(space, path, true)?.ok_or_else(|| {
            StoreError::Unavailable("the folder could not be created".to_string())
        })?;
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
