//! Reports changes to each watched space's `.workspace/layout.json` and to its panel files.
//!
//! The watcher covers `<space>` non-recursively and `<space>/.workspace/` recursively, so it
//! sees the file being written, renamed over or removed, panel folders and files below it, and
//! the folder being created after the watch began. [`classify`] turns a raw notify event into
//! the changes the daemon cares about; everything else, including the writer's own temp files,
//! is dropped. The scratch area is the daemon's own and is not watched.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::UnboundedSender;

use crate::model::SpaceId;
use crate::ports::FileChange;

const LAYOUT_FILE: &str = "layout.json";
const WORKSPACE: &str = ".workspace";
const PANELS_DIR: &str = "panels";
const PANEL_FILES: [&str; 3] = ["panel.json", "diagram.mmd", "content.md"];

/// How often a `.workspace/` that failed to arm after it appeared is tried again.
const ARM_RETRY: Duration = Duration::from_secs(30);

struct WatchedSpace {
    space: SpaceId,
    /// Every spelling of the space's `.workspace/` that an event may carry: the one the space
    /// names, and the canonical one, which is what the platform reports on a symlinked prefix.
    workspaces: Vec<PathBuf>,
    /// True while `.workspace/` itself is being watched.
    armed: bool,
    /// Why the last attempt to watch an existing `.workspace/` failed, until one succeeds.
    arm_error: Option<String>,
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
            arm_error: None,
        });
        true
    }

    fn remove(&mut self, space: &SpaceId) {
        self.spaces.retain(|watched| &watched.space != space);
    }
}

/// The changes in `event`, for the paths under a watched space's `.workspace/`:
/// - `layout.json` reports `layout.json`;
/// - `panels` and `panels/<id>` (a panel folder created, removed, renamed or replaced) report
///   `panels`;
/// - `panels/<id>/<file>` for `panel.json`, `diagram.mmd` and `content.md` reports
///   `panels/<id>/<file>`.
///
/// A path with any segment starting with `.` (a temp file, `.DS_Store`, a folder renamed away)
/// is never reported, and neither is anything else. Every event kind counts (a rename onto the
/// file and a removal included) except the ones that only observe a read, which the daemon's own
/// reads would otherwise feed back. An event that asks for a rescan (events were lost) gives
/// `layout.json` and `panels` for every watched space, since a re-read that finds the same
/// bytes is ignored.
pub fn classify(event: &Event, roots: &WatchRoots) -> Vec<FileChange> {
    if event.need_rescan() {
        return all_changes(roots);
    }
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
        for watched in &roots.spaces {
            let Some(relative) = watched
                .workspaces
                .iter()
                .find_map(|workspace| path.strip_prefix(workspace).ok())
            else {
                continue;
            };
            let Some(change) = panel_or_layout_change(&watched.space, relative) else {
                continue;
            };
            if !changes.contains(&change) {
                changes.push(change);
            }
        }
    }
    changes
}

/// The change for `relative`, a path below a space's `.workspace/`, if it is one to report.
fn panel_or_layout_change(space: &SpaceId, relative: &Path) -> Option<FileChange> {
    let mut segments = Vec::new();
    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return None;
        };
        let segment = segment.to_str()?;
        if segment.starts_with('.') {
            return None;
        }
        segments.push(segment);
    }
    match segments[..] {
        [LAYOUT_FILE] => Some(layout_change(space)),
        [PANELS_DIR] | [PANELS_DIR, _] => Some(panels_change(space)),
        [PANELS_DIR, id, file] if PANEL_FILES.contains(&file) => Some(FileChange {
            space: space.clone(),
            path: format!("{PANELS_DIR}/{id}/{file}"),
        }),
        _ => None,
    }
}

fn layout_change(space: &SpaceId) -> FileChange {
    FileChange {
        space: space.clone(),
        path: LAYOUT_FILE.to_string(),
    }
}

fn panels_change(space: &SpaceId) -> FileChange {
    FileChange {
        space: space.clone(),
        path: PANELS_DIR.to_string(),
    }
}

/// `layout.json` then `panels` for one space.
fn space_changes(space: &SpaceId) -> [FileChange; 2] {
    [layout_change(space), panels_change(space)]
}

fn all_changes(roots: &WatchRoots) -> Vec<FileChange> {
    roots
        .spaces
        .iter()
        .flat_map(|watched| space_changes(&watched.space))
        .collect()
}

