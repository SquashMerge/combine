#![cfg(feature = "test-support")]
//! The filesystem adapter: data dir, host ID, ID file, config file, git probe and file store.

mod common;
mod daemon_support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use combined::adapters::fs::config::{self, BackendName, ConfigFileError};
use combined::adapters::fs::data_dir::{DataDir, DataDirError};
use combined::adapters::fs::file_store::FsFileStore;
use combined::adapters::fs::git_probe::FsGitProbe;
use combined::adapters::fs::host_id::{self, is_valid_host_id, HostIdError};
use combined::adapters::fs::ids_file::{self, IdsFile, IdsFileError};
use combined::adapters::fs::safe_write::write_atomic;
use combined::model::{ContentHash, SpaceId};
use combined::ports::{
    valid_store_path, FileChange, FileStore, GitEntry, GitProbe, StoreError, StoreRevision,
};
use combined::testing::MemoryFileStore;
use daemon_support::ShortTempDir;
use futures::executor::block_on;
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use sha2::{Digest, Sha256};

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn open(scratch: &ShortTempDir) -> DataDir {
    DataDir::open(&scratch.path().join("data")).unwrap()
}

const KEEP: &str = "the daemon will not replace it, because clients check this ID; fix or restore it, or delete it only to give this host a new identity";

#[test]
fn data_dir_is_created_with_mode_0700() {
    let scratch = ShortTempDir::new();
    let dir = open(&scratch);
    assert_eq!(mode(dir.path()), 0o700);
    assert!(dir.path().join("combined.lock").is_file());
}

#[test]
fn existing_data_dir_is_tightened_to_0700() {
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("data");
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let dir = DataDir::open(&path).unwrap();
    assert_eq!(mode(dir.path()), 0o700);
}

#[test]
fn second_open_of_a_locked_data_dir_fails() {
    let scratch = ShortTempDir::new();
    let first = open(&scratch);
    let second = DataDir::open(first.path());
    assert!(
        matches!(second, Err(DataDirError::Locked { .. })),
        "{second:?}"
    );
}

#[test]
fn lock_is_released_when_the_holder_drops() {
    let scratch = ShortTempDir::new();
    let first = open(&scratch);
    let path = first.path().to_path_buf();
    drop(first);
    assert!(DataDir::open(&path).is_ok());
}

#[test]
fn write_atomic_replaces_the_file_and_leaves_no_temp_file() {
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("out.json");
    write_atomic(&path, b"one", 0o600).unwrap();
    write_atomic(&path, b"two", 0o600).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"two");
    assert_eq!(mode(&path), 0o600);
    let names: Vec<_> = fs::read_dir(scratch.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["out.json"]);
}

#[test]
fn concurrent_write_atomic_calls_to_one_path_all_succeed_and_leave_one_writers_bytes() {
    const WRITERS: usize = 8;
    const ROUNDS: usize = 50;
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("out.json");
    let payloads: Vec<Vec<u8>> = (0..WRITERS)
        .map(|writer| format!("writer {writer} ").repeat(2048).into_bytes())
        .collect();
    let barrier = std::sync::Barrier::new(WRITERS);
    std::thread::scope(|scope| {
        for payload in &payloads {
            let (path, barrier) = (&path, &barrier);
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..ROUNDS {
                    write_atomic(path, payload, 0o600).unwrap();
                }
            });
        }
    });
    let bytes = fs::read(&path).unwrap();
    assert!(payloads.contains(&bytes), "the file holds a mix of writers");
    let names: Vec<_> = fs::read_dir(scratch.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["out.json"]);
}

#[test]
fn host_id_is_created_on_first_open_with_mode_0600() {
    let scratch = ShortTempDir::new();
    let dir = open(&scratch);
    let id = host_id::load_or_create(&dir).unwrap();
    assert!(is_valid_host_id(&id.0), "{}", id.0);
    assert_eq!(mode(&dir.host_json()), 0o600);
    let text = fs::read_to_string(dir.host_json()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["host_id"], id.0.as_str());
}

