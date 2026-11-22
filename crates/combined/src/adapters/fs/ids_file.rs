//! `ids.json`: how far the daemon has reserved attachment IDs.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::safe_write::write_atomic;

const VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdsFile {
    pub version: u32,
    pub ids_reserved_through: u64,
}

impl IdsFile {
    /// A version 1 file reserving IDs through `ids_reserved_through`.
    pub fn new(ids_reserved_through: u64) -> IdsFile {
        IdsFile {
            version: VERSION,
            ids_reserved_through,
        }
    }
}

#[derive(Debug)]
pub enum IdsFileError {
    Unreadable { path: PathBuf, source: io::Error },
    Invalid { path: PathBuf, detail: String },
}

impl fmt::Display for IdsFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const KEEP: &str = "the daemon will not reset it, because that would reuse attachment IDs; fix or restore it";
        match self {
            IdsFileError::Unreadable { path, source } => {
                write!(f, "cannot read {}: {source}; {KEEP}", path.display())
            }
            IdsFileError::Invalid { path, detail } => {
                write!(f, "{} is invalid: {detail}; {KEEP}", path.display())
            }
        }
    }
}

impl std::error::Error for IdsFileError {}

/// `None` if the file doesn't exist; anything else wrong with it is an error.
pub fn load(path: &Path) -> Result<Option<IdsFile>, IdsFileError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(IdsFileError::Unreadable {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let invalid = |detail: String| IdsFileError::Invalid {
        path: path.to_path_buf(),
        detail,
    };
    let file: IdsFile =
        serde_json::from_slice(&bytes).map_err(|error| invalid(error.to_string()))?;
    if file.version != VERSION {
        return Err(invalid(format!(
            "version {} is not {VERSION}",
            file.version
        )));
    }
    Ok(Some(file))
}

pub fn store(path: &Path, file: &IdsFile) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(file).map_err(io::Error::from)?;
    write_atomic(path, &bytes, 0o600)
}
