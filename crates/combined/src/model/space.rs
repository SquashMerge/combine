//! Spaces: the folders terminals are grouped by.

use combine_protocol::TermId;
use sha2::{Digest, Sha256};

/// The space's folder path. Clients key it together with the host's ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SpaceId(pub String);

impl SpaceId {
    /// The lowercase hex SHA-256 of the space ID's bytes: the folder name that stands for the
    /// space under the fallback root and the scratch root.
    pub fn sha256_hex(&self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut hex = String::with_capacity(64);
        for byte in Sha256::digest(self.0.as_bytes()) {
            hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
            hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
        }
        hex
    }
}

/// What kind of root a space's folder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKind {
    Repository,
    Worktree,
    Folder,
}

/// A folder and the terminals that started in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Space {
    pub id: SpaceId,
    pub kind: RootKind,
    /// In the backend's list order; terminals created later are appended.
    pub terminals: Vec<TermId>,
}
