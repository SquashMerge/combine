//! The per-space file store on disk, at `<space>/.workspace/<path>`.
//!
//! Panel reads use `read`, which looks at the fallback copy first; `list_dir` lists only the
//! project's folder.
//!
//! A pulled repository can put a symlink at `.workspace`, at any directory under it or at the
//! file itself, so the store refuses every symlink on the way: each component is checked with
//! `symlink_metadata`, the verified directory is canonicalized and must still lie under the
//! canonical `.workspace`, and the read or the atomic write then uses that canonical path.
//!
//! When the project can't be written (`PermissionDenied` or `ReadOnlyFilesystem`, whether from
//! creating the folders or from the write itself) and the store has a fallback root, the bytes
//! go to `<root>/<sha256(space) in hex>/<path>` and the write reports `read_only`. A refusal
//! for a symlink, a path outside `.workspace` or a relative space is not such a failure: it
//! stays `Unavailable` and never reaches the fallback.
//!
//! A read takes only a regular file. A FIFO, device, socket or directory at the file's path,
//! in the project or as the fallback copy, is refused with "is not a regular file" and never
//! read: the daemon reads on its one server loop, and a blocking open or read of a FIFO would
//! stall every client. The file is opened with `O_NONBLOCK`, so even a FIFO swapped in after
//! the type check doesn't block the open, and `O_NOFOLLOW`; its type is then checked again on
//! the opened handle, before any byte is read.
//!
//! The directory checks are not race-free. Std offers no descriptor-relative open without
//! `unsafe`, so a process that can modify the space's folder while a call runs can still swap
//! a directory component for a symlink between the check and the open. The checks close the
//! case of a symlink already present in the working tree, not a concurrent attacker.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{self, Read as _};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::{self, UnboundedReceiver};

use super::safe_write::write_atomic;
use super::watcher::SpaceWatcher;
use crate::model::{ContentHash, SpaceId};
use crate::ports::{
    valid_store_path, DirListing, EntryKind, FileChange, FileStore, ListedEntry, StoreError,
    StoreRevision, StoredFile, WriteOutcome,
};

/// Files under each space's `.workspace/` folder. Revisions are kept in memory: each space's
/// starts at 0 and goes up by 1 on every successful write by this store.
pub struct FsFileStore {
    revisions: Mutex<BTreeMap<SpaceId, u64>>,
    fallback: Option<PathBuf>,
    watcher: Mutex<Option<SpaceWatcher>>,
    changes_tx: mpsc::UnboundedSender<FileChange>,
    changes_rx: Mutex<Option<UnboundedReceiver<FileChange>>>,
}

impl Default for FsFileStore {
    fn default() -> Self {
        let (changes_tx, changes_rx) = mpsc::unbounded_channel();
        FsFileStore {
            revisions: Mutex::new(BTreeMap::new()),
            fallback: None,
            watcher: Mutex::new(None),
            changes_tx,
            changes_rx: Mutex::new(Some(changes_rx)),
        }
    }
}

impl FsFileStore {
    /// A store without a fallback: a project that can't be written gives `ReadOnly`.
    pub fn new() -> Self {
        Self::default()
    }

    /// A store that writes to `<root>/<hash of the space>/<path>` when the project can't be
    /// written.
    pub fn with_fallback(root: impl Into<PathBuf>) -> Self {
        FsFileStore {
            fallback: Some(root.into()),
            ..Self::default()
        }
    }

    fn fallback_file(&self, space: &SpaceId, path: &str) -> Option<PathBuf> {
        if !valid_store_path(path) {
            return None;
        }
        let root = self.fallback.as_ref()?;
        let digest = Sha256::digest(space.0.as_bytes());
        let mut name = String::with_capacity(64);
        for byte in digest {
            let _ = write!(name, "{byte:02x}");
        }
        Some(root.join(name).join(path))
    }
}

fn content_hash(bytes: &[u8]) -> ContentHash {
    ContentHash(Sha256::digest(bytes).into())
}

