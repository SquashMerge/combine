//! The check that a project subfolder is a usable directory.

use crate::model::SpaceId;

/// Why a folder was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirRefusal {
    Missing,
    NotADirectory,
    /// With symlinks followed, the folder lies outside the project.
    Outside,
    /// The folder is, or lies inside, a `.git` directory.
    InsideGit,
    Unavailable(String),
}

/// Checks folders of a project. Implemented with I/O by an adapter.
pub trait ProjectDirs: Send + Sync {
    /// `relative` is `""` (the root itself) or `'/'`-separated with no empty, `.` or `..`
    /// segment. Succeeds only when `<root>/<relative>`, with symlinks followed, is a directory
    /// inside the canonical root and not inside a `.git` directory.
    fn check_dir(&self, root: &SpaceId, relative: &str) -> Result<(), DirRefusal>;
}