/// What the event handler and the re-arm thread share. It never owns the notify watcher: the
/// handler runs on the watcher's own thread, and dropping a watcher from there would hang.
struct Shared {
    roots: Mutex<WatchRoots>,
    changes: UnboundedSender<FileChange>,
    /// The number of successful [`arm`] calls.
    arms: AtomicU64,
    /// The paths registered with the notify watcher, each once.
    registered: Mutex<Vec<PathBuf>>,
}

impl Shared {
    fn roots(&self) -> MutexGuard<'_, WatchRoots> {
        self.roots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn registered(&self) -> MutexGuard<'_, Vec<PathBuf>> {
        self.registered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers `path` with `watcher`. A path that is already registered is unwatched first,
    /// so it never counts twice.
    fn register(
        &self,
        watcher: &mut RecommendedWatcher,
        path: &Path,
        mode: RecursiveMode,
    ) -> notify::Result<()> {
        let mut registered = self.registered();
        if let Some(index) = registered.iter().position(|known| known == path) {
            let _ = watcher.unwatch(path);
            registered.remove(index);
        }
        watcher.watch(path, mode)?;
        registered.push(path.to_path_buf());
        Ok(())
    }

    /// Stops watching `path` and forgets it.
    fn unregister(&self, watcher: &mut RecommendedWatcher, path: &Path) {
        let mut registered = self.registered();
        let _ = watcher.unwatch(path);
        registered.retain(|known| known != path);
    }

    /// The spaces whose existing `.workspace/` could not be watched.
    fn failed_arms(&self) -> Vec<SpaceId> {
        self.roots()
            .spaces
            .iter()
            .filter(|watched| watched.arm_error.is_some())
            .map(|watched| watched.space.clone())
            .collect()
    }

    fn arm_error(&self, space: &SpaceId) -> Option<String> {
        self.roots()
            .spaces
            .iter()
            .find(|watched| &watched.space == space)
            .and_then(|watched| watched.arm_error.clone())
    }
}

/// Held while a watch is added. The event handler never takes it: notify runs the handler on
/// the thread that serves `watch`, so waiting on a watch from the handler would hang.
type WatcherCell = Mutex<Option<RecommendedWatcher>>;

/// Watches `space`'s `.workspace/` when it exists and isn't watched yet. True when this call
/// began watching it. A `.workspace/` that doesn't exist is not a failure: the space's folder
/// is watched and arms it when it appears. Any other refusal, such as the platform's cap on
/// watched paths, is returned and kept on the space until a later call succeeds.
fn arm(shared: &Shared, cell: &WatcherCell, space: &SpaceId) -> notify::Result<bool> {
    let mut guard = cell.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(watcher) = guard.as_mut() else {
        return Ok(false);
    };
    let workspace = {
        let roots = shared.roots();
        match roots.spaces.iter().find(|watched| &watched.space == space) {
            Some(watched) if !watched.armed => Path::new(&watched.space.0).join(WORKSPACE),
            _ => return Ok(false),
        }
    };
    let registered = shared.register(watcher, &workspace, RecursiveMode::Recursive);
    let mut roots = shared.roots();
    let Some(watched) = roots
        .spaces
        .iter_mut()
        .find(|watched| &watched.space == space)
    else {
        return Ok(false);
    };
    if let Err(error) = registered {
        if matches!(
            std::fs::symlink_metadata(&workspace),
            Err(missing) if missing.kind() == std::io::ErrorKind::NotFound
        ) {
            return Ok(false);
        }
        watched.arm_error = Some(error.to_string());
        return Err(error);
    }
    watched.armed = true;
    watched.arm_error = None;
    drop(roots);
    shared.arms.fetch_add(1, Ordering::SeqCst);
    Ok(true)
}

/// One notify watcher for every watched space. Dropping it stops the watching.
pub struct SpaceWatcher {
    shared: Arc<Shared>,
    watcher: Arc<WatcherCell>,
}

impl SpaceWatcher {
    pub fn new(changes: UnboundedSender<FileChange>) -> notify::Result<SpaceWatcher> {
        Self::with_arm_retry(changes, ARM_RETRY)
    }

