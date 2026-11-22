//! One client state file: load, and safe writes that never replace an invalid or changed file.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::model::{ClientStateFile, LastSpace, SpaceState, VERSION};

const MAX_FILE_BYTES: u64 = 1 << 20;
const MAX_TEMP_ATTEMPTS: u64 = 16;
const MAX_LOCK_ATTEMPTS: u32 = 8;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    /// The file failed to load, so every write is refused.
    Invalid,
    /// The file is not what Combine loaded or last wrote.
    ChangedOnDisk,
    Io(String),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateError::Invalid => {
                f.write_str("the client state file is invalid, so Combine did not change it")
            }
            StateError::ChangedOnDisk => f.write_str(
                "the client state file changed on disk, so Combine did not overwrite it",
            ),
            StateError::Io(e) => write!(f, "could not write the client state file: {e}"),
        }
    }
}

impl std::error::Error for StateError {}

/// What a load found. `Invalid` carries the text to show; the store then uses defaults and refuses writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    Missing,
    Loaded,
    Invalid(String),
}

enum Status {
    Invalid,
    Loaded { bytes: Vec<u8> },
}

struct Inner {
    status: Status,
    file: ClientStateFile,
}

pub struct ClientStateStore {
    path: PathBuf,
    inner: Mutex<Inner>,
}

fn parse(bytes: &[u8]) -> Result<ClientStateFile, String> {
    let file: ClientStateFile = serde_json::from_slice(bytes)
        .map_err(|e| format!("the client state file is invalid: {e}"))?;
    if file.version != VERSION {
        return Err(format!(
            "the client state file has version {}, but this Combine reads version {VERSION}",
            file.version
        ));
    }
    Ok(file)
}

fn read_current(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("the client state file could not be read: {e}")),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("the client state file could not be read: {e}"))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("the client state file is too large".to_string());
    }
    Ok(Some(bytes))
}

impl ClientStateStore {
    /// A missing file means defaults. An invalid or unreadable one also gives defaults, and the
    /// store then refuses every write.
    pub fn load(path: PathBuf) -> (ClientStateStore, LoadOutcome) {
        let (inner, outcome) = match read_current(&path) {
            Ok(None) => (
                Inner {
                    status: Status::Loaded { bytes: Vec::new() },
                    file: ClientStateFile::default(),
                },
                LoadOutcome::Missing,
            ),
            Ok(Some(bytes)) => match parse(&bytes) {
                Ok(file) => (
                    Inner {
                        status: Status::Loaded { bytes },
                        file,
                    },
                    LoadOutcome::Loaded,
                ),
                Err(message) => (invalid_inner(), LoadOutcome::Invalid(message)),
            },
            Err(message) => (invalid_inner(), LoadOutcome::Invalid(message)),
        };
        let store = ClientStateStore {
            path,
            inner: Mutex::new(inner),
        };
        (store, outcome)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn snapshot(&self) -> ClientStateFile {
        self.lock().file.clone()
    }

    pub fn set_space_state(&self, key: &str, state: SpaceState) -> Result<(), StateError> {
        self.update(|file| {
            file.spaces.insert(key.to_string(), state);
        })
    }

    pub fn set_last_space(&self, last: LastSpace) -> Result<(), StateError> {
        self.update(|file| file.last_space = Some(last))
    }

    fn update(&self, change: impl FnOnce(&mut ClientStateFile)) -> Result<(), StateError> {
        let mut inner = self.lock();
        let Status::Loaded { bytes: loaded } = &inner.status else {
            return Err(StateError::Invalid);
        };
        let mut next = inner.file.clone();
        change(&mut next);
        let mut bytes =
            serde_json::to_vec_pretty(&next).map_err(|e| StateError::Io(e.to_string()))?;
        bytes.push(b'\n');
        self.write_checked(loaded, &bytes)?;
        inner.status = Status::Loaded { bytes };
        inner.file = next;
        Ok(())
    }

    /// Locks the file, refuses if it differs from `loaded`, then replaces it with `bytes`.
    fn write_checked(&self, loaded: &[u8], bytes: &[u8]) -> Result<(), StateError> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(|e| StateError::Io(e.to_string()))?;
        }
        let lock = self.lock_file()?;
        let mut current = Vec::new();
        if let Some(mut file) = lock.as_ref() {
            file.read_to_end(&mut current)
                .map_err(|e| StateError::Io(e.to_string()))?;
        }
        if current != loaded {
            return Err(StateError::ChangedOnDisk);
        }
        let first = NEXT_TEMP.fetch_add(MAX_TEMP_ATTEMPTS, Ordering::Relaxed);
        self.write_safely_from(bytes, first)
    }

    /// Takes an exclusive advisory lock on the file itself, so concurrent stores and processes
    /// serialize their whole read-compare-write. `None` means it does not exist. A writer renames
    /// a new file over the old one, so after locking, the handle must still be the file at the
    /// path (same device and inode); otherwise it is released and the lock is retaken.
    fn lock_file(&self) -> Result<Option<fs::File>, StateError> {
        for _ in 0..MAX_LOCK_ATTEMPTS {
            let file = match fs::File::open(&self.path) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(StateError::Io(e.to_string())),
            };
            file.lock().map_err(|e| StateError::Io(e.to_string()))?;
            let held = file.metadata().map_err(|e| StateError::Io(e.to_string()))?;
            match fs::metadata(&self.path) {
                Ok(at_path) if (at_path.dev(), at_path.ino()) == (held.dev(), held.ino()) => {
                    return Ok(Some(file))
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(StateError::Io(e.to_string())),
            }
        }
        Err(StateError::Io(
            "the client state file kept being replaced while locking it".to_string(),
        ))
    }

    fn temp_path(&self, n: u64) -> PathBuf {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.path
            .with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()))
    }

    /// Writes `bytes` to a new, uniquely named temp file next to the state file (`O_EXCL`, mode
    /// 0600, so an existing entry at a candidate name is skipped and never followed), syncs it,
    /// renames it over the state file, then syncs the directory.
    fn write_safely_from(&self, bytes: &[u8], first: u64) -> Result<(), StateError> {
        for n in first..first + MAX_TEMP_ATTEMPTS {
            let temp = self.temp_path(n);
            let mut file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)
            {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(StateError::Io(e.to_string())),
            };
            let result = file
                .write_all(bytes)
                .and_then(|()| file.sync_all())
                .and_then(|()| {
                    drop(file);
                    fs::rename(&temp, &self.path)
                })
                .and_then(|()| self.sync_dir());
            return result.map_err(|e| {
                let _ = fs::remove_file(&temp);
                StateError::Io(e.to_string())
            });
        }
        Err(StateError::Io(
            "no free temporary file name next to the client state file".to_string(),
        ))
    }

    fn sync_dir(&self) -> io::Result<()> {
        match self.path.parent() {
            Some(dir) => fs::File::open(dir)?.sync_all(),
            None => Ok(()),
        }
    }
}

