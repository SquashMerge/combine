//! Where a host's client state file lives.

use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidHostId;

impl fmt::Display for InvalidHostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("host ID is not a lowercase UUID")
    }
}

impl std::error::Error for InvalidHostId {}

/// A lowercase hyphenated UUID shape: 36 chars, `-` at 8, 13, 18 and 23, lowercase hex elsewhere.
pub fn is_valid_host_id(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

/// `<data_dir>/hosts/<host_id>.json`. The ID is checked first, so it can never carry a path separator.
pub fn client_state_path(data_dir: &Path, host_id: &str) -> Result<PathBuf, InvalidHostId> {
    if !is_valid_host_id(host_id) {
        return Err(InvalidHostId);
    }
    Ok(data_dir.join("hosts").join(format!("{host_id}.json")))
}