/// The app's `hosts::identity::is_valid_host_id` reads the same file and must give the same
/// answers, because it relies on the daemon's rule.
#[test]
fn host_id_rule_matches_the_shared_table() {
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../combine-protocol/tests/fixtures/host_id_cases.json"
    ))
    .unwrap();
    for (key, expected) in [("accept", true), ("reject", false)] {
        let ids = cases[key].as_array().unwrap();
        assert!(!ids.is_empty());
        for id in ids {
            let id = id.as_str().unwrap();
            assert_eq!(is_valid_host_id(id), expected, "{id:?}");
        }
    }
}

#[test]
fn host_id_is_reused_on_the_next_open() {
    let scratch = ShortTempDir::new();
    let first = {
        let dir = open(&scratch);
        host_id::load_or_create(&dir).unwrap()
    };
    let dir = open(&scratch);
    assert_eq!(host_id::load_or_create(&dir).unwrap(), first);
}

#[test]
fn generated_host_ids_are_lowercase_v4_uuids() {
    let scratch = ShortTempDir::new();
    let mut seen = std::collections::BTreeSet::new();
    for n in 0..20 {
        let dir = DataDir::open(&scratch.path().join(format!("d{n}"))).unwrap();
        let id = host_id::load_or_create(&dir).unwrap().0;
        assert!(is_valid_host_id(&id), "{id}");
        assert_eq!(id, id.to_lowercase());
        let bytes = id.as_bytes();
        assert_eq!(bytes[14], b'4', "{id}");
        assert!(matches!(bytes[19], b'8' | b'9' | b'a' | b'b'), "{id}");
        assert!(seen.insert(id));
    }
}

#[test]
fn host_id_validity_is_8_4_4_4_12_lowercase_hex() {
    assert!(is_valid_host_id("0123abcd-0123-4abc-8def-0123456789ab"));
    assert!(!is_valid_host_id("0123ABCD-0123-4abc-8def-0123456789ab"));
    assert!(!is_valid_host_id("0123abcd01234abc8def0123456789ab"));
    assert!(!is_valid_host_id("0123abcd-0123-4abc-8def-0123456789a"));
    assert!(!is_valid_host_id("0123abcd-0123-4abc-8def-0123456789ag"));
}

fn refuses_and_keeps(content: &[u8]) -> HostIdError {
    let scratch = ShortTempDir::new();
    let dir = open(&scratch);
    fs::write(dir.host_json(), content).unwrap();
    let error = host_id::load_or_create(&dir).unwrap_err();
    assert_eq!(fs::read(dir.host_json()).unwrap(), content);
    let text = error.to_string();
    assert!(text.contains("host.json"), "{text}");
    assert!(text.ends_with(KEEP), "{text}");
    error
}

#[test]
fn host_id_file_with_bad_json_refuses_and_is_left_unchanged() {
    let error = refuses_and_keeps(b"{ not json");
    assert!(matches!(error, HostIdError::BadJson { .. }), "{error:?}");
}

