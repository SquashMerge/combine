//! What a directory's `.git` entry is.

/// What `<dir>/.git` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitEntry {
    Absent,
    Directory,
    File,
}

/// What `<dir>/.git` is. Implemented with I/O by an adapter, in memory by tests.
pub trait GitProbe: Send + Sync {
    fn git_entry(&self, dir: &str) -> GitEntry;
}
