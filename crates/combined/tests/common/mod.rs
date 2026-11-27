#![allow(dead_code)]
//! Fixtures shared by the daemon's test crates.

use std::collections::{BTreeMap, BTreeSet};

use combine_protocol::{AttachRequestOptions, BackendInfo, Display};
use combined::model::SpawnSpec;
use combined::ports::{GitEntry, GitProbe};

pub fn display() -> Display {
    Display {
        foreground: "#d4d4d4".to_string(),
        background: "#1e1e1e".to_string(),
        cursor_color: "#ffffff".to_string(),
        cell_width: 8,
        cell_height: 16,
    }
}

pub fn request_options(cols: u16, rows: u16, observe: bool) -> AttachRequestOptions {
    AttachRequestOptions {
        cols,
        rows,
        display: display(),
        observe,
        history: 0,
    }
}

pub fn spawn_spec(cwd: &str, cols: u16, rows: u16) -> SpawnSpec {
    SpawnSpec {
        argv: vec!["sh".to_string()],
        cwd: cwd.to_string(),
        env: BTreeMap::new(),
        labels: BTreeMap::new(),
        cols,
        rows,
    }
}

/// The backend a terminal list names: the fake's ID and instance.
pub fn fake_backend_info() -> BackendInfo {
    BackendInfo {
        id: "fake".to_string(),
        instance: Some("fake-core".to_string()),
    }
}

/// An in-memory [`GitProbe`]: a repository has a `.git` directory, a worktree a `.git` file,
/// and any other folder neither. Directories match as exact strings.
#[derive(Debug, Default, Clone)]
pub struct FakeGitProbe {
    repositories: BTreeSet<String>,
    worktrees: BTreeSet<String>,
}

impl FakeGitProbe {
    pub fn with_repository(mut self, dir: &str) -> Self {
        self.repositories.insert(dir.to_string());
        self
    }

    pub fn with_worktree(mut self, dir: &str) -> Self {
        self.worktrees.insert(dir.to_string());
        self
    }
}

impl GitProbe for FakeGitProbe {
    fn git_entry(&self, dir: &str) -> GitEntry {
        if self.repositories.contains(dir) {
            GitEntry::Directory
        } else if self.worktrees.contains(dir) {
            GitEntry::File
        } else {
            GitEntry::Absent
        }
    }
}