    /// [`SpaceWatcher::new`] with the interval at which a `.workspace/` that failed to arm is
    /// tried again named, for tests.
    pub fn with_arm_retry(
        changes: UnboundedSender<FileChange>,
        arm_retry: Duration,
    ) -> notify::Result<SpaceWatcher> {
        let shared = Arc::new(Shared {
            roots: Mutex::new(WatchRoots::new()),
            changes,
            arms: AtomicU64::new(0),
            registered: Mutex::new(Vec::new()),
        });
        let (rearm, requests) = mpsc::channel();
        let handler_shared = Arc::clone(&shared);
        let watcher =
            notify::recommended_watcher(move |result| handle(&handler_shared, &rearm, result))?;
        let cell = Arc::new(Mutex::new(Some(watcher)));
        let thread_shared = Arc::clone(&shared);
        let thread_cell = Arc::downgrade(&cell);
        thread::Builder::new()
            .name("layout-rearm".to_string())
            .spawn(move || rearm_loop(&thread_shared, &thread_cell, &requests, arm_retry))
            .map_err(notify::Error::io)?;
        Ok(SpaceWatcher {
            shared,
            watcher: cell,
        })
    }

    /// Starts watching `space`'s folder and, when it exists, its `.workspace/`. When either
    /// can't be watched the space is left as if it had never been added, so a later call tries
    /// again, and the error is returned. A call for a space whose `.workspace/` failed to arm
    /// after it appeared tries that arm again.
    pub fn watch(&self, space: &SpaceId) -> notify::Result<()> {
        if !self.shared.roots().add(space) {
            if self.shared.arm_error(space).is_none() {
                return Ok(());
            }
            if arm(&self.shared, &self.watcher, space)? {
                send_space_changes(&self.shared, space);
            }
            return Ok(());
        }
        let watched = {
            let mut guard = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
            match guard.as_mut() {
                Some(watcher) => {
                    self.shared
                        .register(watcher, Path::new(&space.0), RecursiveMode::NonRecursive)
                }
                None => Ok(()),
            }
        };
        if let Err(error) = watched {
            self.shared.roots().remove(space);
            return Err(error);
        }
        if let Err(error) = arm(&self.shared, &self.watcher, space) {
            let mut guard = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(watcher) = guard.as_mut() {
                self.shared.unregister(watcher, Path::new(&space.0));
            }
            self.shared.roots().remove(space);
            return Err(error);
        }
        Ok(())
    }

    /// True while `space`'s `.workspace/` is being watched.
    pub fn is_armed(&self, space: &SpaceId) -> bool {
        self.shared
            .roots()
            .spaces
            .iter()
            .any(|watched| &watched.space == space && watched.armed)
    }

    /// The number of times a `.workspace/` watch has been set up. A re-arm is counted before
    /// its synthetic changes are sent.
    pub fn arms(&self) -> u64 {
        self.shared.arms.load(Ordering::SeqCst)
    }

    /// The paths registered with the notify watcher, each once.
    pub fn watched_paths(&self) -> Vec<PathBuf> {
        self.shared.registered().clone()
    }
}

/// Watches the `.workspace/` folders the handler saw appear, and reports `layout.json` and
/// `panels` for each, since the files may have been written before the watch began. A folder
/// the platform refused to watch is tried again every `arm_retry`, on a deadline that requests
/// for other spaces don't move; the refusal is logged when it starts and when its text changes.
/// Ends when the watcher that owns the handler's sender is dropped.
fn rearm_loop(
    shared: &Shared,
    cell: &Weak<WatcherCell>,
    requests: &mpsc::Receiver<SpaceId>,
    arm_retry: Duration,
) {
    let mut next_retry = Instant::now() + arm_retry;
    loop {
        let mut spaces =
            match requests.recv_timeout(next_retry.saturating_duration_since(Instant::now())) {
                Ok(space) => vec![space],
                Err(mpsc::RecvTimeoutError::Timeout) => Vec::new(),
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
        if Instant::now() >= next_retry {
            next_retry = Instant::now() + arm_retry;
            for space in shared.failed_arms() {
                if !spaces.contains(&space) {
                    spaces.push(space);
                }
            }
        }
        for space in spaces {
            let Some(cell) = cell.upgrade() else {
                return;
            };
            let before = shared.arm_error(&space);
            match arm(shared, &cell, &space) {
                Ok(true) => send_space_changes(shared, &space),
                Ok(false) => {}
                Err(error) => {
                    if before.as_deref() != Some(error.to_string().as_str()) {
                        eprintln!("combined: cannot watch {}/{WORKSPACE}: {error}", space.0);
                    }
                }
            }
        }
    }
}

fn send_space_changes(shared: &Shared, space: &SpaceId) {
    for change in space_changes(space) {
        let _ = shared.changes.send(change);
    }
}

fn handle(shared: &Shared, rearm: &mpsc::Sender<SpaceId>, result: notify::Result<Event>) {
    let mut wanted = Vec::new();
    let changes = {
        let mut roots = shared.roots();
        let Ok(event) = &result else {
            let all = all_changes(&roots);
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
