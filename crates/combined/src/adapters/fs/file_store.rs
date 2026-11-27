//! The per-space file store on disk, at `<space>/.workspace/<path>`.
//!
//! Panel reads use `read`, which looks at the fallback copy first; `list_dir` lists only the
//! project's folder.
//!
//! A pulled repository can put a symlink at `.workspace`, at any directory under it or at the
//! file itself, so the file operations (`read`, `write`, `write_project`, `remove_file`,
//! `remove_dir` and `list_dir`) never follow one there, and refuse it, dangling or not. Each
//! walks the path by descriptor: `<space>/.workspace` is opened as a directory with
//! `O_NOFOLLOW | O_DIRECTORY`, each folder below it is opened (and, for a write, created with
//! `mkdirat`) relative to the descriptor of the one above it, the same way, and the file is
//! checked with `fstatat(AT_SYMLINK_NOFOLLOW)` and then read, replaced or removed relative to
//! its parent's descriptor: a read opens it with `O_NOFOLLOW`, a write publishes a temp file
//! with `renameat` (see [`write_atomic_at`]), and a remove `unlinkat`s it.
//!
//! What this holds against a concurrent process, and what it doesn't:
//! - Swapping `.workspace`, a folder or the file for a symlink while a call runs can make the
//!   call fail, but can't make it follow the symlink: after the first open, every step
//!   resolves one name relative to a descriptor already open on a folder, and none follows a
//!   symlink in that name (a rename replaces a symlink at its target name).
//! - A descriptor pins the folder, not its place. If a process moves a folder the call has
//!   already opened, `.workspace` included, out of `.workspace`, the call goes on in that
//!   folder where it now is. Only a process running as the user can do that, and SPEC's
//!   threat model trusts it.
//! - `watch` is not part of this: the watcher works by path, and follows a symlink at the
//!   watch root (`watcher.rs`).
//!
//! The space's own path, above `.workspace`, is followed as given: it is the user's folder, not
//! the repository's. A hard link is an ordinary file in its folder and is read as one; a write
//! replaces the link, never the file it shares. Each folder on the way is opened for reading,
//! so one the user can search but not list counts as denied. The fallback copies are in the
//! daemon's own data folder and are reached by path.
//!
//! When the project can't be written (`PermissionDenied` or `ReadOnlyFilesystem`, whether from
//! the folder check, from creating the folders or from the write itself) and the store has a fallback root, the bytes
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

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read as _};
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::{self, UnboundedReceiver};

use super::safe_write::{write_atomic, write_atomic_at};
use super::watcher::SpaceWatcher;
use crate::model::{ContentHash, SpaceId};
use crate::ports::{
    valid_store_path, DirListing, EntryKind, FileChange, FileStore, ListedEntry, StoreError,
    StoreRevision, StoredFile, WriteOutcome,
};

/// Every folder on the way is opened like this: for reading, as a directory, and never through
/// a symlink.
const FOLDER_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// A file to read: never through a symlink, and without waiting on a FIFO.
const READ_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NONBLOCK)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// 0o777 before the umask, as `std::fs::create_dir` uses.
const FOLDER_MODE: Mode = Mode::RWXU.union(Mode::RWXG).union(Mode::RWXO);

/// 0o644 before the umask.
const FILE_MODE: Mode = Mode::RUSR
    .union(Mode::WUSR)
    .union(Mode::RGRP)
    .union(Mode::ROTH);

const SYMLINK: &str = "is a symlink; the file store does not follow it";

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
        Some(root.join(space.sha256_hex()).join(path))
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

/// Why a walk under `.workspace` gave up: the store refused the path, or the project denied
/// the access.
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

    /// The mapping of the calls with no fallback: a denial is `ReadOnly`.
    fn into_read_only(self) -> StoreError {
        match self {
            Checked::Refused(error) => error,
            Checked::Denied(error) => StoreError::ReadOnly(error.to_string()),
        }
    }
}

impl From<StoreError> for Checked {
    fn from(error: StoreError) -> Checked {
        Checked::Refused(error)
    }
}

/// A failed step: denied when the project can't be written or read there.
fn created(error: io::Error) -> Checked {
    if is_denied(&error) {
        Checked::Denied(error)
    } else {
        Checked::Refused(unavailable(error))
    }
}

/// A folder under `<space>/.workspace` (or `.workspace` itself), open by descriptor. `path`
/// names it in messages only; nothing is resolved through it.
struct Folder {
    dir: File,
    path: PathBuf,
}

