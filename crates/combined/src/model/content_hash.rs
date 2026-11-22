//! Content hashes of files the daemon reads and writes.

/// A SHA-256 digest of a file's bytes. The adapter that reads the file computes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash(pub [u8; 32]);
