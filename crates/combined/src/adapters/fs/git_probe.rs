//! Reads what `<dir>/.git` is from the filesystem.

use std::path::Path;

use crate::ports::{GitEntry, GitProbe};

pub struct FsGitProbe;

impl GitProbe for FsGitProbe {
    fn git_entry(&self, dir: &str) -> GitEntry {
        match std::fs::metadata(Path::new(dir).join(".git")) {
            Ok(meta) if meta.is_dir() => GitEntry::Directory,
            Ok(meta) if meta.is_file() => GitEntry::File,
            _ => GitEntry::Absent,
        }
    }
}
