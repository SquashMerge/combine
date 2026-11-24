//! The scratch panels' folders on disk, at `<root>/<key>/<panel>/<file>`.
//!
//! The root is the data directory's `scratch/` folder. It is the daemon's own area, outside
//! every project and not watched, so there is no fallback copy and no change stream. Folders
//! are created with mode 0700 and files with 0600.
//!
//! Every component (the root, the key folder, the panel folder and the file) is checked with
//! `symlink_metadata`, and a symlink is refused, never followed. As in the file store, the
//! checks and the later open are separate steps, so they close the case of a symlink already
//! in place, not a concurrent attacker.

use std::fs::DirBuilder;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use super::file_store::{read_bounded, Bounded};
use super::safe_write::write_atomic;
use crate::model::ContentHash;
use crate::ports::{
    ScratchDir, ScratchListing, ScratchStore, StoreError, StoreRevision, StoredFile, WriteOutcome,
};

const PANEL_FILE: &str = "panel.json";
const MERMAID_FILE: &str = "diagram.mmd";
const MARKDOWN_FILE: &str = "content.md";
const SCRATCH_FILE: &str = "scratch.json";

/// Scratch panel folders under one root. The revision is one counter for the whole store: it
/// goes up by 1 on every successful write.
pub struct FsScratchStore {
    root: PathBuf,
    revision: AtomicU64,
}

impl FsScratchStore {
    pub fn new(root: PathBuf) -> Self {
        FsScratchStore {
            root,
            revision: AtomicU64::new(0),
        }
    }

    fn revision(&self) -> StoreRevision {
        StoreRevision(self.revision.load(Ordering::SeqCst))
    }
}

fn valid_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn valid_panel(panel: &str) -> bool {
    !panel.is_empty() && !panel.contains('/') && !panel.starts_with('.')
}

fn valid_file(file: &str) -> bool {
    matches!(
        file,
        PANEL_FILE | MERMAID_FILE | MARKDOWN_FILE | SCRATCH_FILE
    )
}

fn valid_dir(dir: &ScratchDir) -> Result<(), StoreError> {
    if valid_key(&dir.key) && valid_panel(&dir.panel) {
        Ok(())
    } else {
        Err(StoreError::InvalidPath)
    }
}

fn unavailable(error: io::Error) -> StoreError {
    StoreError::Unavailable(error.to_string())
}

fn refused(what: &Path, why: &str) -> StoreError {
    StoreError::Unavailable(format!("{} {why}", what.display()))
}

/// Whether `path` is a real directory. A missing one is `false`; a symlink or anything else
/// that isn't a directory is refused.
fn real_dir(path: &Path) -> Result<bool, StoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(refused(
            path,
            "is a symlink; the scratch store does not follow it",
        )),
        Ok(meta) if meta.is_dir() => Ok(true),
        Ok(_) => Err(refused(path, "is not a directory")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(unavailable(error)),
    }
}

/// Checks the root, the key folder and the panel folder of `dir`, and returns the panel
/// folder, or `None` when a folder is missing.
fn checked_dir(root: &Path, dir: &ScratchDir) -> Result<Option<(PathBuf, PathBuf)>, StoreError> {
    if !real_dir(root)? {
        return Ok(None);
    }
    let key = root.join(&dir.key);
    if !real_dir(&key)? {
        return Ok(None);
    }
    let panel = key.join(&dir.panel);
    if !real_dir(&panel)? {
        return Ok(None);
    }
    Ok(Some((key, panel)))
}

/// The real folders in `path`, with UTF-8 names. A missing `path` has none.
fn child_dirs(path: &Path) -> Result<Vec<String>, StoreError> {
    if !real_dir(path)? {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(path).map_err(unavailable)? {
        let entry = entry.map_err(unavailable)?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if entry.file_type().map_err(unavailable)?.is_dir() {
            names.push(name);
        }
    }
    Ok(names)
}

fn create_folder(path: &Path) -> Result<(), StoreError> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(unavailable(error)),
    }
}

#[async_trait]
impl ScratchStore for FsScratchStore {
    async fn list(&self, max_dirs: usize) -> Result<ScratchListing, StoreError> {
        let mut dirs = Vec::new();
        for key in child_dirs(&self.root)? {
            let folder = self.root.join(&key);
            for panel in child_dirs(&folder)? {
                dirs.push(ScratchDir {
                    key: key.clone(),
                    panel,
                });
            }
        }
        dirs.sort();
        let truncated = dirs.len() > max_dirs;
        dirs.truncate(max_dirs);
        Ok(ScratchListing { dirs, truncated })
    }

    async fn read(
        &self,
        dir: &ScratchDir,
        file: &str,
        max_bytes: usize,
    ) -> Result<Option<StoredFile>, StoreError> {
        valid_dir(dir)?;
        if !valid_file(file) {
            return Err(StoreError::InvalidPath);
        }
        let Some((_, panel)) = checked_dir(&self.root, dir)? else {
            return Ok(None);
        };
        let path = panel.join(file);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(refused(
                    &path,
                    "is a symlink; the scratch store does not follow it",
                ))
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(unavailable(error)),
        }
        let revision = self.revision();
        match read_bounded(&path, max_bytes) {
            Ok(Bounded::Bytes(bytes)) => Ok(Some(StoredFile {
                hash: content_hash(&bytes),
                bytes,
                revision,
                read_only: false,
            })),
            Ok(Bounded::TooLarge(size)) => Err(StoreError::TooLarge {
                size,
                read_only: false,
            }),
            Ok(Bounded::NotRegular) => Err(refused(&path, "is not a regular file")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(unavailable(error)),
        }
    }

    async fn write(
        &self,
        dir: &ScratchDir,
        file: &str,
        bytes: Vec<u8>,
    ) -> Result<WriteOutcome, StoreError> {
        valid_dir(dir)?;
        if !valid_file(file) {
            return Err(StoreError::InvalidPath);
        }
        let key = self.root.join(&dir.key);
        let panel = key.join(&dir.panel);
        if !real_dir(&self.root)? {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.root)
                .map_err(unavailable)?;
        }
        for folder in [&key, &panel] {
            if !real_dir(folder)? {
                create_folder(folder)?;
            }
        }
        for folder in [self.root.as_path(), &key, &panel] {
            real_dir(folder)?;
        }
        let path = panel.join(file);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(refused(
                    &path,
                    "is a symlink; the scratch store does not follow it",
                ))
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(unavailable(error)),
        }
        write_atomic(&path, &bytes, 0o600).map_err(unavailable)?;
        let revision = StoreRevision(self.revision.fetch_add(1, Ordering::SeqCst) + 1);
        Ok(WriteOutcome {
            revision,
            hash: content_hash(&bytes),
            read_only: false,
        })
    }

    async fn remove(&self, dir: &ScratchDir) -> Result<(), StoreError> {
        valid_dir(dir)?;
        let Some((key, panel)) = checked_dir(&self.root, dir)? else {
            return Ok(());
        };
        match std::fs::remove_dir_all(&panel) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(unavailable(error)),
        }
        let _ = std::fs::remove_dir(&key);
        Ok(())
    }
}

fn content_hash(bytes: &[u8]) -> ContentHash {
    ContentHash(Sha256::digest(bytes).into())
}