impl Folder {
    /// Opens `<space>/.workspace` and each folder in `folders` below it, each relative to the
    /// one above with [`FOLDER_FLAGS`], and returns the last. `None` means one is missing,
    /// which only a call without `create` reaches. With `create`, the space's folder is made
    /// as needed and each missing folder from `.workspace` down is made with `mkdirat` in its
    /// parent. A symlink or a non-directory at any of them is refused.
    fn walk(space: &SpaceId, folders: &[&str], create: bool) -> Result<Option<Folder>, Checked> {
        let folder = Path::new(&space.0);
        if !folder.is_absolute() {
            return Err(StoreError::Unavailable(format!(
                "space {} is not an absolute folder",
                space.0
            ))
            .into());
        }
        if create {
            std::fs::create_dir_all(folder).map_err(created)?;
        }
        let workspace = folder.join(".workspace");
        // Absolute, so the walk starts from the space's path; only `.workspace` itself must not
        // be a symlink.
        let Some(mut dir) = open_folder(rustix::fs::CWD, &workspace, workspace.clone(), create)?
        else {
            return Ok(None);
        };
        for segment in folders {
            let path = dir.path.join(segment);
            match open_folder(dir.dir.as_fd(), Path::new(segment), path, create)? {
                Some(next) => dir = next,
                None => return Ok(None),
            }
        }
        Ok(Some(dir))
    }

    /// The type of `name` in this folder, without following a symlink. `None` means it is
    /// missing; a symlink there is refused.
    fn entry(&self, name: &str) -> Result<Option<FileType>, Checked> {
        match rustix::fs::statat(&self.dir, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
                FileType::Symlink => Err(refused(&self.path.join(name), SYMLINK).into()),
                kind => Ok(Some(kind)),
            },
            Err(Errno::NOENT) => Ok(None),
            Err(errno) => Err(created(errno.into())),
        }
    }
}

/// Opens `name` in `parent` as a folder, refusing a symlink; with `create`, makes it first
/// if it is missing. `path` names it in messages. `None` means it is missing.
fn open_folder(
    parent: BorrowedFd<'_>,
    name: &Path,
    path: PathBuf,
    create: bool,
) -> Result<Option<Folder>, Checked> {
    if create {
        match rustix::fs::mkdirat(parent, name, FOLDER_MODE) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(errno) => return Err(created(errno.into())),
        }
    }
    match rustix::fs::openat(parent, name, FOLDER_FLAGS, Mode::empty()) {
        Ok(fd) => Ok(Some(Folder {
            dir: File::from(fd),
            path,
        })),
        Err(Errno::NOENT) => Ok(None),
        // `O_NOFOLLOW | O_DIRECTORY` on a symlink is `ELOOP` on some systems and `ENOTDIR` on
        // others: look at the entry, only to say which refusal it is.
        Err(errno @ (Errno::LOOP | Errno::NOTDIR)) => {
            match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
                .map(|stat| FileType::from_raw_mode(stat.st_mode))
            {
                Ok(FileType::Symlink) => Err(refused(&path, SYMLINK).into()),
                Ok(FileType::Directory) | Err(_) => Err(created(errno.into())),
                Ok(_) => Err(refused(&path, "is not a directory").into()),
            }
        }
        Err(errno) => Err(created(errno.into())),
    }
}

/// Opens the folders down to the file's parent with [`Folder::walk`] and returns the parent
/// with the file's name in it. `None` means a folder is missing.
fn walk_to_file<'p>(
    space: &SpaceId,
    path: &'p str,
    create: bool,
) -> Result<Option<(Folder, &'p str)>, Checked> {
    file_path(space, path)?;
    let segments: Vec<&str> = path.split('/').collect();
    let (name, folders) = segments.split_last().ok_or(StoreError::InvalidPath)?;
    Ok(Folder::walk(space, folders, create)?.map(|parent| (parent, *name)))
}

/// Replaces `.workspace/<path>` atomically, making its folders as needed. A symlink at the
/// file is refused, and the write never reaches the fallback.
fn write_in_project(space: &SpaceId, path: &str, bytes: &[u8]) -> Result<(), Checked> {
    let Some((parent, name)) = walk_to_file(space, path, true)? else {
        return Err(StoreError::Unavailable("the folder could not be created".to_string()).into());
    };
    parent.entry(name)?;
    write_atomic_at(&parent.dir, name, bytes, FILE_MODE).map_err(created)
}

/// Reads `.workspace/<path>`, bounded like [`read_bounded`]. `None` means it, or a folder on
/// the way, is missing.
fn read_in_project(
    space: &SpaceId,
    path: &str,
    max_bytes: usize,
) -> Result<Option<Bounded>, StoreError> {
    let Some((parent, name)) = walk_to_file(space, path, false).map_err(Checked::into_store)?
    else {
        return Ok(None);
    };
    match parent.entry(name).map_err(Checked::into_store)? {
        None => return Ok(None),
        Some(FileType::RegularFile) => {}
        Some(_) => return Ok(Some(Bounded::NotRegular)),
    }
    let file = match rustix::fs::openat(&parent.dir, name, READ_FLAGS, Mode::empty()) {
        Ok(fd) => File::from(fd),
        Err(Errno::NOENT) => return Ok(None),
        // Swapped for a symlink after the check.
        Err(Errno::LOOP) => return Err(refused(&parent.path.join(name), SYMLINK)),
        Err(errno) => return Err(unavailable(errno.into())),
    };
    read_open(file, max_bytes).map(Some).map_err(unavailable)
}

