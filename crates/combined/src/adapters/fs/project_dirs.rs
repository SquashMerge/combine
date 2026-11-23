//! The project folder check on disk.
//!
//! This is not race-free: a process that can modify the project while the check runs can swap a
//! folder for a symlink between the check and the later use, as for the file store.

use std::io;
use std::path::{Component, Path};

use crate::model::SpaceId;
use crate::ports::{valid_store_path, DirRefusal, ProjectDirs};

/// Checks project folders with the real filesystem.
pub struct FsProjectDirs;

/// `ENOTDIR`, which `ErrorKind::NotADirectory` reports on current toolchains only.
fn not_a_directory(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotADirectory || error.raw_os_error() == Some(20)
}

impl ProjectDirs for FsProjectDirs {
    fn check_dir(&self, root: &SpaceId, relative: &str) -> Result<(), DirRefusal> {
        let root_path = Path::new(&root.0);
        if !root_path.is_absolute() {
            return Err(DirRefusal::Unavailable(format!(
                "space {} is not an absolute folder",
                root.0
            )));
        }
        if !relative.is_empty() && !valid_store_path(relative) {
            return Err(DirRefusal::Unavailable(format!(
                "{relative:?} is not a valid relative folder"
            )));
        }
        let canonical_root = std::fs::canonicalize(root_path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                DirRefusal::Missing
            } else {
                DirRefusal::Unavailable(error.to_string())
            }
        })?;
        let folder = std::fs::canonicalize(root_path.join(relative)).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                DirRefusal::Missing
            } else if not_a_directory(&error) {
                DirRefusal::NotADirectory
            } else {
                DirRefusal::Unavailable(error.to_string())
            }
        })?;
        let inside = folder
            .strip_prefix(&canonical_root)
            .map_err(|_| DirRefusal::Outside)?;
        let in_git = inside.components().any(|part| match part {
            Component::Normal(name) => name.as_encoded_bytes().eq_ignore_ascii_case(b".git"),
            _ => false,
        });
        if in_git {
            return Err(DirRefusal::InsideGit);
        }
        match std::fs::metadata(&folder) {
            Ok(meta) if meta.is_dir() => Ok(()),
            Ok(_) => Err(DirRefusal::NotADirectory),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Err(DirRefusal::Missing),
            Err(error) => Err(DirRefusal::Unavailable(error.to_string())),
        }
    }
}
