//! `hosts.json`: the pure parser and the store that records a host's ID on first contact.
//!
//! Every [`ConfigError`] is Combine text that ends with at most one serde message, and entry
//! errors name the entry's position, never its name.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use combine_protocol::HostId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::identity::{self, HostIdStore};

pub const DEFAULT_COMBINED_PATH: &str = "/opt/homebrew/bin/combined";
pub const MAX_SOCKET_PATH_BYTES: usize = 103;

const MAX_FILE_BYTES: usize = 1 << 20;
const MAX_NAME_CHARS: usize = 64;
const MAX_DESTINATION_BYTES: usize = 255;
const MAX_COMBINED_PATH_BYTES: usize = 1024;
const TEMP_PREFIX: &str = ".hosts.json.";
const MAX_TEMP_ATTEMPTS: u64 = 16;
const MAX_LOCK_ATTEMPTS: u32 = 8;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostsFileV1 {
    pub version: u32,
    pub hosts: Vec<HostEntryV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostEntryV1 {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub combined_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostsConfig {
    pub hosts: Vec<HostConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostConfig {
    pub name: String,
    pub host_id: Option<HostId>,
    pub transport: Transport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Ssh {
        destination: String,
        combined_path: String,
    },
    Local {
        socket: PathBuf,
    },
}

/// Why `hosts.json` is invalid, as text for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

fn invalid(text: impl Into<String>) -> ConfigError {
    ConfigError(text.into())
}

fn entry_error(position: usize, reason: impl fmt::Display) -> ConfigError {
    invalid(format!("hosts.json host {position}: {reason}"))
}

/// Parses and validates the bytes of `hosts.json`. Any failure makes the whole file invalid.
pub fn parse_hosts(bytes: &[u8]) -> Result<HostsConfig, ConfigError> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(invalid("hosts.json is over 1 MiB"));
    }
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|e| invalid(format!("hosts.json is not valid JSON: {e}")))?;
    let Value::Object(object) = &value else {
        return Err(invalid("hosts.json is not a JSON object"));
    };
    match object.get("version") {
        None => return Err(invalid("hosts.json has no \"version\" field")),
        Some(Value::Number(n)) if n.as_u64() == Some(1) => {}
        Some(Value::Number(n)) => {
            return Err(invalid(format!(
                "hosts.json has version {n}; this Combine reads version 1"
            )))
        }
        Some(_) => {
            return Err(invalid(
                "hosts.json has a \"version\" that is not a number; this Combine reads version 1",
            ))
        }
    }
    let file: HostsFileV1 = serde_json::from_value(value)
        .map_err(|e| invalid(format!("hosts.json does not match the schema: {e}")))?;
    let mut hosts: Vec<HostConfig> = Vec::with_capacity(file.hosts.len());
    for (index, entry) in file.hosts.iter().enumerate() {
        let position = index + 1;
        let host = validate_entry(position, entry, &file.hosts[..index])?;
        hosts.push(host);
    }
    Ok(HostsConfig { hosts })
}

