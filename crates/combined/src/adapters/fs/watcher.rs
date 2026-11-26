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
                let found = folder_id(&workspace);
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
fn arm(shared: &Shared, cell: &WatcherCell, space: &SpaceId) -> notify::Result<bool> {
    let mut guard = cell.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(watcher) = guard.as_mut() else {
        return Ok(false);
    };
    let workspace = {
        let roots = shared.roots();
        match roots.spaces.iter().find(|watched| &watched.space == space) {
            Some(watched) if watched.armed.is_none() => watched.workspace(),
            _ => return Ok(false),
        }
    };
    // Read before the watch is set up: a folder replaced in between then differs from the one
    // remembered, and the next settle arms it again.
    let found = folder_id(&workspace);
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
    let Some(found) = found.or_else(|| folder_id(&workspace)) else {
        return Ok(false);
    };
    watched.armed = Some(found);
    watched.arm_error = None;
    drop(roots);
    shared.arms.fetch_add(1, Ordering::SeqCst);
    Ok(true)
}

/// Brings `space`'s watch in line with its `.workspace/` on disk: one that is watched but gone
/// is marked as not watched, one that was replaced since it was armed is armed again, and
/// [`arm`] watches one that exists and isn't watched. True when this call began watching it.
fn settle(shared: &Shared, cell: &WatcherCell, space: &SpaceId) -> notify::Result<bool> {
    {
        let mut roots = shared.roots();
        let Some(watched) = roots
            .spaces
            .iter_mut()
            .find(|watched| &watched.space == space)
        else {
            return Ok(false);
        };
        if let Some(armed) = watched.armed {
            match folder_id(&watched.workspace()) {
                Some(found) if found == armed => return Ok(false),
                Some(_) => watched.armed = None,
                None => {
                    watched.armed = None;
                    return Ok(false);
                }
            }
        }
    }
    arm(shared, cell, space)
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
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    /// A folder under the temp dir, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Scratch {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "combined-watcher-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Scratch(path)
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
        let scratch = Scratch::new();
        let space = SpaceId(scratch.0.to_str().unwrap().to_string());
        let (tx, changes) = unbounded_channel();
        let shared = Shared {
            roots: Mutex::new(WatchRoots::new()),
            changes: tx,
            arms: AtomicU64::new(0),
            registered: Mutex::new(Vec::new()),
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
}