fn invalid_inner() -> Inner {
    Inner {
        status: Status::Invalid,
        file: ClientStateFile::default(),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::super::path::client_state_path;
    use super::super::testing::{last_space, space_state, TestDir, ID_A, ID_B};
    use super::*;

    fn store_at(dir: &TestDir, id: &str) -> (ClientStateStore, LoadOutcome) {
        let path = client_state_path(dir.path(), id).expect("valid host id");
        ClientStateStore::load(path)
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .expect("list dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect()
    }

    #[test]
    fn round_trips_space_state_and_last_space() {
        let dir = TestDir::new();
        let (store, _) = store_at(&dir, ID_A);
        store
            .set_space_state("/work/a", space_state("s1"))
            .expect("write space");
        store
            .set_last_space(last_space("s1", 7))
            .expect("write last");
        let (again, outcome) = store_at(&dir, ID_A);
        assert_eq!(outcome, LoadOutcome::Loaded);
        let file = again.snapshot();
        assert_eq!(file.spaces.get("/work/a"), Some(&space_state("s1")));
        assert_eq!(file.last_space, Some(last_space("s1", 7)));
        assert_eq!(file.version, 1);
    }

    #[test]
    fn write_is_temp_and_rename_mode_0600() {
        let dir = TestDir::new();
        let (store, _) = store_at(&dir, ID_A);
        store.set_last_space(last_space("s1", 1)).expect("write");
        let meta = fs::metadata(store.path()).expect("state file");
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        let parent = store.path().parent().expect("parent");
        assert!(leftovers(parent).is_empty());

        let first = NEXT_TEMP.load(Ordering::Relaxed);
        let squatter = store.temp_path(first);
        fs::write(&squatter, b"squatter").expect("occupy temp name");
        store
            .set_last_space(last_space("s2", 2))
            .expect("write again");
        assert_eq!(fs::read(&squatter).expect("squatter"), b"squatter");
        assert_eq!(
            store_at(&dir, ID_A).0.snapshot().last_space,
            Some(last_space("s2", 2))
        );
    }

    #[test]
    fn changed_on_disk_refuses_write() {
        let dir = TestDir::new();
        let (store, _) = store_at(&dir, ID_A);
        store.set_last_space(last_space("s1", 1)).expect("write");
        let other = ClientStateFile {
            last_space: Some(last_space("elsewhere", 9)),
            ..ClientStateFile::default()
        };
        let edited = serde_json::to_vec(&other).expect("serialize");
        fs::write(store.path(), &edited).expect("edit behind the store");
        assert_eq!(
            store.set_last_space(last_space("s2", 2)),
            Err(StateError::ChangedOnDisk)
        );
        assert_eq!(fs::read(store.path()).expect("read"), edited);
        assert_eq!(store.snapshot().last_space, Some(last_space("s1", 1)));
    }

    #[test]
    fn rejects_non_uuid_host_id() {
        let dir = TestDir::new();
        for bad in [
            "",
            "../escape",
            "6A1D3C9E-2F4B-4E8A-B7C5-1D0E9F8A7B6C",
            "6a1d3c9e2f4b4e8ab7c51d0e9f8a7b6c",
            "6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6c/x",
        ] {
            assert!(client_state_path(dir.path(), bad).is_err(), "{bad:?}");
        }
        assert!(client_state_path(dir.path(), ID_A).is_ok());
    }

    #[test]
    fn unknown_field_is_invalid() {
        let dir = TestDir::new();
        let path = client_state_path(dir.path(), ID_A).expect("path");
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(
            &path,
            br#"{"version":1,"last_space":null,"spaces":{},"extra":true}"#,
        )
        .expect("seed");
        let (store, outcome) = ClientStateStore::load(path);
        assert!(matches!(outcome, LoadOutcome::Invalid(_)));
        assert_eq!(store.snapshot(), ClientStateFile::default());
    }

    #[test]
    fn two_clients_are_independent() {
        let dir = TestDir::new();
        let (a, _) = store_at(&dir, ID_A);
        let (b, _) = store_at(&dir, ID_B);
        a.set_last_space(last_space("only-a", 1)).expect("write a");
        b.set_space_state("/p", space_state("only-b"))
            .expect("write b");
        assert_ne!(a.path(), b.path());
        let (a2, _) = store_at(&dir, ID_A);
        let (b2, _) = store_at(&dir, ID_B);
        assert_eq!(a2.snapshot().last_space, Some(last_space("only-a", 1)));
        assert!(a2.snapshot().spaces.is_empty());
        assert_eq!(b2.snapshot().last_space, None);
        assert_eq!(b2.snapshot().spaces.len(), 1);
    }

    #[test]
    fn missing_file_loads_defaults() {
        let dir = TestDir::new();
        let (store, outcome) = store_at(&dir, ID_A);
        assert_eq!(outcome, LoadOutcome::Missing);
        assert_eq!(store.snapshot(), ClientStateFile::default());
        assert!(!store.path().exists());
    }

    #[test]
    fn invalid_file_is_never_overwritten() {
        let dir = TestDir::new();
        let path = client_state_path(dir.path(), ID_A).expect("path");
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, b"{ not json").expect("seed");
        let (store, outcome) = ClientStateStore::load(path.clone());
        assert!(matches!(outcome, LoadOutcome::Invalid(_)));
        assert_eq!(
            store.set_last_space(last_space("s1", 1)),
            Err(StateError::Invalid)
        );
        assert_eq!(
            store.set_space_state("/p", space_state("s1")),
            Err(StateError::Invalid)
        );
        assert_eq!(fs::read(&path).expect("read"), b"{ not json");
        assert!(leftovers(path.parent().expect("parent")).is_empty());
    }

    #[test]
    fn restart_restores_state() {
        let dir = TestDir::new();
        {
            let (store, _) = store_at(&dir, ID_A);
            store
                .set_space_state("/work/a", space_state("s1"))
                .expect("space");
            store.set_last_space(last_space("s1", 42)).expect("last");
        }
        let (store, outcome) = store_at(&dir, ID_A);
        assert_eq!(outcome, LoadOutcome::Loaded);
        assert_eq!(store.snapshot().last_space, Some(last_space("s1", 42)));
        assert_eq!(
            store.snapshot().spaces.get("/work/a"),
            Some(&space_state("s1"))
        );
        store
            .set_last_space(last_space("s1", 43))
            .expect("write after restart");
        assert_eq!(store.snapshot().spaces.len(), 1);
    }
}
