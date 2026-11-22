//! The daemon's identity: a UUID written once to `host.json`.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;

use combine_protocol::HostId;
use serde::{Deserialize, Serialize};

use super::data_dir::DataDir;
use super::safe_write::write_atomic;

const VERSION: u32 = 1;
const KEEP: &str = "the daemon will not replace it, because clients check this ID; fix or restore it, or delete it only to give this host a new identity";

#[derive(Debug)]
pub enum HostIdError {
    Unreadable { path: PathBuf, source: io::Error },
    BadJson { path: PathBuf, detail: String },
    WrongVersion { path: PathBuf, found: u32 },
    InvalidId { path: PathBuf },
    Write { path: PathBuf, source: io::Error },
}

impl fmt::Display for HostIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostIdError::Unreadable { path, source } => {
                write!(f, "cannot read {}: {source}; {KEEP}", path.display())
            }
            HostIdError::BadJson { path, detail } => {
                write!(
                    f,
                    "{} is not valid host JSON: {detail}; {KEEP}",
                    path.display()
                )
            }
            HostIdError::WrongVersion { path, found } => write!(
                f,
                "{} has version {found}, not {VERSION}; {KEEP}",
                path.display()
            ),
            HostIdError::InvalidId { path } => write!(
                f,
                "{} holds a host_id that is not a lowercase UUID; {KEEP}",
                path.display()
            ),
            HostIdError::Write { path, source } => {
                write!(f, "cannot write {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for HostIdError {}

#[derive(Serialize, Deserialize)]
struct HostFile {
    version: u32,
    host_id: String,
}

/// Reads `host.json`, or writes a new ID if the file doesn't exist. Any other state of the file
/// is an error, and the file is left as it is.
pub fn load_or_create(dir: &DataDir) -> Result<HostId, HostIdError> {
    let path = dir.host_json();
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return create(dir),
        Err(source) => return Err(HostIdError::Unreadable { path, source }),
    };
    let file: HostFile = serde_json::from_slice(&bytes).map_err(|error| HostIdError::BadJson {
        path: path.clone(),
        detail: error.to_string(),
    })?;
    if file.version != VERSION {
        return Err(HostIdError::WrongVersion {
            path,
            found: file.version,
        });
    }
    if !is_valid_host_id(&file.host_id) {
        return Err(HostIdError::InvalidId { path });
    }
    Ok(HostId(file.host_id))
}

fn create(dir: &DataDir) -> Result<HostId, HostIdError> {
    let path = dir.host_json();
    let write = |source| HostIdError::Write {
        path: path.clone(),
        source,
    };
    let id = new_uuid().map_err(write)?;
    let file = HostFile {
        version: VERSION,
        host_id: id.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&file)
        .map_err(|error| write(io::Error::new(io::ErrorKind::InvalidData, error)))?;
    write_atomic(&path, &bytes, 0o600).map_err(write)?;
    Ok(HostId(id))
}

/// A version 4 UUID from `/dev/urandom`, lowercase and hyphenated.
fn new_uuid() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut text = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        text.push_str(&format!("{byte:02x}"));
    }
    Ok(text)
}

/// Whether `id` is 8-4-4-4-12 lowercase hex.
pub fn is_valid_host_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
}
