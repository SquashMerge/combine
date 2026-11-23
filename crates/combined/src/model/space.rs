//! Spaces: the folders terminals are grouped by.

use combine_protocol::TermId;

/// The space's folder path. Clients key it together with the host's ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SpaceId(pub String);

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