fn validate_entry(
    position: usize,
    entry: &HostEntryV1,
    earlier: &[HostEntryV1],
) -> Result<HostConfig, ConfigError> {
    let fail = |reason: &str| entry_error(position, reason);

    let name = entry.name.as_str();
    if name.is_empty() || name != name.trim() {
        return Err(fail("name is empty or has surrounding spaces"));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(fail("name is longer than 64 characters"));
    }
    if name.chars().any(char::is_control) {
        return Err(fail("name has control characters"));
    }
    if let Some(m) = earlier.iter().position(|e| e.name == entry.name) {
        return Err(entry_error(
            position,
            format!("name is used by host {}", m + 1),
        ));
    }

    let transport = match (&entry.ssh, &entry.socket) {
        (Some(_), Some(_)) => return Err(fail("has both \"ssh\" and \"socket\"")),
        (None, None) => return Err(fail("needs \"ssh\" or \"socket\"")),
        (Some(destination), None) => {
            if !is_valid_destination(destination) {
                return Err(fail(
                    "\"ssh\" must be an SSH destination such as mini or user@host, not starting with \"-\"",
                ));
            }
            let combined_path = match &entry.combined_path {
                Some(path) if !is_valid_combined_path(path) => {
                    return Err(fail(
                        "\"combined_path\" must be an absolute path made of letters, digits and . _ / + -",
                    ))
                }
                Some(path) => path.clone(),
                None => DEFAULT_COMBINED_PATH.to_string(),
            };
            Transport::Ssh {
                destination: destination.clone(),
                combined_path,
            }
        }
        (None, Some(socket)) => {
            if entry.combined_path.is_some() {
                return Err(fail("\"combined_path\" is only for \"ssh\" hosts"));
            }
            if !socket.starts_with('/') {
                return Err(fail("\"socket\" must be an absolute path"));
            }
            if socket.len() > MAX_SOCKET_PATH_BYTES {
                return Err(entry_error(
                    position,
                    format!(
                        "\"socket\" is {} bytes; macOS allows at most {MAX_SOCKET_PATH_BYTES}",
                        socket.len()
                    ),
                ));
            }
            if socket.chars().any(char::is_control) {
                return Err(fail("\"socket\" has control characters"));
            }
            Transport::Local {
                socket: PathBuf::from(socket),
            }
        }
    };

    let host_id = match &entry.host_id {
        None => None,
        Some(id) => {
            if !identity::is_valid_host_id(id) {
                return Err(fail("\"host_id\" must be a lowercase UUID"));
            }
            if let Some(m) = earlier.iter().position(|e| e.host_id.as_ref() == Some(id)) {
                return Err(entry_error(
                    position,
                    format!("\"host_id\" is also used by host {}", m + 1),
                ));
            }
            Some(HostId(id.clone()))
        }
    };

    Ok(HostConfig {
        name: entry.name.clone(),
        host_id,
        transport,
    })
}

fn is_valid_destination(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_DESTINATION_BYTES
        && !s.starts_with('-')
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn is_valid_combined_path(s: &str) -> bool {
    s.starts_with('/')
        && s.len() <= MAX_COMBINED_PATH_BYTES
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'+' | b'-'))
}

/// Why the store did not record a host ID. The `Display` texts are Combine text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    Invalid,
    ChangedOnDisk,
    NoSuchEntry,
    AlreadyRecorded(HostId),
    DuplicateHostId { other: String },
    Io(String),
}

impl StoreError {
    /// External text for the UI's detail line: the I/O error, for `Io` only.
    pub fn detail(&self) -> Option<String> {
        match self {
            StoreError::Io(text) => Some(text.clone()),
            _ => None,
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Invalid => {
                f.write_str("hosts.json is invalid, so Combine will not write it")
            }
            StoreError::ChangedOnDisk => f.write_str(
                "hosts.json changed on disk since Combine started; restart Combine to load it",
            ),
            StoreError::NoSuchEntry => f.write_str("hosts.json no longer has this host"),
            StoreError::AlreadyRecorded(id) => {
                write!(f, "hosts.json already records host ID {}", id.0)
            }
            StoreError::DuplicateHostId { other } => {
                write!(f, "host \"{other}\" in hosts.json already has this host ID")
            }
            StoreError::Io(_) => f.write_str("could not write hosts.json"),
        }
    }
}

impl std::error::Error for StoreError {}

enum StoreState {
    Invalid,
    Loaded { bytes: Vec<u8> },
}

/// The file behind the rail. Writes only the host ID, never over an invalid or changed file.
pub struct HostsStore {
    path: PathBuf,
    state: Mutex<StoreState>,
}

