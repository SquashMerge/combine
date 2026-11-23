//! Traits core needs from the outside world: the terminal backend, the git probe, the file store and the project folder check.

mod backend;
mod file_store;
mod git_probe;
mod project_dirs;

pub use backend::{
    AttachEvent, AttachEvents, AttachOptions, Attachment, BackendError, BackendResult,
    Capabilities, TerminalBackend,
};
pub use file_store::{
    valid_store_path, DirListing, EntryKind, FileChange, FileStore, ListedEntry, StoreError,
    StoreRevision, StoredFile, WriteOutcome,
};
pub use git_probe::{GitEntry, GitProbe};
pub use project_dirs::{DirRefusal, ProjectDirs};
