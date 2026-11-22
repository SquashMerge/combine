//! Reports changes to each watched space's `.workspace/layout.json`.
//!
//! The watcher covers `<space>` and `<space>/.workspace/`, both non-recursively, so it sees the
//! file being written, renamed over or removed, and the folder being created after the watch
//! began. [`classify`] turns a raw notify event into the changes the daemon cares about;
//! everything else, including the writer's own temp files, is dropped.

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;

use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::UnboundedSender;

use crate::model::SpaceId;
use crate::ports::FileChange;

const LAYOUT_FILE: &str = "layout.json";
const WORKSPACE: &str = ".workspace";

struct WatchedSpace {
    space: SpaceId,
    /// Every spelling of the space's `.workspace/` that an event may carry: the one the space
    /// names, and the canonical one, which is what the platform reports on a symlinked prefix.
    workspaces: Vec<PathBuf>,
    /// True while `.workspace/` itself is being watched.
    armed: bool,
}

impl WatchedSpace {
    fn names_workspace(&self, path: &Path) -> bool {
        self.workspaces.iter().any(|workspace| workspace == path)
    }
}

/// The spaces being watched.
#[derive(Default)]
pub struct WatchRoots {
    spaces: Vec<WatchedSpace>,
}

impl WatchRoots {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a space. False when it was already there.
    pub fn add(&mut self, space: &SpaceId) -> bool {
        if self.spaces.iter().any(|watched| &watched.space == space) {
            return false;
        }
        let folder = Path::new(&space.0);
        let mut workspaces = vec![folder.join(WORKSPACE)];
        if let Ok(canonical) = std::fs::canonicalize(folder) {
            let workspace = canonical.join(WORKSPACE);
            if !workspaces.contains(&workspace) {
                workspaces.push(workspace);
            }
        }
        self.spaces.push(WatchedSpace {
            space: space.clone(),
            workspaces,
            armed: false,
        });
        true
    }
}

/// The `layout.json` changes in `event`: the exact name, directly under a watched space's
/// `.workspace/`. A name starting with `.` is a temp file and is never reported. Every event
/// kind counts (a rename onto the file and a removal included) except the ones that only
/// observe a read, which the daemon's own reads would otherwise feed back.
pub fn classify(event: &Event, roots: &WatchRoots) -> Vec<FileChange> {
    if matches!(
        event.kind,
        EventKind::Access(
            AccessKind::Open(_) | AccessKind::Read | AccessKind::Close(AccessMode::Read)
        )
    ) {
        return Vec::new();
    }
    let mut changes = Vec::new();
    for path in &event.paths {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with('.') || name != LAYOUT_FILE {
            continue;
        }
        let Some(parent) = path.parent() else {
            continue;
        };
        let Some(watched) = roots
            .spaces
            .iter()
            .find(|watched| watched.names_workspace(parent))
        else {
            continue;
        };
        let change = layout_change(&watched.space);
        if !changes.contains(&change) {
            changes.push(change);
        }
    }
    changes
}

fn layout_change(space: &SpaceId) -> FileChange {
    FileChange {
        space: space.clone(),
        path: LAYOUT_FILE.to_string(),
    }
}

struct Shared {
    roots: Mutex<WatchRoots>,
    /// Held while a watch is added. The event handler never takes it: notify runs the handler
    /// on the thread that serves `watch`, so waiting on a watch from the handler would hang.
    watcher: Mutex<Option<RecommendedWatcher>>,
    changes: UnboundedSender<FileChange>,
}

impl Shared {
    fn roots(&self) -> MutexGuard<'_, WatchRoots> {
        self.roots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Watches `space`'s `.workspace/` when it exists and isn't watched yet. True when this
    /// call began watching it.
    fn arm(&self, space: &SpaceId) -> bool {
        let mut guard = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(watcher) = guard.as_mut() else {
            return false;
        };
        let workspace = {
            let roots = self.roots();
            match roots.spaces.iter().find(|watched| &watched.space == space) {
                Some(watched) if !watched.armed => Path::new(&watched.space.0).join(WORKSPACE),
                _ => return false,
            }
        };
        if watcher
            .watch(&workspace, RecursiveMode::NonRecursive)
            .is_err()
        {
            return false;
        }
        if let Some(watched) = self
            .roots()
            .spaces
            .iter_mut()
            .find(|watched| &watched.space == space)
        {
            watched.armed = true;
        }
        true
    }
}

/// One notify watcher for every watched space. Dropping it stops the watching.
pub struct SpaceWatcher {
    shared: Arc<Shared>,
}

impl SpaceWatcher {
    pub fn new(changes: UnboundedSender<FileChange>) -> notify::Result<SpaceWatcher> {
        let shared = Arc::new(Shared {
            roots: Mutex::new(WatchRoots::new()),
            watcher: Mutex::new(None),
            changes,
        });
        let (rearm, requests) = mpsc::channel();
        let weak = Arc::downgrade(&shared);
        let watcher = notify::recommended_watcher(move |result| handle(&weak, &rearm, result))?;
        *shared
            .watcher
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(watcher);
        let weak = Arc::downgrade(&shared);
        thread::Builder::new()
            .name("layout-rearm".to_string())
            .spawn(move || rearm_loop(&weak, &requests))
            .map_err(notify::Error::io)?;
        Ok(SpaceWatcher { shared })
    }

    /// Starts watching `space`'s folder and, when it exists, its `.workspace/`.
    pub fn watch(&self, space: &SpaceId) -> notify::Result<()> {
        if !self.shared.roots().add(space) {
            return Ok(());
        }
        {
            let mut guard = self
                .shared
                .watcher
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(watcher) = guard.as_mut() {
                watcher.watch(Path::new(&space.0), RecursiveMode::NonRecursive)?;
            }
        }
        self.shared.arm(space);
        Ok(())
    }
}

/// Watches the `.workspace/` folders the handler saw appear, and reports `layout.json` for
/// each, since the file may have been written before the watch began. Ends when the watcher
/// that owns the handler's sender is dropped.
fn rearm_loop(shared: &Weak<Shared>, requests: &mpsc::Receiver<SpaceId>) {
    for space in requests {
        let Some(shared) = shared.upgrade() else {
            return;
        };
        if shared.arm(&space) {
            let _ = shared.changes.send(layout_change(&space));
        }
    }
}

fn handle(shared: &Weak<Shared>, rearm: &mpsc::Sender<SpaceId>, result: notify::Result<Event>) {
    let Some(shared) = shared.upgrade() else {
        return;
    };
    let mut wanted = Vec::new();
    let changes = {
        let mut roots = shared.roots();
        let Ok(event) = &result else {
            let all: Vec<_> = roots
                .spaces
                .iter()
                .map(|watched| layout_change(&watched.space))
                .collect();
            drop(roots);
            for change in all {
                let _ = shared.changes.send(change);
            }
            return;
        };
        for watched in &mut roots.spaces {
            if !event.paths.iter().any(|path| watched.names_workspace(path)) {
                continue;
            }
            if matches!(event.kind, EventKind::Remove(_)) {
                watched.armed = false;
            } else if !watched.armed {
                wanted.push(watched.space.clone());
            }
        }
        classify(event, &roots)
    };
    for change in changes {
        let _ = shared.changes.send(change);
    }
    for space in wanted {
        let _ = rearm.send(space);
    }
}
