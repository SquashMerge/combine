//! The filesystem adapter: the daemon's data directory, its identity and ID files, the start-up
//! config, the git probe, project folder checks, the on-disk file store and its watcher.

pub mod config;
pub mod data_dir;
pub mod file_store;
pub mod git_probe;
pub mod host_id;
pub mod ids_file;
pub mod project_dirs;
pub mod safe_write;
pub mod watcher;