fn file_path(space: &SpaceId, path: &str) -> Result<PathBuf, StoreError> {
    if !valid_store_path(path) {
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

/// The project can't be written, as opposed to being refused or broken.
fn is_denied(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
    )
}

/// Why [`checked_folder`] gave up: the store refused the path, or creating a folder was denied.
enum Checked {
    Refused(StoreError),
    Denied(io::Error),
}

impl Checked {
    fn into_store(self) -> StoreError {
        match self {
            Checked::Refused(error) => error,
            Checked::Denied(error) => unavailable(error),
        }
    }
}

impl From<StoreError> for Checked {
    fn from(error: StoreError) -> Checked {
        Checked::Refused(error)
    }
}

/// A failure to create a folder: denied when the project can't be written there.
fn created(error: io::Error) -> Checked {
    if is_denied(&error) {
        Checked::Denied(error)
    } else {
        Checked::Refused(unavailable(error))
    }
}

/// Verifies `<space>/.workspace` and every directory in `folders` below it, and returns the
/// canonical path of the last one. `None` means a component is missing, which only a call
/// without `create` reaches. A symlink or non-directory at any component is refused, and the
/// canonical folder must still lie under the canonical `.workspace`.
///
/// The checks and the later open are separate steps: see the module note on the race.
fn checked_folder(
    space: &SpaceId,
    folders: &[&str],
    create: bool,
) -> Result<Option<PathBuf>, Checked> {
    let folder = Path::new(&space.0);
    if !folder.is_absolute() {
        return Err(StoreError::Unavailable(format!(
            "space {} is not an absolute folder",
            space.0
        ))
        .into());
    }
    let workspace = folder.join(".workspace");
    let mut dir = workspace.clone();
    for step in std::iter::once(None).chain(folders.iter().map(Some)) {
        match step {
            None if create => std::fs::create_dir_all(&dir).map_err(created)?,
            None => {}
            Some(segment) => {
                dir.push(segment);
                if create {
                    match std::fs::create_dir(&dir) {
                        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => {
                            return Err(created(error))
                        }
                        _ => {}
                    }
                }
            }
        }
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(refused(&dir, "is a symlink; the file store does not follow it").into())
            }
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(refused(&dir, "is not a directory").into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(unavailable(error).into()),
        }
    }
    let root = std::fs::canonicalize(&workspace).map_err(unavailable)?;
    let canonical = std::fs::canonicalize(&dir).map_err(unavailable)?;
    if !canonical.starts_with(&root) {
        return Err(refused(&dir, "resolves outside .workspace").into());
    }
    Ok(Some(canonical))
}

/// Verifies the folders down to the file's parent with [`checked_folder`] and returns the
/// file's path inside the canonical parent. `None` means a folder is missing. A symlink at the
/// file itself is refused too.
fn checked_file(space: &SpaceId, path: &str, create: bool) -> Result<Option<PathBuf>, Checked> {
    file_path(space, path)?;
    let segments: Vec<&str> = path.split('/').collect();
    let (name, folders) = segments.split_last().ok_or(StoreError::InvalidPath)?;
    let Some(parent) = checked_folder(space, folders, create)? else {
        return Ok(None);
    };
    let file = parent.join(name);
    match std::fs::symlink_metadata(&file) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(refused(&file, "is a symlink; the file store does not follow it").into())
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(unavailable(error).into()),
    }
    Ok(Some(file))
}

/// Lists the entries of `dir`, skipping names that aren't UTF-8. Stops after `max_entries + 1`
/// kept entries, sorts them by name and cuts the list to `max_entries`.
fn read_listing(dir: &Path, max_entries: usize) -> Result<DirListing, StoreError> {
    let keep = max_entries.saturating_add(1);
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(unavailable)? {
        let entry = entry.map_err(unavailable)?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let file_type = entry.file_type().map_err(unavailable)?;
        let kind = if file_type.is_symlink() {
            EntryKind::Symlink
        } else if file_type.is_dir() {
            EntryKind::Directory
        } else if file_type.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        entries.push(ListedEntry { name, kind });
        if entries.len() >= keep {
            break;
        }
    }
    entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    let truncated = entries.len() > max_entries;
    entries.truncate(max_entries);
    Ok(DirListing { entries, truncated })
}

/// What a bounded read found.
enum Bounded {
    Bytes(Vec<u8>),
    /// More than the bound: the file's size, as far as is known.
    TooLarge(u64),
    /// Not a regular file: nothing was read.
    NotRegular,
}

/// Reads `path`, but never more than `max_bytes + 1` bytes of it, so a huge file, or one that
/// grows during the read, costs at most that. Only a regular file is read, and neither the
/// open nor the read waits on a FIFO: see the module note.
fn read_bounded(path: &Path, max_bytes: usize) -> io::Result<Bounded> {
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Ok(Bounded::NotRegular);
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        return Ok(Bounded::NotRegular);
    }
    let len = meta.len();
    let cap = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::with_capacity(usize::try_from(len.min(cap)).unwrap_or(0));
    file.take(cap).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        let read = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        return Ok(Bounded::TooLarge(len.max(read)));
    }
    Ok(Bounded::Bytes(bytes))
}