#[test]
fn host_id_file_with_another_version_refuses() {
    let error =
        refuses_and_keeps(br#"{"version":2,"host_id":"0123abcd-0123-4abc-8def-0123456789ab"}"#);
    assert!(
        matches!(error, HostIdError::WrongVersion { found: 2, .. }),
        "{error:?}"
    );
}

#[test]
fn host_id_file_with_a_malformed_uuid_refuses() {
    let error = refuses_and_keeps(br#"{"version":1,"host_id":"NOT-A-UUID"}"#);
    assert!(matches!(error, HostIdError::InvalidId { .. }), "{error:?}");
}

#[test]
fn unreadable_host_id_file_refuses() {
    let scratch = ShortTempDir::new();
    let dir = open(&scratch);
    fs::create_dir(dir.host_json()).unwrap();
    let error = host_id::load_or_create(&dir).unwrap_err();
    assert!(matches!(error, HostIdError::Unreadable { .. }), "{error:?}");
    assert!(error.to_string().ends_with(KEEP));
    assert!(dir.host_json().is_dir());
}

fn write_config(scratch: &ShortTempDir, text: &str) -> std::path::PathBuf {
    let path = scratch.path().join("daemon.json");
    fs::write(&path, text).unwrap();
    path
}

#[test]
fn config_with_every_field_parses() {
    let scratch = ShortTempDir::new();
    let path = write_config(
        &scratch,
        r#"{"version":1,"backend":"fake","herdr_core_socket":"/tmp/a.sock","data_dir":"/tmp/d","socket":"/tmp/d/c.sock"}"#,
    );
    let file = config::load(&path, true).unwrap().unwrap();
    assert_eq!(file.backend, Some(BackendName::Fake));
    assert_eq!(file.herdr_core_socket, Some("/tmp/a.sock".into()));
    assert_eq!(file.data_dir, Some("/tmp/d".into()));
    assert_eq!(file.socket, Some("/tmp/d/c.sock".into()));
    let path = write_config(&scratch, r#"{"version":1,"backend":"herdr-core"}"#);
    let file = config::load(&path, true).unwrap().unwrap();
    assert_eq!(file.backend, Some(BackendName::HerdrCore));
}

#[test]
fn config_with_only_version_parses() {
    let scratch = ShortTempDir::new();
    let path = write_config(&scratch, r#"{"version":1}"#);
    let file = config::load(&path, false).unwrap().unwrap();
    assert_eq!(file.version, 1);
    assert_eq!(file.backend, None);
    assert_eq!(file.herdr_core_socket, None);
    assert_eq!(file.data_dir, None);
    assert_eq!(file.socket, None);
}

#[test]
fn config_with_unknown_field_is_refused() {
    let scratch = ShortTempDir::new();
    let path = write_config(&scratch, r#"{"version":1,"herdr_core_sock":"/tmp/a"}"#);
    let error = config::load(&path, false).unwrap_err();
    assert!(
        matches!(error, ConfigFileError::BadJson { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("herdr_core_sock"), "{error}");
}

#[test]
fn config_with_another_version_is_refused() {
    let scratch = ShortTempDir::new();
    let path = write_config(&scratch, r#"{"version":2}"#);
    let error = config::load(&path, false).unwrap_err();
    assert!(
        matches!(error, ConfigFileError::WrongVersion { found: 2, .. }),
        "{error:?}"
    );
}

#[test]
fn config_with_bad_json_is_refused() {
    let scratch = ShortTempDir::new();
    let path = write_config(&scratch, "{");
    let error = config::load(&path, false).unwrap_err();
    assert!(
        matches!(error, ConfigFileError::BadJson { .. }),
        "{error:?}"
    );
}

#[test]
fn config_with_a_relative_path_is_refused() {
    let scratch = ShortTempDir::new();
    let path = write_config(&scratch, r#"{"version":1,"socket":"c.sock"}"#);
    let error = config::load(&path, false).unwrap_err();
    assert!(
        matches!(
            error,
            ConfigFileError::RelativePath {
                field: "socket",
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn missing_optional_config_file_is_none() {
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("absent.json");
    assert_eq!(config::load(&path, false).unwrap(), None);
}

#[test]
fn missing_required_config_file_is_an_error() {
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("absent.json");
    let error = config::load(&path, true).unwrap_err();
    assert!(
        matches!(error, ConfigFileError::Missing { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("absent.json"));
}

#[test]
fn ids_file_missing_reads_as_none() {
    let scratch = ShortTempDir::new();
    assert_eq!(
        ids_file::load(&scratch.path().join("ids.json")).unwrap(),
        None
    );
}

#[test]
fn ids_file_round_trips_with_mode_0600() {
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("ids.json");
    ids_file::store(&path, &IdsFile::new(1 << 20)).unwrap();
    assert_eq!(mode(&path), 0o600);
    let loaded = ids_file::load(&path).unwrap().unwrap();
    assert_eq!(loaded.version, 1);
    assert_eq!(loaded.ids_reserved_through, 1 << 20);
}

#[test]
fn invalid_ids_file_refuses() {
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("ids.json");
    for text in [
        "nope",
        r#"{"version":2,"ids_reserved_through":5}"#,
        r#"{"version":1}"#,
    ] {
        fs::write(&path, text).unwrap();
        let error = ids_file::load(&path).unwrap_err();
        assert!(
            matches!(error, IdsFileError::Invalid { .. }),
            "{text}: {error:?}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let error = ids_file::load(&path).unwrap_err();
    assert!(
        matches!(error, IdsFileError::Unreadable { .. }),
        "{error:?}"
    );
}

#[test]
fn ids_file_with_unknown_field_refuses() {
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("ids.json");
    fs::write(&path, r#"{"version":1,"ids_reserved_through":5,"extra":1}"#).unwrap();
    let error = ids_file::load(&path).unwrap_err();
    assert!(matches!(error, IdsFileError::Invalid { .. }), "{error:?}");
}

#[test]
fn fs_git_probe_reports_directory_file_and_absent() {
    let scratch = ShortTempDir::new();
    let repo = scratch.path().join("repo");
    let worktree = scratch.path().join("worktree");
    let plain = scratch.path().join("plain");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::create_dir_all(&worktree).unwrap();
    fs::write(worktree.join(".git"), "gitdir: elsewhere").unwrap();
    fs::create_dir_all(&plain).unwrap();
    let probe = FsGitProbe;
    let entry = |dir: &Path| probe.git_entry(dir.to_str().unwrap());
    assert_eq!(entry(&repo), GitEntry::Directory);
    assert_eq!(entry(&worktree), GitEntry::File);
    assert_eq!(entry(&plain), GitEntry::Absent);
    assert_eq!(entry(&scratch.path().join("missing")), GitEntry::Absent);
}

fn space(scratch: &ShortTempDir, name: &str) -> SpaceId {
    SpaceId(scratch.path().join(name).to_str().unwrap().to_string())
}

#[test]
fn fs_file_store_reads_back_what_it_wrote() {
    let scratch = ShortTempDir::new();
    let space = space(&scratch, "work");
    let store = FsFileStore::new();
    let revision = block_on(store.write(&space, "layouts/a.json", b"hello".to_vec()))
        .unwrap()
        .revision;
    assert_eq!(revision, StoreRevision(1));
    let file = block_on(store.read(&space, "layouts/a.json"))
        .unwrap()
        .unwrap();
    assert_eq!(file.bytes, b"hello");
    assert_eq!(file.revision, StoreRevision(1));
    let on_disk = Path::new(&space.0).join(".workspace/layouts/a.json");
    assert_eq!(fs::read(on_disk).unwrap(), b"hello");
}

#[test]
fn fs_file_store_counts_revisions_per_space() {
    let scratch = ShortTempDir::new();
    let (a, b) = (space(&scratch, "a"), space(&scratch, "b"));
    let store = FsFileStore::new();
    assert_eq!(block_on(store.revision(&a)).unwrap(), StoreRevision(0));
    block_on(store.write(&a, "x", b"1".to_vec())).unwrap();
    let second = block_on(store.write(&a, "x", b"2".to_vec()))
        .unwrap()
        .revision;
    assert_eq!(second, StoreRevision(2));
    assert_eq!(block_on(store.revision(&a)).unwrap(), StoreRevision(2));
    assert_eq!(block_on(store.revision(&b)).unwrap(), StoreRevision(0));
    assert_eq!(block_on(store.read(&a, "x")).unwrap().unwrap().bytes, b"2");
}

#[test]
fn fs_file_store_reports_a_missing_file() {
    let scratch = ShortTempDir::new();
    let space = space(&scratch, "work");
    let store = FsFileStore::new();
    assert_eq!(block_on(store.read(&space, "nothing")).unwrap(), None);
    assert!(!Path::new(&space.0).exists());
}

#[test]
fn fs_file_store_path_rules_match_memory_file_store() {
    let scratch = ShortTempDir::new();
    let space = space(&scratch, "work");
    let fs_store = FsFileStore::new();
    let memory = MemoryFileStore::new();
    let table = [
        ("", false),
        ("a//b", false),
        ("./a", false),
        ("a/..", false),
        ("../a", false),
        ("/a", false),
        ("a/", false),
        ("a\0b", false),
        ("a", true),
        ("b/c.json", true),
        ("d.e/.f", true),
    ];
    for (path, valid) in table {
        for (name, store) in [("fs", &fs_store as &dyn FileStore), ("memory", &memory)] {
            let wrote = block_on(store.write(&space, path, b"x".to_vec()));
            let read = block_on(store.read(&space, path));
            if valid {
                assert!(
                    wrote.is_ok() && read.is_ok(),
                    "{name} {path:?}: {wrote:?} {read:?}"
                );
            } else {
                assert_eq!(wrote, Err(StoreError::InvalidPath), "{name} {path:?}");
                assert_eq!(read, Err(StoreError::InvalidPath), "{name} {path:?}");
            }
        }
    }
}

#[test]
fn fs_file_store_refuses_a_relative_space() {
    let store = FsFileStore::new();
    let space = SpaceId("cs4-relative-space".to_string());
    let written = block_on(store.write(&space, "a", b"x".to_vec()));
    let Err(StoreError::Unavailable(detail)) = written else {
        panic!("expected Unavailable, got {written:?}");
    };
    assert!(detail.contains("cs4-relative-space"), "{detail}");
    assert!(matches!(
        block_on(store.read(&space, "a")),
        Err(StoreError::Unavailable(_))
    ));
    assert!(!Path::new("cs4-relative-space").exists());
    assert_eq!(block_on(store.revision(&space)).unwrap(), StoreRevision(0));
}

// ---- file store symlink containment ----

enum Escape {
    /// `<space>/.workspace` is a symlink to the outside folder.
    Workspace,
    /// `<space>/.workspace/sub` is a symlink to the outside folder; the path is `sub/victim`.
    Folder,
    /// `<space>/.workspace/victim` is a symlink to the outside file.
    File,
}

const SENTINEL: &[u8] = b"keep me";

/// Builds a space whose `victim` path escapes through a symlink, and returns the space, the
/// store path and the outside sentinel file.
fn escaping_space(scratch: &ShortTempDir, escape: &Escape) -> (SpaceId, &'static str, PathBuf) {
    use std::os::unix::fs::symlink;
    let space = scratch.path().join("space");
    let outside = scratch.path().join("outside");
    fs::create_dir(&space).unwrap();
    fs::create_dir(&outside).unwrap();
    let sentinel = outside.join("victim");
    fs::write(&sentinel, SENTINEL).unwrap();
    let path = match escape {
        Escape::Workspace => {
            symlink(&outside, space.join(".workspace")).unwrap();
            "victim"
        }
        Escape::Folder => {
            fs::create_dir(space.join(".workspace")).unwrap();
            symlink(&outside, space.join(".workspace/sub")).unwrap();
            "sub/victim"
        }
        Escape::File => {
            fs::create_dir(space.join(".workspace")).unwrap();
            symlink(&sentinel, space.join(".workspace/victim")).unwrap();
            "victim"
        }
    };
    (SpaceId(space.to_str().unwrap().to_string()), path, sentinel)
}

fn assert_read_refused(escape: Escape) {
    let scratch = ShortTempDir::new();
    let (space, path, sentinel) = escaping_space(&scratch, &escape);
    let result = block_on(FsFileStore::new().read(&space, path));
    assert!(
        matches!(result, Err(StoreError::Unavailable(_))),
        "{result:?}"
    );
    assert_eq!(fs::read(&sentinel).unwrap(), SENTINEL);
}

fn assert_write_refused(escape: Escape) {
    let scratch = ShortTempDir::new();
    let (space, path, sentinel) = escaping_space(&scratch, &escape);
    let store = FsFileStore::new();
    let result = block_on(store.write(&space, path, b"overwritten".to_vec()));
    assert!(
        matches!(result, Err(StoreError::Unavailable(_))),
        "{result:?}"
    );
    assert_eq!(fs::read(&sentinel).unwrap(), SENTINEL);
    assert_eq!(block_on(store.revision(&space)).unwrap(), StoreRevision(0));
    let outside = sentinel.parent().unwrap();
    let names: Vec<_> = fs::read_dir(outside).unwrap().collect();
    assert_eq!(names.len(), 1, "nothing new appears outside: {names:?}");
}

#[test]
fn fs_file_store_refuses_to_read_through_a_symlinked_workspace() {
    assert_read_refused(Escape::Workspace);
}

#[test]
fn fs_file_store_refuses_to_write_through_a_symlinked_workspace() {
    assert_write_refused(Escape::Workspace);
}

#[test]
fn fs_file_store_refuses_to_read_through_a_symlinked_folder() {
    assert_read_refused(Escape::Folder);
}

#[test]
fn fs_file_store_refuses_to_write_through_a_symlinked_folder() {
    assert_write_refused(Escape::Folder);
}

#[test]
fn fs_file_store_refuses_to_read_a_symlinked_file() {
    assert_read_refused(Escape::File);
}

#[test]
fn fs_file_store_refuses_to_write_a_symlinked_file() {
    assert_write_refused(Escape::File);
}

// ---- the file store contract, for the real filesystem and the in-memory fake ----

const LAYOUT: &str = "layout.json";
const BOUND: Duration = Duration::from_secs(10);

fn sha256(bytes: &[u8]) -> ContentHash {
    ContentHash(Sha256::digest(bytes).into())
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

/// Waits up to a short slice for the next change.
fn poll_change(changes: &mut BoxStream<'static, FileChange>) -> Option<FileChange> {
    runtime()
        .block_on(async { tokio::time::timeout(Duration::from_millis(200), changes.next()).await })
        .ok()
        .flatten()
}

/// Runs `cause` until a change for `space`'s layout arrives, for at most [`BOUND`]. A watcher
/// may not be listening yet when the first cause happens, so the cause is repeated.
fn change_after(
    changes: &mut BoxStream<'static, FileChange>,
    space: &SpaceId,
    mut cause: impl FnMut(),
) -> Option<FileChange> {
    let deadline = Instant::now() + BOUND;
    while Instant::now() < deadline {
        cause();
        if let Some(change) = poll_change(changes) {
            if change.space == *space {
                return Some(change);
            }
        }
    }
    None
}

fn fs_layout(space: &SpaceId) -> PathBuf {
    Path::new(&space.0).join(".workspace").join(LAYOUT)
}

#[test]
fn store_contract_write_leaves_no_temp_and_reader_sees_whole_file() {
    let scratch = ShortTempDir::new();
    let space = space(&scratch, "work");
    let store = FsFileStore::new();
    let (a, b) = (vec![b'a'; 200_000], vec![b'b'; 200_000]);
    block_on(store.write(&space, LAYOUT, a.clone())).unwrap();
    let file = fs_layout(&space);
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let mut reads = 0;
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                let seen = fs::read(&file).unwrap();
                assert!(seen == a || seen == b, "a reader saw {} bytes", seen.len());
                reads += 1;
            }
            reads
        });
        for round in 0..40 {
            let bytes = if round % 2 == 0 { &b } else { &a };
            block_on(store.write(&space, LAYOUT, bytes.clone())).unwrap();
        }
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(reader.join().unwrap() > 0);
    });
    let names: Vec<_> = fs::read_dir(file.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, [LAYOUT]);
}

#[test]
fn store_contract_hash_matches_content() {
    let scratch = ShortTempDir::new();
    let space = space(&scratch, "work");
    let fallback = scratch.path().join("fallback");
    let fs_store = FsFileStore::with_fallback(&fallback);
    let memory = MemoryFileStore::new();
    for (name, store) in [("fs", &fs_store as &dyn FileStore), ("memory", &memory)] {
        let bytes = format!("{name} content").into_bytes();
        let outcome = block_on(store.write(&space, LAYOUT, bytes.clone())).unwrap();
        assert_eq!(outcome.hash, sha256(&bytes), "{name} write");
        assert!(!outcome.read_only, "{name}");
        let file = block_on(store.read(&space, LAYOUT)).unwrap().unwrap();
        assert_eq!(file.hash, sha256(&bytes), "{name} read");
        assert_eq!(file.hash, outcome.hash, "{name}");
        assert!(!file.read_only, "{name}");
    }
}

#[test]
fn store_contract_own_write_emits_change() {
    let scratch = ShortTempDir::new();
    let fs_space = space(&scratch, "work");
    let fs_store = FsFileStore::new();
    fs::create_dir_all(Path::new(&fs_space.0).join(".workspace")).unwrap();
    block_on(fs_store.watch(&fs_space)).unwrap();
    let mut fs_changes = fs_store.changes();
    let change = change_after(&mut fs_changes, &fs_space, || {
        block_on(fs_store.write(&fs_space, LAYOUT, b"mine".to_vec())).unwrap();
    });
    assert_eq!(
        change,
        Some(FileChange {
            space: fs_space,
            path: LAYOUT.to_string()
        })
    );

    let memory = MemoryFileStore::new();
    let memory_space = SpaceId("/repo".to_string());
    let mut memory_changes = memory.changes();
    block_on(memory.watch(&memory_space)).unwrap();
    block_on(memory.write(&memory_space, LAYOUT, b"mine".to_vec())).unwrap();
    assert_eq!(
        poll_change(&mut memory_changes),
        Some(FileChange {
            space: memory_space,
            path: LAYOUT.to_string()
        })
    );
}

#[test]
fn store_contract_change_on_outside_write() {
    let scratch = ShortTempDir::new();
    let fs_space = space(&scratch, "work");
    let fs_store = FsFileStore::new();
    fs::create_dir_all(Path::new(&fs_space.0).join(".workspace")).unwrap();
    block_on(fs_store.watch(&fs_space)).unwrap();
    let mut fs_changes = fs_store.changes();
    let mut round = 0;
    let change = change_after(&mut fs_changes, &fs_space, || {
        round += 1;
        fs::write(fs_layout(&fs_space), format!("outside {round}")).unwrap();
    });
    assert!(change.is_some(), "the outside write was never reported");
    assert_eq!(fs_store.changes().next().now_or_never(), Some(None));

    let memory = MemoryFileStore::new();
    let memory_space = SpaceId("/repo".to_string());
    let mut memory_changes = memory.changes();
    block_on(memory.watch(&memory_space)).unwrap();
    memory.write_outside(&memory_space, LAYOUT, b"theirs".to_vec());
    assert!(poll_change(&mut memory_changes).is_some());
    memory.remove_outside(&memory_space, LAYOUT);
    assert!(poll_change(&mut memory_changes).is_some());
}

#[test]
fn store_contract_invalid_path() {
    let table = [
        ("", false),
        ("a//b", false),
        ("./a", false),
        ("a/..", false),
        ("../a", false),
        ("/a", false),
        ("a/", false),
        ("a\0b", false),
        ("a", true),
        ("b/c.json", true),
        ("d.e/.f", true),
    ];
    let scratch = ShortTempDir::new();
    let space = space(&scratch, "work");
    let fs_store = FsFileStore::with_fallback(scratch.path().join("fallback"));
    let memory = MemoryFileStore::new();
    for (path, valid) in table {
        assert_eq!(valid_store_path(path), valid, "{path:?}");
        for (name, store) in [("fs", &fs_store as &dyn FileStore), ("memory", &memory)] {
            let wrote = block_on(store.write(&space, path, b"x".to_vec()));
            assert_eq!(wrote.is_ok(), valid, "{name} {path:?}: {wrote:?}");
            if !valid {
                assert_eq!(wrote, Err(StoreError::InvalidPath), "{name} {path:?}");
                assert_eq!(
                    block_on(store.read(&space, path)),
                    Err(StoreError::InvalidPath),
                    "{name} {path:?}"
                );
            }
        }
    }
    assert!(!scratch.path().join("fallback").exists());
}

fn hex_sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Makes `folder` unwritable and says whether that holds for this user: it doesn't for root.
fn lock_down(folder: &Path) -> bool {
    fs::set_permissions(folder, fs::Permissions::from_mode(0o555)).unwrap();
    let probe = folder.join("probe");
    let writable = fs::write(&probe, b"x").is_ok();
    let _ = fs::remove_file(&probe);
    !writable
}

fn unlock(folder: &Path) {
    fs::set_permissions(folder, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn store_contract_read_only_routes_to_fallback() {
    for workspace_exists in [false, true] {
        let scratch = ShortTempDir::new();
        let space = space(&scratch, "work");
        let folder = Path::new(&space.0);
        let locked = if workspace_exists {
            fs::create_dir_all(folder.join(".workspace")).unwrap();
            folder.join(".workspace")
        } else {
            fs::create_dir_all(folder).unwrap();
            folder.to_path_buf()
        };
        if !lock_down(&locked) {
            eprintln!("skipped: this user can write a 0o555 folder");
            continue;
        }
        let root = scratch.path().join("spaces");
        let store = FsFileStore::with_fallback(&root);
        let written = block_on(store.write(&space, LAYOUT, b"kept aside".to_vec()));
        let read = block_on(store.read(&space, LAYOUT));
        let bare = block_on(FsFileStore::new().write(&space, LAYOUT, b"x".to_vec()));
        unlock(&locked);

        let outcome = written.unwrap();
        assert!(outcome.read_only, "workspace_exists {workspace_exists}");
        assert_eq!(outcome.revision, StoreRevision(1));
        assert_eq!(outcome.hash, sha256(b"kept aside"));
        let copy = root.join(hex_sha256(&space.0)).join(LAYOUT);
        assert_eq!(fs::read(&copy).unwrap(), b"kept aside");
        assert!(!fs_layout(&space).exists());
        let file = read.unwrap().unwrap();
        assert_eq!(file.bytes, b"kept aside");
        assert!(file.read_only);
        assert_eq!(file.hash, outcome.hash);
        assert!(
            matches!(bare, Err(StoreError::ReadOnly(_))),
            "no fallback: {bare:?}"
        );
        assert!(!fs_layout(&space).exists());
    }
}

#[test]
fn store_contract_read_only_without_fallback_is_read_only_error() {
    let memory = MemoryFileStore::new().without_fallback();
    let space = SpaceId("/repo".to_string());
    memory.set_read_only(&space, true);
    assert!(matches!(
        block_on(memory.write(&space, LAYOUT, b"x".to_vec())),
        Err(StoreError::ReadOnly(_))
    ));
}

#[test]
fn store_contract_symlinked_workspace_with_fallback_is_unavailable_and_creates_no_fallback_file() {
    for escape in [Escape::Workspace, Escape::Folder, Escape::File] {
        let scratch = ShortTempDir::new();
        let (space, path, sentinel) = escaping_space(&scratch, &escape);
        let root = scratch.path().join("spaces");
        let store = FsFileStore::with_fallback(&root);
        let result = block_on(store.write(&space, path, b"overwritten".to_vec()));
        assert!(
            matches!(result, Err(StoreError::Unavailable(_))),
            "{result:?}"
        );
        assert_eq!(fs::read(&sentinel).unwrap(), SENTINEL);
        assert!(!root.exists(), "the fallback was reached");
        assert_eq!(block_on(store.revision(&space)).unwrap(), StoreRevision(0));
    }
    let scratch = ShortTempDir::new();
    let root = scratch.path().join("spaces");
    let store = FsFileStore::with_fallback(&root);
    let relative = SpaceId("cs7-relative-space".to_string());
    assert!(matches!(
        block_on(store.write(&relative, LAYOUT, b"x".to_vec())),
        Err(StoreError::Unavailable(_))
    ));
    assert!(!root.exists());
}

#[test]
fn store_contract_fallback_stays_authoritative_after_the_project_is_writable() {
    let scratch = ShortTempDir::new();
    let space = space(&scratch, "work");
    let folder = Path::new(&space.0);
    fs::create_dir_all(folder.join(".workspace")).unwrap();
    fs::write(fs_layout(&space), b"project").unwrap();
    let workspace = folder.join(".workspace");
    if !lock_down(&workspace) {
        eprintln!("skipped: this user can write a 0o555 folder");
        return;
    }
    let fs_store = FsFileStore::with_fallback(scratch.path().join("spaces"));
    let first = block_on(fs_store.write(&space, LAYOUT, b"A".to_vec()));
    unlock(&workspace);
    let first = first.unwrap();
    assert!(first.read_only);

    let memory = MemoryFileStore::new();
    let memory_space = SpaceId("/repo".to_string());
    memory.write_outside(&memory_space, LAYOUT, b"project".to_vec());
    memory.set_read_only(&memory_space, true);
    assert!(
        block_on(memory.write(&memory_space, LAYOUT, b"A".to_vec()))
            .unwrap()
            .read_only
    );
    memory.set_read_only(&memory_space, false);

    for (name, store, space) in [
        ("fs", &fs_store as &dyn FileStore, &space),
        ("memory", &memory, &memory_space),
    ] {
        let outcome = block_on(store.write(space, LAYOUT, b"B".to_vec())).unwrap();
        assert!(outcome.read_only, "{name}");
        assert_eq!(outcome.hash, sha256(b"B"), "{name}");
        let file = block_on(store.read(space, LAYOUT)).unwrap().unwrap();
        assert_eq!(file.bytes, b"B", "{name}");
        assert_eq!(file.hash, outcome.hash, "{name}");
        assert!(file.read_only, "{name}");
    }
    assert_eq!(fs::read(fs_layout(&space)).unwrap(), b"project");
    assert_eq!(
        memory.bytes(&memory_space, LAYOUT),
        Some(b"project".to_vec())
    );
    assert_eq!(
        memory.fallback_bytes(&memory_space, LAYOUT),
        Some(b"B".to_vec())
    );
}