/// Lists the entries of `dir`, skipping names that aren't UTF-8. Stops after `max_entries + 1`
/// kept entries, sorts them by name and cuts the list to `max_entries`.
fn read_listing(dir: &Folder, max_entries: usize) -> Result<DirListing, StoreError> {
    let keep = max_entries.saturating_add(1);
    let mut entries = Vec::new();
    let reader = rustix::fs::Dir::read_from(&dir.dir).map_err(|errno| unavailable(errno.into()))?;
    for entry in reader {
        let entry = entry.map_err(|errno| unavailable(errno.into()))?;
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if name == "." || name == ".." {
            continue;
        }
        let file_type = match entry.file_type() {
            // The file system doesn't report types in its listing: ask for this entry's.
            FileType::Unknown => {
                rustix::fs::statat(&dir.dir, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW)
                    .map(|stat| FileType::from_raw_mode(stat.st_mode))
                    .map_err(|errno| unavailable(errno.into()))?
            }
            known => known,
        };
        let kind = match file_type {
            FileType::Symlink => EntryKind::Symlink,
            FileType::Directory => EntryKind::Directory,
            FileType::RegularFile => EntryKind::File,
            _ => EntryKind::Other,
        };
        entries.push(ListedEntry {
            name: name.to_string(),
            kind,
        });
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
pub(super) enum Bounded {
    Bytes(Vec<u8>),
    /// More than the bound: the file's size, as far as is known.
    TooLarge(u64),
    /// Not a regular file: nothing was read.
    NotRegular,
}

/// Reads `path`, but never more than `max_bytes + 1` bytes of it, so a huge file, or one that
/// grows during the read, costs at most that. Only a regular file is read, and neither the
/// open nor the read waits on a FIFO: see the module note.
pub(super) fn read_bounded(path: &Path, max_bytes: usize) -> io::Result<Bounded> {
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Ok(Bounded::NotRegular);
    }
    let file = File::from(rustix::fs::open(path, READ_FLAGS, Mode::empty())?);
    read_open(file, max_bytes)
}

/// The bounded read of a file already opened with `O_NONBLOCK`: its type is checked on the
/// handle before any byte is read.
fn read_open(file: File, max_bytes: usize) -> io::Result<Bounded> {
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
        match read_in_project(space, path, max_bytes)? {
            Some(found) => {
                found_file(found, revision, false, &format!(".workspace/{path}")).map(Some)
            }
            None => Ok(None),
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
            aside = match write_in_project(space, path, &bytes) {
                Ok(()) => None,
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
        write_in_project(space, path, &bytes).map_err(Checked::into_read_only)?;
        let revision = revisions.entry(space.clone()).or_insert(0);
        *revision += 1;
        Ok(WriteOutcome {
            revision: StoreRevision(*revision),
            hash: content_hash(&bytes),
            read_only: false,
        })
    }

    async fn remove_file(&self, space: &SpaceId, path: &str) -> Result<(), StoreError> {
        file_path(space, path)?;
        let walked = walk_to_file(space, path, false).map_err(Checked::into_read_only)?;
        let Some((parent, name)) = walked else {
            return Ok(());
        };
        let Some(_) = parent.entry(name).map_err(Checked::into_read_only)? else {
            return Ok(());
        };
        match rustix::fs::unlinkat(&parent.dir, name, AtFlags::empty()).map_err(io::Error::from) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) if is_denied(&error) => Err(StoreError::ReadOnly(error.to_string())),
            Err(error) => Err(unavailable(error)),
        }
    }

    async fn remove_dir(&self, space: &SpaceId, path: &str) -> Result<(), StoreError> {
        file_path(space, path)?;
        let walked = walk_to_file(space, path, false).map_err(Checked::into_read_only)?;
        let Some((parent, name)) = walked else {
            return Ok(());
        };
        // Checked without opening it, so an empty folder the user can't list still goes; the
        // removal doesn't follow a symlink swapped in after the check.
        match parent.entry(name).map_err(Checked::into_read_only)? {
            None => return Ok(()),
            Some(FileType::Directory) => {}
            Some(_) => return Err(refused(&parent.path.join(name), "is not a directory")),
        }
        let removed = rustix::fs::unlinkat(&parent.dir, name, AtFlags::REMOVEDIR);
        match removed.map_err(io::Error::from) {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                Ok(())
            }
            Err(error) if is_denied(&error) => Err(StoreError::ReadOnly(error.to_string())),
            Err(error) => Err(unavailable(error)),
        }
    }

    async fn list_dir(
        &self,
        space: &SpaceId,
        path: &str,
        max_entries: usize,
    ) -> Result<Option<DirListing>, StoreError> {
        file_path(space, path)?;
        let folders: Vec<&str> = path.split('/').collect();
        let Some(dir) = Folder::walk(space, &folders, false).map_err(Checked::into_store)? else {
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