impl HostsStore {
    /// A missing file means no hosts. An invalid or unreadable one leaves the store refusing
    /// every write.
    pub fn load(path: PathBuf) -> (HostsStore, Result<HostsConfig, ConfigError>) {
        let loaded = match fs::read(&path) {
            Ok(bytes) => parse_hosts(&bytes).map(|config| (config, bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Ok((HostsConfig { hosts: Vec::new() }, Vec::new()))
            }
            Err(e) => Err(invalid(format!("hosts.json could not be read: {e}"))),
        };
        match loaded {
            Ok((config, bytes)) => {
                let store = HostsStore {
                    path,
                    state: Mutex::new(StoreState::Loaded { bytes }),
                };
                (store, Ok(config))
            }
            Err(error) => {
                let store = HostsStore {
                    path,
                    state: Mutex::new(StoreState::Invalid),
                };
                (store, Err(error))
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> MutexGuard<'_, StoreState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes an exclusive advisory lock on `hosts.json` itself, so concurrent store instances
    /// and processes serialize their whole read-compare-write. The file is opened read-only and
    /// never created: `None` means it does not exist. A writer renames a new file over the old
    /// one, so after locking, the handle must still be the file at the path; otherwise it is
    /// released and the lock is retaken on the new file.
    fn lock_file(&self) -> Result<Option<fs::File>, StoreError> {
        for _ in 0..MAX_LOCK_ATTEMPTS {
            let file = match fs::File::open(&self.path) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(StoreError::Io(e.to_string())),
            };
            file.lock().map_err(|e| StoreError::Io(e.to_string()))?;
            let held = file.metadata().map_err(|e| StoreError::Io(e.to_string()))?;
            match fs::metadata(&self.path) {
                Ok(at_path) if (at_path.dev(), at_path.ino()) == (held.dev(), held.ino()) => {
                    return Ok(Some(file))
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(StoreError::Io(e.to_string())),
            }
        }
        Err(StoreError::Io(
            "hosts.json kept being replaced while locking it".to_string(),
        ))
    }

    fn temp_path(&self, n: u64) -> PathBuf {
        self.path
            .with_file_name(format!("{TEMP_PREFIX}{}.{n}.tmp", std::process::id()))
    }

    /// Writes `bytes` to a new, uniquely named temp file next to `hosts.json`, then renames it
    /// over `hosts.json`. The temp file is created exclusively (`O_EXCL`), so an existing entry
    /// at a candidate name, symlinks included, is skipped and never followed or truncated.
    fn write_safely(&self, bytes: &[u8]) -> Result<(), StoreError> {
        let first = NEXT_TEMP.fetch_add(MAX_TEMP_ATTEMPTS, Ordering::Relaxed);
        self.write_safely_from(bytes, first)
    }

    fn write_safely_from(&self, bytes: &[u8], first: u64) -> Result<(), StoreError> {
        for n in first..first + MAX_TEMP_ATTEMPTS {
            let temp = self.temp_path(n);
            let mut file = match OpenOptions::new().write(true).create_new(true).open(&temp) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(StoreError::Io(e.to_string())),
            };
            let result = self
                .match_private_mode(&file)
                .and_then(|()| file.write_all(bytes))
                .and_then(|()| file.sync_all())
                .and_then(|()| {
                    drop(file);
                    fs::rename(&temp, &self.path)
                });
            return result.map_err(|e| {
                let _ = fs::remove_file(&temp);
                StoreError::Io(e.to_string())
            });
        }
        Err(StoreError::Io(
            "no free temporary file name next to hosts.json".to_string(),
        ))
    }

    /// If `hosts.json` is private to its owner (no group or other bits), the replacement is too
    /// (0600). Otherwise the temp file keeps the default mode from the umask.
    #[cfg(unix)]
    fn match_private_mode(&self, file: &fs::File) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        match fs::metadata(&self.path) {
            Ok(meta) if meta.permissions().mode() & 0o077 == 0 => {
                file.set_permissions(fs::Permissions::from_mode(0o600))
            }
            _ => Ok(()),
        }
    }

    #[cfg(not(unix))]
    fn match_private_mode(&self, _file: &fs::File) -> io::Result<()> {
        Ok(())
    }
}

impl HostIdStore for HostsStore {
    fn record_first_contact(&self, name: &str, host_id: &HostId) -> Result<(), StoreError> {
        let mut state = self.lock();
        let StoreState::Loaded { bytes: loaded } = &*state else {
            return Err(StoreError::Invalid);
        };
        let loaded = loaded.clone();
        let lock = self.lock_file()?;
        let mut current = Vec::new();
        if let Some(mut file) = lock.as_ref() {
            file.read_to_end(&mut current)
                .map_err(|e| StoreError::Io(e.to_string()))?;
        }
        if current != loaded {
            return Err(StoreError::ChangedOnDisk);
        }
        if current.is_empty() {
            return Err(StoreError::NoSuchEntry);
        }
        let mut file: HostsFileV1 =
            serde_json::from_slice(&current).map_err(|_| StoreError::Invalid)?;
        let Some(index) = file.hosts.iter().position(|e| e.name == name) else {
            return Err(StoreError::NoSuchEntry);
        };
        if let Some(existing) = &file.hosts[index].host_id {
            return Err(StoreError::AlreadyRecorded(HostId(existing.clone())));
        }
        if let Some(other) = file
            .hosts
            .iter()
            .find(|e| e.host_id.as_deref() == Some(host_id.0.as_str()))
        {
            return Err(StoreError::DuplicateHostId {
                other: other.name.clone(),
            });
        }
        file.hosts[index].host_id = Some(host_id.0.clone());
        let mut out =
            serde_json::to_vec_pretty(&file).map_err(|e| StoreError::Io(e.to_string()))?;
        out.push(b'\n');
        self.write_safely(&out)?;
        drop(lock);
        *state = StoreState::Loaded { bytes: out };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;

    use super::*;

    const ID_A: &str = "6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6c";
    const ID_B: &str = "0f8c2a6e-4b1d-4c5e-9a7b-3d2e1f0a9b8c";

    static NEXT_DIR: AtomicU32 = AtomicU32::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("combine-hosts-config-{}-{n}", std::process::id()));
            fs::create_dir_all(&dir).expect("create test dir");
            TestDir(dir)
        }

        fn file(&self) -> PathBuf {
            self.0.join("hosts.json")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_leftovers(dir: &TestDir) -> Vec<String> {
        fs::read_dir(&dir.0)
            .expect("list test dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(TEMP_PREFIX) && n.ends_with(".tmp"))
            .collect()
    }

    fn parse(text: &str) -> Result<HostsConfig, ConfigError> {
        parse_hosts(text.as_bytes())
    }

    fn error_text(text: &str) -> String {
        parse(text).expect_err("should be invalid").to_string()
    }

    fn one_entry(entry: &str) -> String {
        format!(r#"{{"version": 1, "hosts": [{entry}]}}"#)
    }

    fn id(s: &str) -> HostId {
        HostId(s.to_string())
    }

    fn store_with(dir: &TestDir, text: &str) -> HostsStore {
        fs::write(dir.file(), text).expect("write hosts.json");
        let (store, config) = HostsStore::load(dir.file());
        config.expect("valid hosts.json");
        store
    }

    #[test]
    fn parses_ssh_and_local_entries_in_file_order() {
        let text = format!(
            r#"{{"version": 1, "hosts": [
                {{"name": "this-mac", "socket": "/tmp/combine-local/combined.sock"}},
                {{"name": "mini", "ssh": "mini", "combined_path": "/usr/local/bin/combined", "host_id": "{ID_A}"}}
            ]}}"#
        );
        let config = parse(&text).expect("valid");
        assert_eq!(
            config.hosts,
            vec![
                HostConfig {
                    name: "this-mac".to_string(),
                    host_id: None,
                    transport: Transport::Local {
                        socket: PathBuf::from("/tmp/combine-local/combined.sock")
                    },
                },
                HostConfig {
                    name: "mini".to_string(),
                    host_id: Some(id(ID_A)),
                    transport: Transport::Ssh {
                        destination: "mini".to_string(),
                        combined_path: "/usr/local/bin/combined".to_string()
                    },
                },
            ]
        );
    }

    #[test]
    fn defaults_combined_path() {
        let config = parse(&one_entry(r#"{"name": "mini", "ssh": "user@mini"}"#)).expect("valid");
        assert_eq!(
            config.hosts[0].transport,
            Transport::Ssh {
                destination: "user@mini".to_string(),
                combined_path: DEFAULT_COMBINED_PATH.to_string()
            }
        );
    }

    #[test]
    fn rejects_missing_version() {
        assert_eq!(
            error_text(r#"{"hosts": []}"#),
            "hosts.json has no \"version\" field"
        );
        assert_eq!(error_text("[]"), "hosts.json is not a JSON object");
        assert!(error_text("{").starts_with("hosts.json is not valid JSON: "));
    }

    #[test]
    fn rejects_unsupported_version() {
        assert_eq!(
            error_text(r#"{"version": 2, "hosts": []}"#),
            "hosts.json has version 2; this Combine reads version 1"
        );
    }

    #[test]
    fn rejects_non_number_version() {
        assert_eq!(
            error_text(r#"{"version": "x\u001b[31m", "hosts": []}"#),
            "hosts.json has a \"version\" that is not a number; this Combine reads version 1"
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let top = error_text(r#"{"version": 1, "hosts": [], "extra": true}"#);
        assert!(
            top.starts_with("hosts.json does not match the schema: "),
            "{top}"
        );
        let entry = error_text(&one_entry(r#"{"name": "a", "ssh": "a", "port": 22}"#));
        assert!(
            entry.starts_with("hosts.json does not match the schema: "),
            "{entry}"
        );
    }

    #[test]
    fn rejects_duplicate_names() {
        let text = format!(
            "{},{}",
            r#"{"name": "a", "ssh": "a"}"#, r#"{"name": "a", "ssh": "b"}"#
        );
        assert_eq!(
            error_text(&one_entry(&text)),
            "hosts.json host 2: name is used by host 1"
        );
    }

    #[test]
    fn rejects_duplicate_host_ids() {
        let text = format!(
            r#"{{"name": "a", "ssh": "a", "host_id": "{ID_A}"}},{{"name": "b", "ssh": "b", "host_id": "{ID_A}"}}"#
        );
        assert_eq!(
            error_text(&one_entry(&text)),
            "hosts.json host 2: \"host_id\" is also used by host 1"
        );
    }

    #[test]
    fn rejects_both_or_neither_transport() {
        assert_eq!(
            error_text(&one_entry(
                r#"{"name": "a", "ssh": "a", "socket": "/tmp/a.sock"}"#
            )),
            "hosts.json host 1: has both \"ssh\" and \"socket\""
        );
        assert_eq!(
            error_text(&one_entry(r#"{"name": "a"}"#)),
            "hosts.json host 1: needs \"ssh\" or \"socket\""
        );
        assert_eq!(
            error_text(&one_entry(
                r#"{"name": "a", "socket": "/tmp/a.sock", "combined_path": "/x"}"#
            )),
            "hosts.json host 1: \"combined_path\" is only for \"ssh\" hosts"
        );
    }

    #[test]
    fn rejects_unsafe_combined_path() {
        let reason = "hosts.json host 1: \"combined_path\" must be an absolute path made of letters, digits and . _ / + -";
        for path in [
            "combined",
            "/opt/a b/combined",
            "/opt/$(x)",
            "/opt/a;b",
            "/opt/\u{e9}",
        ] {
            let entry = format!(r#"{{"name": "a", "ssh": "a", "combined_path": "{path}"}}"#);
            assert_eq!(error_text(&one_entry(&entry)), reason, "{path}");
        }
        let long = format!("/{}", "a".repeat(1024));
        let entry = format!(r#"{{"name": "a", "ssh": "a", "combined_path": "{long}"}}"#);
        assert_eq!(error_text(&one_entry(&entry)), reason);
    }

    #[test]
    fn rejects_destination_starting_with_dash() {
        let reason = "hosts.json host 1: \"ssh\" must be an SSH destination such as mini or user@host, not starting with \"-\"";
        for dest in ["-oProxyCommand=x", "", "a b", "a\\tb"] {
            let entry = format!(r#"{{"name": "a", "ssh": "{dest}"}}"#);
            assert_eq!(error_text(&one_entry(&entry)), reason, "{dest:?}");
        }
    }

    #[test]
    fn rejects_socket_path_over_103_bytes() {
        let ok = format!("/{}", "a".repeat(102));
        assert!(parse(&one_entry(&format!(r#"{{"name": "a", "socket": "{ok}"}}"#))).is_ok());
        let long = format!("/{}", "a".repeat(103));
        assert_eq!(
            error_text(&one_entry(&format!(
                r#"{{"name": "a", "socket": "{long}"}}"#
            ))),
            "hosts.json host 1: \"socket\" is 104 bytes; macOS allows at most 103"
        );
        assert_eq!(
            error_text(&one_entry(r#"{"name": "a", "socket": "relative.sock"}"#)),
            "hosts.json host 1: \"socket\" must be an absolute path"
        );
    }

    #[test]
    fn rejects_socket_with_control_characters() {
        assert_eq!(
            error_text(&one_entry(
                r#"{"name": "a", "socket": "/tmp/a\u001b.sock"}"#
            )),
            "hosts.json host 1: \"socket\" has control characters"
        );
    }

    #[test]
    fn rejects_invalid_host_id() {
        for bad in ["", "not-a-uuid", &ID_A.to_uppercase()] {
            let entry = format!(r#"{{"name": "a", "ssh": "a", "host_id": "{bad}"}}"#);
            assert_eq!(
                error_text(&one_entry(&entry)),
                "hosts.json host 1: \"host_id\" must be a lowercase UUID",
                "{bad}"
            );
        }
        assert!(parse(&one_entry(r#"{"name": "a", "ssh": "a", "host_id": null}"#)).is_ok());
    }

    #[test]
    fn rejects_file_over_1_mib() {
        let mut bytes = br#"{"version": 1, "hosts": []}"#.to_vec();
        bytes.resize((1 << 20) + 1, b' ');
        assert_eq!(
            parse_hosts(&bytes).expect_err("too big").to_string(),
            "hosts.json is over 1 MiB"
        );
        bytes.truncate(1 << 20);
        assert!(parse_hosts(&bytes).is_ok());
    }

    #[test]
    fn entry_errors_name_the_position() {
        let name = "n".repeat(1000);
        let entry = format!(r#"{{"name": "{name}", "ssh": "a"}}"#);
        assert_eq!(
            error_text(&one_entry(&entry)),
            "hosts.json host 1: name is longer than 64 characters"
        );
        assert_eq!(
            error_text(&one_entry(r#"{"name": " a", "ssh": "a"}"#)),
            "hosts.json host 1: name is empty or has surrounding spaces"
        );
        assert_eq!(
            error_text(&one_entry(r#"{"name": "a\u001bb", "ssh": "a"}"#)),
            "hosts.json host 1: name has control characters"
        );
    }

    #[test]
    fn missing_file_means_no_hosts() {
        let dir = TestDir::new();
        let (store, config) = HostsStore::load(dir.file());
        assert_eq!(config.expect("no hosts"), HostsConfig { hosts: Vec::new() });
        assert_eq!(store.path(), dir.file());
        assert_eq!(
            store.record_first_contact("mini", &id(ID_A)),
            Err(StoreError::NoSuchEntry)
        );
        assert!(!dir.file().exists());
    }

    #[test]
    fn records_first_contact_with_safe_write() {
        let dir = TestDir::new();
        let store = store_with(
            &dir,
            r#"{"version": 1, "hosts": [{"name": "this-mac", "socket": "/tmp/a.sock"}, {"name": "mini", "ssh": "mini"}]}"#,
        );
        store
            .record_first_contact("mini", &id(ID_A))
            .expect("recorded");
        assert!(temp_leftovers(&dir).is_empty());
        let written = fs::read_to_string(dir.file()).expect("read");
        assert!(written.ends_with("}\n"));
        let value: Value = serde_json::from_str(&written).expect("json");
        assert_eq!(
            value,
            serde_json::json!({
                "version": 1,
                "hosts": [
                    {"name": "this-mac", "socket": "/tmp/a.sock"},
                    {"name": "mini", "ssh": "mini", "host_id": ID_A}
                ]
            })
        );
        assert!(!written.contains("combined_path"));
        let (_, reloaded) = HostsStore::load(dir.file());
        assert_eq!(reloaded.expect("valid").hosts[1].host_id, Some(id(ID_A)));
    }

    #[test]
    fn never_overwrites_invalid_file() {
        let dir = TestDir::new();
        let bytes = br#"{"version": 2, "hosts": []}"#;
        fs::write(dir.file(), bytes).expect("write");
        let (store, config) = HostsStore::load(dir.file());
        assert!(config.is_err());
        assert_eq!(
            store.record_first_contact("mini", &id(ID_A)),
            Err(StoreError::Invalid)
        );
        assert_eq!(fs::read(dir.file()).expect("read"), bytes);
        assert!(temp_leftovers(&dir).is_empty());
    }

    #[test]
    fn refuses_write_when_changed_on_disk() {
        let dir = TestDir::new();
        let store = store_with(
            &dir,
            r#"{"version": 1, "hosts": [{"name": "mini", "ssh": "mini"}]}"#,
        );
        let edited = br#"{"version": 1, "hosts": [{"name": "mini", "ssh": "other"}]}"#;
        fs::write(dir.file(), edited).expect("edit");
        assert_eq!(
            store.record_first_contact("mini", &id(ID_A)),
            Err(StoreError::ChangedOnDisk)
        );
        assert_eq!(fs::read(dir.file()).expect("read"), edited);
    }

    #[test]
    fn never_rewrites_recorded_host_id() {
        let dir = TestDir::new();
        let text = format!(
            r#"{{"version": 1, "hosts": [{{"name": "mini", "ssh": "mini", "host_id": "{ID_A}"}}]}}"#
        );
        let store = store_with(&dir, &text);
        assert_eq!(
            store.record_first_contact("mini", &id(ID_B)),
            Err(StoreError::AlreadyRecorded(id(ID_A)))
        );
        assert_eq!(fs::read_to_string(dir.file()).expect("read"), text);
        assert_eq!(
            store.record_first_contact("gone", &id(ID_B)),
            Err(StoreError::NoSuchEntry)
        );
    }

    #[test]
    fn refuses_first_contact_with_another_entrys_host_id() {
        let dir = TestDir::new();
        let text = format!(
            r#"{{"version": 1, "hosts": [{{"name": "mini", "ssh": "mini", "host_id": "{ID_A}"}}, {{"name": "spare", "ssh": "spare"}}]}}"#
        );
        let store = store_with(&dir, &text);
        assert_eq!(
            store.record_first_contact("spare", &id(ID_A)),
            Err(StoreError::DuplicateHostId {
                other: "mini".to_string()
            })
        );
        assert_eq!(fs::read_to_string(dir.file()).expect("read"), text);
    }

    const TWO_HOSTS: &str = r#"{"version": 1, "hosts": [{"name": "mini", "ssh": "mini"}, {"name": "spare", "ssh": "spare"}]}"#;

    #[cfg(unix)]
    #[test]
    fn refuses_to_follow_a_symlink_at_the_temp_path() {
        use std::os::unix::fs::symlink;
        let dir = TestDir::new();
        let store = store_with(&dir, TWO_HOSTS);
        let sentinel = dir.0.join("unrelated.txt");
        fs::write(&sentinel, b"keep me").expect("sentinel");
        symlink(&sentinel, dir.0.join(".hosts.json.tmp")).expect("old name");
        symlink(&sentinel, store.temp_path(5000)).expect("first candidate");

        store
            .write_safely_from(b"{\"version\": 1, \"hosts\": []}\n", 5000)
            .expect("skips the symlink");
        assert_eq!(fs::read(&sentinel).expect("read"), b"keep me");
        let meta = fs::symlink_metadata(dir.file()).expect("meta");
        assert!(meta.file_type().is_file() && !meta.file_type().is_symlink());

        let store = store_with(&dir, TWO_HOSTS);
        store
            .record_first_contact("mini", &id(ID_A))
            .expect("recorded");
        assert_eq!(fs::read(&sentinel).expect("read"), b"keep me");
        let meta = fs::symlink_metadata(dir.file()).expect("meta");
        assert!(meta.file_type().is_file());
        assert!(fs::read_to_string(dir.file()).expect("read").contains(ID_A));
    }

    #[test]
    fn a_pre_existing_temp_name_is_skipped_not_truncated() {
        let dir = TestDir::new();
        let store = store_with(&dir, TWO_HOSTS);
        fs::write(store.temp_path(7000), b"someone else's file").expect("planted");
        store
            .write_safely_from(b"new bytes", 7000)
            .expect("written");
        assert_eq!(
            fs::read(store.temp_path(7000)).expect("read"),
            b"someone else's file"
        );
        assert_eq!(fs::read(dir.file()).expect("read"), b"new bytes");

        for n in 8000..8000 + MAX_TEMP_ATTEMPTS {
            fs::write(store.temp_path(n), b"taken").expect("planted");
        }
        let err = store.write_safely_from(b"x", 8000).expect_err("all taken");
        assert!(matches!(err, StoreError::Io(_)));
        assert_eq!(fs::read(dir.file()).expect("read"), b"new bytes");
    }

    #[test]
    fn two_store_instances_do_not_corrupt_each_other() {
        let dir = TestDir::new();
        fs::write(dir.file(), TWO_HOSTS).expect("write");
        let (first, _) = HostsStore::load(dir.file());
        let (second, _) = HostsStore::load(dir.file());
        first
            .record_first_contact("mini", &id(ID_A))
            .expect("first");
        assert_eq!(
            second.record_first_contact("spare", &id(ID_B)),
            Err(StoreError::ChangedOnDisk)
        );
        let file: HostsFileV1 =
            serde_json::from_slice(&fs::read(dir.file()).expect("read")).expect("valid json");
        assert_eq!(file.hosts[0].host_id.as_deref(), Some(ID_A));
        assert_eq!(file.hosts[1].host_id, None);
        assert!(temp_leftovers(&dir).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn private_file_mode_is_kept() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TestDir::new();
        let store = store_with(&dir, TWO_HOSTS);
        fs::set_permissions(dir.file(), fs::Permissions::from_mode(0o600)).expect("chmod");
        store
            .record_first_contact("mini", &id(ID_A))
            .expect("recorded");
        let mode = fs::metadata(dir.file()).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn concurrent_store_instances_never_lose_a_record() {
        use std::sync::Barrier;
        use std::thread;

        for round in 0..60 {
            let dir = TestDir::new();
            fs::write(dir.file(), TWO_HOSTS).expect("write");
            let (first, _) = HostsStore::load(dir.file());
            let (second, _) = HostsStore::load(dir.file());
            let barrier = Barrier::new(2);
            let (a, b) = thread::scope(|scope| {
                let a = scope.spawn(|| {
                    barrier.wait();
                    first.record_first_contact("mini", &id(ID_A))
                });
                let b = scope.spawn(|| {
                    barrier.wait();
                    second.record_first_contact("spare", &id(ID_B))
                });
                (a.join().expect("thread a"), b.join().expect("thread b"))
            });
            let file: HostsFileV1 =
                serde_json::from_slice(&fs::read(dir.file()).expect("read")).expect("valid json");
            let recorded: Vec<Option<&str>> =
                file.hosts.iter().map(|h| h.host_id.as_deref()).collect();
            match (a, b) {
                (Ok(()), Err(StoreError::ChangedOnDisk)) => {
                    assert_eq!(recorded, [Some(ID_A), None], "round {round}")
                }
                (Err(StoreError::ChangedOnDisk), Ok(())) => {
                    assert_eq!(recorded, [None, Some(ID_B)], "round {round}")
                }
                other => panic!("round {round}: unexpected outcomes {other:?}"),
            }
            assert!(temp_leftovers(&dir).is_empty(), "round {round}");
        }
    }

    #[test]
    fn store_error_texts() {
        assert_eq!(
            StoreError::Invalid.to_string(),
            "hosts.json is invalid, so Combine will not write it"
        );
        assert_eq!(
            StoreError::ChangedOnDisk.to_string(),
            "hosts.json changed on disk since Combine started; restart Combine to load it"
        );
        assert_eq!(
            StoreError::NoSuchEntry.to_string(),
            "hosts.json no longer has this host"
        );
        assert_eq!(
            StoreError::AlreadyRecorded(id(ID_A)).to_string(),
            format!("hosts.json already records host ID {ID_A}")
        );
        assert_eq!(
            StoreError::DuplicateHostId {
                other: "mini".to_string()
            }
            .to_string(),
            "host \"mini\" in hosts.json already has this host ID"
        );
        let io = StoreError::Io("disk full".to_string());
        assert_eq!(io.to_string(), "could not write hosts.json");
        assert_eq!(io.detail(), Some("disk full".to_string()));
        assert_eq!(StoreError::Invalid.detail(), None);
    }
}
