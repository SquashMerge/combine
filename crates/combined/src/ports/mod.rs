//! Traits core needs from the outside world: the terminal backend, the git probe and the file store.

mod backend;
mod file_store;
mod git_probe;

pub use backend::{
    AttachEvent, AttachEvents, AttachOptions, Attachment, BackendError, BackendResult,
    Capabilities, TerminalBackend,
};
pub use file_store::{FileStore, StoreError, StoreRevision, StoredFile};
pub use git_probe::{GitEntry, GitProbe};
