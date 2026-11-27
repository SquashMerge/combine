//! Reports changes to each watched space's `.workspace/layout.json` and to its panel files.
//!
//! The watcher covers `<space>` non-recursively and `<space>/.workspace/` recursively, so it
//! sees the file being written, renamed over or removed, panel folders and files below it, and
//! the folder being created after the watch began. [`classify`] turns a raw notify event into
//! the changes the daemon cares about; everything else, including the writer's own temp files,
//! is dropped. The scratch area is the daemon's own and is not watched.
//!
//! Whether a `.workspace/` is watched never rests on one event alone. Under load FSEvents can
//! report a replaced folder as only a creation, without its removal, and can drop or hold back
//! events, so an event naming the folder, a rescan and a watcher error each make the re-arm
//! thread compare the watch with the disk, and so does a periodic check. The watch remembers
//! which folder (device and inode) it armed, so a replaced `.workspace/` is armed again.

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

/// How often the watches are compared with the disk: a `.workspace/` that failed to arm is
/// tried again, one that exists but isn't watched (its creation was never reported) or was
/// replaced since it was armed is armed, and one that is watched but gone is marked as not
/// watched.
const ARM_RETRY: Duration = Duration::from_secs(30);

struct WatchedSpace {
    space: SpaceId,
    /// Every spelling of the space's `.workspace/` that an event may carry: the one the space
    /// names, and the canonical one, which is what the platform reports on a symlinked prefix.
    workspaces: Vec<PathBuf>,
    /// The folder (device and inode) that was armed, while `.workspace/` itself is watched.
    armed: Option<FolderId>,
    /// Why the last attempt to watch an existing `.workspace/` failed, until one succeeds.
    arm_error: Option<String>,
}

/// A folder's device and inode, which tell a replaced `.workspace/` from the one that was armed.
type FolderId = (u64, u64);

/// The identity of the folder at `path`, following a symlink as the watch does. None when
/// nothing is there.
fn folder_id(path: &Path) -> Option<FolderId> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()))
}

impl WatchedSpace {
    fn workspace(&self) -> PathBuf {
        Path::new(&self.space.0).join(WORKSPACE)
    }

    fn names_workspace(&self, path: &Path) -> bool {
        self.workspaces.iter().any(|workspace| workspace == path)
    }
}

