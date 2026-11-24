//! Traits core needs from the outside world: the terminal backend, the git probe, the file store, the scratch store, the wall clock, the project folder check and the diagram check.

mod backend;
mod diagram_check;
mod file_store;
mod git_probe;
mod project_dirs;
mod scratch_store;
mod wall_clock;

pub use backend::{
    AttachEvent, AttachEvents, AttachOptions, Attachment, BackendError, BackendResult,
    Capabilities, TerminalBackend,
};
pub use diagram_check::{
    DiagramCheck, DiagramCheckError, DiagramFailure, DiagramVerdict, FailureCode,
};
pub use file_store::{
    valid_store_path, DirListing, EntryKind, FileChange, FileMark, FileStore, ListedEntry,
    StoreError, StoreRevision, StoredFile, WriteOutcome,
};
pub use git_probe::{GitEntry, GitProbe};
pub use project_dirs::{DirRefusal, ProjectDirs};
pub use scratch_store::{ScratchDir, ScratchListing, ScratchStore};
pub use wall_clock::WallClock;