#[async_trait]
impl FileStore for FsFileStore {
    async fn read(
        &self,
        space: &SpaceId,
        path: &str,
        max_bytes: usize,
    ) -> Result<Option<StoredFile>, StoreError> {
        let revisions = self
            .revisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let revision = StoreRevision(revisions.get(space).copied().unwrap_or(0));
        file_path(space, path)?;
        if let Some(copy) = self.fallback_file(space, path) {
            match read_bounded(&copy, max_bytes) {
                Ok(found) => {
                    let what = format!("the fallback copy of {path}");
                    return found_file(found, revision, true, &what).map(Some);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(unavailable(error)),
            }
        }
        let Some(file) = checked_file(space, path, false).map_err(Checked::into_store)? else {
            return Ok(None);
        };
        match read_bounded(&file, max_bytes) {
            Ok(found) => {
                found_file(found, revision, false, &format!(".workspace/{path}")).map(Some)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(unavailable(error)),
        }
    }

    async fn write(
        &self,
        space: &SpaceId,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<WriteOutcome, StoreError> {
        let mut revisions = self
            .revisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        file_path(space, path)?;
        let copy = self.fallback_file(space, path);
        let mut aside = copy
            .as_ref()
            .filter(|copy| copy.is_file())
            .map(|_| "the fallback copy is authoritative".to_string());
        if aside.is_none() {
            aside = match checked_file(space, path, true) {
                Ok(Some(file)) => match write_atomic(&file, &bytes, 0o644) {
                    Ok(()) => None,
                    Err(error) if is_denied(&error) => Some(error.to_string()),
                    Err(error) => return Err(unavailable(error)),
                },
                Ok(None) => {
                    return Err(StoreError::Unavailable(
                        "the folder could not be created".to_string(),
                    ))
                }
                Err(Checked::Denied(error)) => Some(error.to_string()),
                Err(Checked::Refused(error)) => return Err(error),
            };
        }
        if let Some(detail) = &aside {
            let Some(copy) = copy else {
                return Err(StoreError::ReadOnly(detail.clone()));
            };
            if let Some(parent) = copy.parent() {
                std::fs::create_dir_all(parent).map_err(unavailable)?;
            }
            write_atomic(&copy, &bytes, 0o644).map_err(unavailable)?;
        }
        let revision = revisions.entry(space.clone()).or_insert(0);
        *revision += 1;
        Ok(WriteOutcome {
            revision: StoreRevision(*revision),
            hash: content_hash(&bytes),
            read_only: aside.is_some(),
        })
    }

    async fn write_project(
        &self,
        space: &SpaceId,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<WriteOutcome, StoreError> {
        let mut revisions = self
            .revisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        file_path(space, path)?;
        match checked_file(space, path, true) {
            Ok(Some(file)) => match write_atomic(&file, &bytes, 0o644) {
                Ok(()) => {}
                Err(error) if is_denied(&error) => {
                    return Err(StoreError::ReadOnly(error.to_string()))
                }
                Err(error) => return Err(unavailable(error)),
            },
            Ok(None) => {
                return Err(StoreError::Unavailable(
                    "the folder could not be created".to_string(),
                ))
            }
            Err(Checked::Denied(error)) => return Err(StoreError::ReadOnly(error.to_string())),
            Err(Checked::Refused(error)) => return Err(error),
        }
        let revision = revisions.entry(space.clone()).or_insert(0);
        *revision += 1;
        Ok(WriteOutcome {
            revision: StoreRevision(*revision),
            hash: content_hash(&bytes),
            read_only: false,
        })
    }

    async fn list_dir(
        &self,
        space: &SpaceId,
        path: &str,
        max_entries: usize,
    ) -> Result<Option<DirListing>, StoreError> {
        file_path(space, path)?;
        let folders: Vec<&str> = path.split('/').collect();
        let Some(dir) = checked_folder(space, &folders, false).map_err(Checked::into_store)? else {
            return Ok(None);
        };
        read_listing(&dir, max_entries).map(Some)
    }

    async fn revision(&self, space: &SpaceId) -> Result<StoreRevision, StoreError> {
        let revisions = self
            .revisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Ok(StoreRevision(revisions.get(space).copied().unwrap_or(0)))
    }

    async fn watch(&self, space: &SpaceId) -> Result<(), StoreError> {
        let mut slot = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            let watcher = SpaceWatcher::new(self.changes_tx.clone())
                .map_err(|error| StoreError::Unavailable(error.to_string()))?;
            *slot = Some(watcher);
        }
        match slot.as_ref() {
            Some(watcher) => watcher
                .watch(space)
                .map_err(|error| StoreError::Unavailable(error.to_string())),
            None => Ok(()),
        }
    }

    fn changes(&self) -> BoxStream<'static, FileChange> {
        let receiver = self
            .changes_rx
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

/// `what` names the file in the refusal of one that isn't regular.
fn found_file(
    found: Bounded,
    revision: StoreRevision,
    read_only: bool,
    what: &str,
) -> Result<StoredFile, StoreError> {
    match found {
        Bounded::Bytes(bytes) => Ok(stored(bytes, revision, read_only)),
        Bounded::TooLarge(size) => Err(StoreError::TooLarge { size, read_only }),
        Bounded::NotRegular => Err(StoreError::Unavailable(format!(
            "{what} is not a regular file"
        ))),
    }
}

fn stored(bytes: Vec<u8>, revision: StoreRevision, read_only: bool) -> StoredFile {
    StoredFile {
        hash: content_hash(&bytes),
        bytes,
        revision,
        read_only,
    }
}