/// Every spelling of `space`'s `.workspace/` an event may carry: the one the space names, and
/// the canonical one. Reads the filesystem.
fn workspace_spellings(space: &SpaceId) -> Vec<PathBuf> {
    let folder = Path::new(&space.0);
    let mut workspaces = vec![folder.join(WORKSPACE)];
    if let Ok(canonical) = std::fs::canonicalize(folder) {
        let workspace = canonical.join(WORKSPACE);
        if !workspaces.contains(&workspace) {
            workspaces.push(workspace);
        }
    }
    workspaces
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
        self.add_spelled(space, workspace_spellings(space))
    }

    /// [`WatchRoots::add`] with the `.workspace/` spellings already resolved, so a caller that
    /// holds the roots' lock does no filesystem work under it.
    fn add_spelled(&mut self, space: &SpaceId, workspaces: Vec<PathBuf>) -> bool {
        if self.spaces.iter().any(|watched| &watched.space == space) {
            return false;
        }
        self.spaces.push(WatchedSpace {
            space: space.clone(),
            workspaces,
            armed: None,
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

/// What a test is told of each registration.
#[cfg(test)]
type OnRegister = Box<dyn Fn(&Path) + Send + Sync>;

/// Looks up a folder's identity; [`folder_id`] outside tests.
type Identify = Box<dyn Fn(&Path) -> Option<FolderId> + Send + Sync>;

/// What the event handler and the re-arm thread share. It never owns the notify watcher: the
/// handler runs on the watcher's own thread, and dropping a watcher from there would hang.
///
/// The handler takes `roots` for every event, so nothing holds it across filesystem work or a
/// call into notify: such work reads what it needs under the lock, lets go, does the work, and
/// takes the lock again to apply the result only if the space is still as it was read.
struct Shared {
    roots: Mutex<WatchRoots>,
    identify: Identify,
    changes: UnboundedSender<FileChange>,
    /// The number of successful [`arm`] calls.
    arms: AtomicU64,
    /// The paths registered with the notify watcher, each once.
    registered: Mutex<Vec<PathBuf>>,
    /// Told each path just after notify accepted a watch on it.
    #[cfg(test)]
    on_register: Option<OnRegister>,
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
        #[cfg(test)]
        if let Some(on_register) = &self.on_register {
            on_register(path);
        }
        registered.push(path.to_path_buf());
        Ok(())
    }

    /// Stops watching `path` and forgets it.
    fn unregister(&self, watcher: &mut RecommendedWatcher, path: &Path) {
        let mut registered = self.registered();
        let _ = watcher.unwatch(path);
        registered.retain(|known| known != path);
    }

    /// `space`'s `.workspace/` and the folder armed for it, read under the lock. None when the
    /// space isn't watched.
    fn armed_state(&self, space: &SpaceId) -> Option<(PathBuf, Option<FolderId>)> {
        self.roots()
            .spaces
            .iter()
            .find(|watched| &watched.space == space)
            .map(|watched| (watched.workspace(), watched.armed))
    }

    /// The spaces whose watch doesn't match the disk, each with whether an event about its
    /// `.workspace/` was missed: one that failed to arm, one that exists but isn't watched or
    /// was replaced since it was armed (an event missed), and one that is watched but gone.
    fn unsettled(&self) -> Vec<(SpaceId, bool)> {
        let spaces: Vec<(SpaceId, PathBuf, Option<FolderId>, bool)> = self
            .roots()
            .spaces
            .iter()
            .map(|watched| {
                (
                    watched.space.clone(),
                    watched.workspace(),
                    watched.armed,
                    watched.arm_error.is_some(),
                )
            })
            .collect();
        spaces
            .into_iter()
            .filter_map(|(space, workspace, armed, refused)| {
                let found = (self.identify)(&workspace);
                let missed = found.is_some() && found != armed && !refused;
                (refused || found != armed).then_some((space, missed))
            })
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

/// Watches `space`'s `.workspace/` when it exists and isn't watched yet, and remembers which
/// folder it watched. True when this call began watching it. A `.workspace/` that doesn't
/// exist is not a failure: the space's folder is watched and arms it when it appears. Any
/// other refusal, such as the platform's cap on watched paths, is returned and kept on the
/// space until a later call succeeds.
///
/// It holds the watcher's cell across the call into notify, which serializes arms, but not
/// `roots`: the folder's identity and the outcome are worked out first, and applied only if
/// the space is still watched and still not armed. The identity it records is always the
/// folder notify was asked to watch.
fn arm(shared: &Shared, cell: &WatcherCell, space: &SpaceId) -> notify::Result<bool> {
    let mut guard = cell.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(watcher) = guard.as_mut() else {
        return Ok(false);
    };
    let workspace = match shared.armed_state(space) {
        Some((workspace, None)) => workspace,
        _ => return Ok(false),
    };
    for _ in 0..ARM_TRIES {
        // The folder notify watches is the one at the path while `watch` runs. Only a folder
        // found both just before and just after the watch was set up is certain to be it: one
        // that appeared, or was replaced, meanwhile may not be, so the watch is set up again.
        let before = (shared.identify)(&workspace);
        let outcome = match shared.register(watcher, &workspace, RecursiveMode::Recursive) {
            Ok(()) => match (before, (shared.identify)(&workspace)) {
                (Some(before), Some(after)) if before == after => Ok(before),
                _ => continue,
            },
            Err(_)
                if matches!(
                    std::fs::symlink_metadata(&workspace),
                    Err(missing) if missing.kind() == std::io::ErrorKind::NotFound
                ) =>
            {
                return Ok(false);
            }
            Err(error) => Err(error),
        };
        let mut roots = shared.roots();
        let Some(watched) = roots
            .spaces
            .iter_mut()
            .find(|watched| &watched.space == space)
        else {
            return Ok(false);
        };
        return match outcome {
            Err(error) => {
                watched.arm_error = Some(error.to_string());
                Err(error)
            }
            // Arms hold the cell, so nothing armed it meanwhile; checked all the same.
            Ok(_) if watched.armed.is_some() => Ok(false),
            Ok(found) => {
                watched.armed = Some(found);
                watched.arm_error = None;
                drop(roots);
                shared.arms.fetch_add(1, Ordering::SeqCst);
                Ok(true)
            }
        };
    }
    // The folder kept changing under the watch. The space stays unarmed, so the next request
    // or check settles it.
    Ok(false)
}

/// How many times [`arm`] sets up a watch whose folder changed while it was set up, before
/// leaving the space unarmed for the next request or check.
const ARM_TRIES: usize = 2;

/// How many times [`settle`] reads a space again when it changed while its folder was looked
/// up, before leaving it to the next request or check.
const SETTLE_TRIES: usize = 3;

/// Brings `space`'s watch in line with its `.workspace/` on disk: one that is watched but gone
/// is marked as not watched, one that was replaced since it was armed is armed again, and
/// [`arm`] watches one that exists and isn't watched. True when this call began watching it.
///
/// The folder is looked up without `roots` held. If the space changed meanwhile (armed,
/// disarmed or no longer watched), the lookup is stale: it is dropped and the space read again.
fn settle(shared: &Shared, cell: &WatcherCell, space: &SpaceId) -> notify::Result<bool> {
    for _ in 0..SETTLE_TRIES {
        let Some((workspace, armed)) = shared.armed_state(space) else {
            return Ok(false);
        };
        let Some(armed) = armed else {
            return arm(shared, cell, space);
        };
        let found = (shared.identify)(&workspace);
        let mut roots = shared.roots();
        let Some(watched) = roots
            .spaces
            .iter_mut()
            .find(|watched| &watched.space == space)
        else {
            return Ok(false);
        };
        if watched.armed != Some(armed) {
            continue;
        }
        match found {
            Some(found) if found == armed => return Ok(false),
            Some(_) => {
                watched.armed = None;
                drop(roots);
                return arm(shared, cell, space);
            }
            None => {
                watched.armed = None;
                return Ok(false);
            }
        }
    }
    Ok(false)
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

    /// [`SpaceWatcher::new`] with the interval of its check of the watches against the disk
    /// (see [`ARM_RETRY`]) named, for tests.
    pub fn with_arm_retry(
        changes: UnboundedSender<FileChange>,
        arm_retry: Duration,
    ) -> notify::Result<SpaceWatcher> {
        let shared = Arc::new(Shared {
            roots: Mutex::new(WatchRoots::new()),
            identify: Box::new(folder_id),
            changes,
            arms: AtomicU64::new(0),
            registered: Mutex::new(Vec::new()),
            #[cfg(test)]
            on_register: None,
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
        let spellings = workspace_spellings(space);
        if !self.shared.roots().add_spelled(space, spellings) {
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
            .any(|watched| &watched.space == space && watched.armed.is_some())
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

/// Settles the spaces the handler asked about (see [`settle`]), and reports `layout.json` and
/// `panels` for each one it arms, since the files may have been written before the watch
/// began. Every `arm_retry` it also settles every space whose watch doesn't match the disk
/// ([`check`]), on a deadline that requests don't move. A refusal is logged when it starts and
/// when its text changes. Ends when the watcher that owns the handler's sender is dropped.
fn rearm_loop(
    shared: &Shared,
    cell: &Weak<WatcherCell>,
    requests: &mpsc::Receiver<SpaceId>,
    arm_retry: Duration,
) {
    let mut next_check = Instant::now() + arm_retry;
    loop {
        let requested =
            match requests.recv_timeout(next_check.saturating_duration_since(Instant::now())) {
                Ok(space) => Some(space),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
        let Some(cell) = cell.upgrade() else {
            return;
        };
        if let Some(space) = requested {
            settle_and_report(shared, &cell, &space);
        }
        if Instant::now() >= next_check {
            next_check = Instant::now() + arm_retry;
            check(shared, &cell);
        }
    }
}

/// Settles every space whose watch doesn't match the disk. A `.workspace/` armed here that no
/// event reported, and that was never refused, is logged: its creation event was lost.
fn check(shared: &Shared, cell: &WatcherCell) {
    for (space, missed) in shared.unsettled() {
        if settle_and_report(shared, cell, &space) && missed {
            eprintln!(
                "combined: watching {}/{WORKSPACE}, whose creation was not reported",
                space.0
            );
        }
    }
}

/// [`settle`]s `space`, sends its changes when that armed it, and logs a refusal that is new or
/// whose text changed. True when it armed the space.
fn settle_and_report(shared: &Shared, cell: &WatcherCell, space: &SpaceId) -> bool {
    let before = shared.arm_error(space);
    match settle(shared, cell, space) {
        Ok(true) => {
            send_space_changes(shared, space);
            true
        }
        Ok(false) => false,
        Err(error) => {
            if before.as_deref() != Some(error.to_string().as_str()) {
                eprintln!("combined: cannot watch {}/{WORKSPACE}: {error}", space.0);
            }
            false
        }
    }
}

fn send_space_changes(shared: &Shared, space: &SpaceId) {
    for change in space_changes(space) {
        let _ = shared.changes.send(change);
    }
}

/// Sends the event's changes, and asks the re-arm thread to settle (see [`settle`]) each space
/// whose `.workspace/` the event names, whatever its kind: the disk decides, since a folder
/// replaced under load can arrive as a creation alone, without its removal. A rescan or a
/// watcher error, after which any `.workspace/` may have come, gone or been replaced
/// unreported, asks about every space.
fn handle(shared: &Shared, rearm: &mpsc::Sender<SpaceId>, result: notify::Result<Event>) {
    let (changes, wanted) = {
        let roots = shared.roots();
        match &result {
            Ok(event) if !event.need_rescan() => {
                let wanted: Vec<SpaceId> = roots
                    .spaces
                    .iter()
                    .filter(|watched| event.paths.iter().any(|path| watched.names_workspace(path)))
                    .map(|watched| watched.space.clone())
                    .collect();
                (classify(event, &roots), wanted)
            }
            _ => {
                let wanted = roots
                    .spaces
                    .iter()
                    .map(|watched| watched.space.clone())
                    .collect();
                (all_changes(&roots), wanted)
            }
        }
    };
    for change in changes {
        let _ = shared.changes.send(change);
    }
    for space in wanted {
        let _ = rearm.send(space);
    }
}

#[cfg(test)]
mod tests {
    //! The re-arm decisions, driven without waiting on FSEvents: the events are built here, and
    //! the notify watcher's own events are ignored, so a `.workspace/` created on disk is one
    //! whose creation was lost.

    use super::*;
    use notify::event::{CreateKind, Flag, ModifyKind, RemoveKind};
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    /// A new directory `<parent>/<prefix>-<8 random hex digits>`, mode 0700. As with `mkdtemp`, a
    /// name that exists (a killed run's leftover: pids are reused) is retried, never reused.
    /// A copy of `crate::testing::fresh_dir`, which unit tests without `test-support` lack.
    fn fresh_dir(parent: &Path, prefix: &str) -> PathBuf {
        use std::hash::{BuildHasher, Hasher};
        use std::os::unix::fs::DirBuilderExt;
        for _ in 0..16 {
            let suffix = std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish() as u32;
            let path = parent.join(format!("{prefix}-{suffix:08x}"));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                created => {
                    created.unwrap_or_else(|error| panic!("creating {}: {error}", path.display()));
                    return path;
                }
            }
        }
        panic!("no free {prefix}-* name under {}", parent.display());
    }

    /// A new folder under the temp dir, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Scratch {
            Scratch(fresh_dir(&std::env::temp_dir(), "combined-watcher"))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct Rig {
        shared: Shared,
        cell: WatcherCell,
        changes: UnboundedReceiver<FileChange>,
        space: SpaceId,
        workspace: PathBuf,
        _scratch: Scratch,
    }

    /// One unarmed space with no `.workspace/`, and a notify watcher whose events go nowhere.
    fn rig() -> Rig {
        rig_with(Box::new(folder_id))
    }

    /// [`rig`] with `identify` standing in for [`folder_id`].
    fn rig_with(identify: Identify) -> Rig {
        let scratch = Scratch::new();
        let space = SpaceId(scratch.0.to_str().unwrap().to_string());
        let (tx, changes) = unbounded_channel();
        let shared = Shared {
            roots: Mutex::new(WatchRoots::new()),
            identify,
            changes: tx,
            arms: AtomicU64::new(0),
            registered: Mutex::new(Vec::new()),
            on_register: None,
        };
        assert!(shared.roots().add(&space));
        let watcher = notify::recommended_watcher(|_: notify::Result<Event>| {}).unwrap();
        let cell = Mutex::new(Some(watcher));
        let workspace = scratch.0.join(WORKSPACE);
        Rig {
            shared,
            cell,
            changes,
            space,
            workspace,
            _scratch: scratch,
        }
    }

    fn armed(rig: &Rig) -> bool {
        rig.shared
            .roots()
            .spaces
            .iter()
            .any(|watched| watched.space == rig.space && watched.armed.is_some())
    }

    fn sent(changes: &mut UnboundedReceiver<FileChange>) -> Vec<String> {
        let mut paths = Vec::new();
        while let Ok(change) = changes.try_recv() {
            paths.push(change.path);
        }
        paths
    }

    fn requested(requests: &mpsc::Receiver<SpaceId>) -> Vec<SpaceId> {
        requests.try_iter().collect()
    }

    #[test]
    fn the_check_arms_a_workspace_whose_creation_was_lost() {
        let mut rig = rig();
        assert!(!arm(&rig.shared, &rig.cell, &rig.space).unwrap());
        std::fs::create_dir(&rig.workspace).unwrap();
        assert!(!armed(&rig));
        assert_eq!(rig.shared.unsettled(), [(rig.space.clone(), true)]);

        check(&rig.shared, &rig.cell);
        assert!(armed(&rig));
        assert_eq!(rig.shared.arms.load(Ordering::SeqCst), 1);
        assert_eq!(sent(&mut rig.changes), ["layout.json", "panels"]);
        assert!(rig.shared.unsettled().is_empty());

        check(&rig.shared, &rig.cell);
        assert_eq!(rig.shared.arms.load(Ordering::SeqCst), 1, "armed twice");
        assert!(sent(&mut rig.changes).is_empty());
    }

    #[test]
    fn the_check_marks_a_gone_workspace_as_not_armed() {
        let rig = rig();
        std::fs::create_dir(&rig.workspace).unwrap();
        assert!(arm(&rig.shared, &rig.cell, &rig.space).unwrap());
        std::fs::remove_dir(&rig.workspace).unwrap();
        assert_eq!(rig.shared.unsettled(), [(rig.space.clone(), false)]);
        check(&rig.shared, &rig.cell);
        assert!(!armed(&rig));
        assert!(rig.shared.unsettled().is_empty());
    }

    #[test]
    fn a_replaced_workspace_reported_only_as_created_is_armed_again() {
        let mut rig = rig();
        std::fs::create_dir(&rig.workspace).unwrap();
        assert!(arm(&rig.shared, &rig.cell, &rig.space).unwrap());
        assert!(sent(&mut rig.changes).is_empty());
        // Replace the folder. The new one exists beside the old first, so its inode differs.
        let next = rig.workspace.with_file_name(".workspace-next");
        std::fs::create_dir(&next).unwrap();
        std::fs::remove_dir(&rig.workspace).unwrap();
        std::fs::rename(&next, &rig.workspace).unwrap();
        assert!(armed(&rig), "nothing has told the watcher yet");
        assert_eq!(rig.shared.unsettled(), [(rig.space.clone(), true)]);

        // Under load FSEvents reported this as a creation alone, with no removal (#81).
        let (tx, requests) = mpsc::channel();
        let created =
            Event::new(EventKind::Create(CreateKind::Folder)).add_path(rig.workspace.clone());
        handle(&rig.shared, &tx, Ok(created));
        assert_eq!(requested(&requests), std::slice::from_ref(&rig.space));
        assert!(settle_and_report(&rig.shared, &rig.cell, &rig.space));
        assert_eq!(rig.shared.arms.load(Ordering::SeqCst), 2);
        assert_eq!(sent(&mut rig.changes), ["layout.json", "panels"]);
        assert!(rig.shared.unsettled().is_empty());
        assert_eq!(rig.shared.registered().len(), 1, "registered twice");
    }

    #[test]
    fn every_event_naming_the_workspace_asks_but_an_unchanged_one_is_left_alone() {
        let mut rig = rig();
        std::fs::create_dir(&rig.workspace).unwrap();
        let (tx, requests) = mpsc::channel();
        let event = |kind| Event::new(kind).add_path(rig.workspace.clone());
        handle(
            &rig.shared,
            &tx,
            Ok(event(EventKind::Create(CreateKind::Folder))),
        );
        assert_eq!(requested(&requests), std::slice::from_ref(&rig.space));
        assert!(settle_and_report(&rig.shared, &rig.cell, &rig.space));
        assert_eq!(sent(&mut rig.changes), ["layout.json", "panels"]);

        for kind in [
            EventKind::Create(CreateKind::Folder),
            EventKind::Modify(ModifyKind::Any),
            EventKind::Remove(RemoveKind::Folder),
        ] {
            handle(&rig.shared, &tx, Ok(event(kind)));
            assert_eq!(requested(&requests), std::slice::from_ref(&rig.space));
            assert!(!settle_and_report(&rig.shared, &rig.cell, &rig.space));
            assert!(armed(&rig), "{kind:?}: the folder is still there");
        }
        assert_eq!(rig.shared.arms.load(Ordering::SeqCst), 1);
        assert!(sent(&mut rig.changes).is_empty());

        let inside = Event::new(EventKind::Create(CreateKind::File))
            .add_path(rig.workspace.join("layout.json"));
        handle(&rig.shared, &tx, Ok(inside));
        assert!(requested(&requests).is_empty());
    }

    #[test]
    fn a_rescan_or_an_error_asks_about_every_space() {
        let mut rig = rig();
        let (tx, requests) = mpsc::channel();
        let rescan = Event::new(EventKind::Other).set_flag(Flag::Rescan);
        handle(&rig.shared, &tx, Ok(rescan));
        assert_eq!(requested(&requests), std::slice::from_ref(&rig.space));
        assert_eq!(sent(&mut rig.changes), ["layout.json", "panels"]);
        handle(&rig.shared, &tx, Err(notify::Error::generic("lost")));
        assert_eq!(requested(&requests), std::slice::from_ref(&rig.space));
        assert_eq!(sent(&mut rig.changes), ["layout.json", "panels"]);

        // The rescan found the folder that no event reported.
        std::fs::create_dir(&rig.workspace).unwrap();
        settle_and_report(&rig.shared, &rig.cell, &rig.space);
        assert!(armed(&rig));
    }

    /// A stand-in for [`folder_id`] that, while closed, blocks every lookup until opened, and
    /// can be told what to answer.
    #[derive(Clone, Default)]
    struct Gate(Arc<(Mutex<GateState>, std::sync::Condvar)>);

    #[derive(Default)]
    struct GateState {
        closed: bool,
        /// Lookups that found the gate closed.
        entered: usize,
        /// The answer to give instead of [`folder_id`]'s.
        answer: Option<Option<FolderId>>,
    }

    impl Gate {
        fn state(&self) -> MutexGuard<'_, GateState> {
            self.0 .0.lock().unwrap()
        }

        fn identify(&self) -> Identify {
            let gate = self.clone();
            Box::new(move |path| {
                let (lock, wake) = &*gate.0;
                let mut state = lock.lock().unwrap();
                if state.closed {
                    state.entered += 1;
                    wake.notify_all();
                    while state.closed {
                        state = wake.wait(state).unwrap();
                    }
                }
                match state.answer {
                    Some(answer) => answer,
                    None => {
                        drop(state);
                        folder_id(path)
                    }
                }
            })
        }

        fn close(&self) {
            self.state().closed = true;
        }

        fn answer(&self, answer: Option<FolderId>) {
            self.state().answer = Some(answer);
        }

        fn open(&self) {
            self.state().closed = false;
            self.0 .1.notify_all();
        }

        /// Waits until a lookup is blocked on the closed gate.
        fn wait_entered(&self) {
            let (lock, wake) = &*self.0;
            let (state, timeout) = wake
                .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(10), |state| {
                    state.entered == 0
                })
                .unwrap();
            assert!(
                !timeout.timed_out() && state.entered > 0,
                "no lookup started"
            );
        }
    }

    /// Opens the gate when dropped, so a failing test never leaves a lookup blocked.
    struct OpenOnDrop<'a>(&'a Gate);

    impl Drop for OpenOnDrop<'_> {
        fn drop(&mut self) {
            self.0.open();
        }
    }

    /// The roots' lock, or a failure if it stays held: a lookup must never run under it.
    fn roots_within(shared: &Shared) -> MutexGuard<'_, WatchRoots> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(roots) = shared.roots.try_lock() {
                return roots;
            }
            assert!(
                Instant::now() < deadline,
                "the roots were held during a lookup"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A folder identity no real folder has.
    const ELSEWHERE: FolderId = (u64::MAX, u64::MAX);

    fn set_armed(rig: &Rig, armed: Option<FolderId>) {
        let mut roots = roots_within(&rig.shared);
        let watched = roots
            .spaces
            .iter_mut()
            .find(|watched| watched.space == rig.space)
            .unwrap();
        watched.armed = armed;
    }

    fn armed_as(rig: &Rig) -> Option<FolderId> {
        rig.shared
            .armed_state(&rig.space)
            .and_then(|(_, armed)| armed)
    }

    #[test]
    fn a_blocked_folder_lookup_never_holds_up_the_handler() {
        let gate = Gate::default();
        let mut rig = rig_with(gate.identify());
        std::fs::create_dir(&rig.workspace).unwrap();
        assert!(arm(&rig.shared, &rig.cell, &rig.space).unwrap());
        let (tx, requests) = mpsc::channel();
        gate.close();
        std::thread::scope(|scope| {
            let _open = OpenOnDrop(&gate);
            let settling = scope.spawn(|| settle(&rig.shared, &rig.cell, &rig.space));
            gate.wait_entered();
            // The lookup is stuck; events must still be handled.
            let (done, handled) = mpsc::channel();
            let (shared, tx) = (&rig.shared, &tx);
            let event = Event::new(EventKind::Modify(ModifyKind::Any))
                .add_path(rig.workspace.join(LAYOUT_FILE));
            scope.spawn(move || {
                handle(shared, tx, Ok(event));
                done.send(()).unwrap();
            });
            let progressed = handled.recv_timeout(Duration::from_secs(10)).is_ok();
            gate.open();
            assert!(progressed, "the handler waited on a folder lookup");
            assert!(!settling.join().unwrap().unwrap());
        });
        assert_eq!(sent(&mut rig.changes), ["layout.json"]);
        assert!(requested(&requests).is_empty());
        assert!(armed(&rig));
    }

    #[test]
    fn a_space_rearmed_during_the_lookup_is_read_again() {
        let gate = Gate::default();
        let mut rig = rig_with(gate.identify());
        std::fs::create_dir(&rig.workspace).unwrap();
        assert!(arm(&rig.shared, &rig.cell, &rig.space).unwrap());
        // The lookup will report another folder, and while it runs the space is armed on that
        // folder. The stale answer must not disarm and arm it again.
        gate.answer(Some(ELSEWHERE));
        gate.close();
        std::thread::scope(|scope| {
            let _open = OpenOnDrop(&gate);
            let settling = scope.spawn(|| settle(&rig.shared, &rig.cell, &rig.space));
            gate.wait_entered();
            set_armed(&rig, Some(ELSEWHERE));
            gate.open();
            assert!(
                !settling.join().unwrap().unwrap(),
                "armed again on a stale lookup"
            );
        });
        assert_eq!(armed_as(&rig), Some(ELSEWHERE));
        assert_eq!(rig.shared.arms.load(Ordering::SeqCst), 1);
        assert!(sent(&mut rig.changes).is_empty());
        assert_eq!(rig.shared.registered().len(), 1);
    }

    #[test]
    fn a_space_disarmed_or_dropped_during_the_lookup_is_left_as_it_became() {
        let gate = Gate::default();
        let mut rig = rig_with(gate.identify());
        std::fs::create_dir(&rig.workspace).unwrap();
        assert!(arm(&rig.shared, &rig.cell, &rig.space).unwrap());
        let armed = armed_as(&rig);

        // Disarmed meanwhile, and the folder now gone: read again, nothing to arm.
        gate.answer(Some(ELSEWHERE));
        gate.close();
        std::thread::scope(|scope| {
            let _open = OpenOnDrop(&gate);
            let settling = scope.spawn(|| settle(&rig.shared, &rig.cell, &rig.space));
            gate.wait_entered();
            set_armed(&rig, None);
            gate.answer(None);
            gate.open();
            assert!(!settling.join().unwrap().unwrap());
        });
        assert_eq!(armed_as(&rig), None);
        assert_eq!(rig.shared.arms.load(Ordering::SeqCst), 1);

        // No longer watched meanwhile: the lookup's answer is dropped.
        set_armed(&rig, armed);
        gate.state().entered = 0;
        gate.answer(Some(ELSEWHERE));
        gate.close();
        std::thread::scope(|scope| {
            let _open = OpenOnDrop(&gate);
            let settling = scope.spawn(|| settle(&rig.shared, &rig.cell, &rig.space));
            gate.wait_entered();
            roots_within(&rig.shared).remove(&rig.space);
            gate.open();
            assert!(!settling.join().unwrap().unwrap());
        });
        assert!(rig.shared.armed_state(&rig.space).is_none());
        assert_eq!(rig.shared.arms.load(Ordering::SeqCst), 1);
        assert!(sent(&mut rig.changes).is_empty());
    }

    #[test]
    fn the_identity_recorded_is_the_folder_notify_watched() {
        // The folder is absent when first looked up, appears (A) before the watch is set up,
        // and is replaced (by B) before it is looked up again: notify watches A. Recording B
        // would leave A's watch in place for good, since B is what's on disk.
        let lookups = Arc::new(Mutex::new(0));
        let identify: Identify = {
            let lookups = Arc::clone(&lookups);
            Box::new(move |path| {
                let mut lookups = lookups.lock().unwrap();
                *lookups += 1;
                match *lookups {
                    1 => {
                        std::fs::create_dir(path).unwrap();
                        None
                    }
                    2 => {
                        let next = path.with_file_name(".workspace-next");
                        std::fs::create_dir(&next).unwrap();
                        std::fs::remove_dir(path).unwrap();
                        std::fs::rename(&next, path).unwrap();
                        folder_id(path)
                    }
                    _ => folder_id(path),
                }
            })
        };
        let watched = Arc::new(Mutex::new(Vec::new()));
        let mut rig = rig_with(identify);
        rig.shared.on_register = Some({
            let watched = Arc::clone(&watched);
            Box::new(move |path| watched.lock().unwrap().push(folder_id(path).unwrap()))
        });

        let armed = arm(&rig.shared, &rig.cell, &rig.space).unwrap();
        let watched = watched.lock().unwrap().clone();
        let a = watched[0];
        let b = folder_id(&rig.workspace).unwrap();
        assert_ne!(a, b, "the folder was not replaced");
        // The watch was set up again on B, and B is what is recorded.
        assert!(armed);
        assert_eq!(watched, [a, b]);
        assert_eq!(armed_as(&rig), Some(b));
        assert_eq!(rig.shared.registered().len(), 1, "registered twice");
        assert_eq!(sent(&mut rig.changes), Vec::<String>::new());
    }
}
