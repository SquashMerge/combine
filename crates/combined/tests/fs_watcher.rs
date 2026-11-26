#![cfg(feature = "test-support")]
//! The layout watcher: what `classify` keeps, and real filesystem events.

mod daemon_support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use combined::adapters::fs::watcher::{classify, SpaceWatcher, WatchRoots};
use combined::model::SpaceId;
use combined::ports::FileChange;
use daemon_support::ShortTempDir;
use notify::event::Flag;
use notify::event::{
    AccessKind, AccessMode, CreateKind, DataChange, MetadataKind, ModifyKind, RemoveKind,
    RenameMode,
};
use notify::{Event, EventKind};
use tokio::sync::mpsc::{self, UnboundedReceiver};

/// How long a test waits for the watcher to report a change. It only matters when one is
/// missing, and it is long because FSEvents holds deliveries back under load: 10-15 s was
/// measured on a loaded machine, where the old 10 s bound failed (#81).
const BOUND: Duration = Duration::from_secs(30);
const SLICE: Duration = Duration::from_millis(200);
/// The real watchers' interval for comparing their watches with the disk: longer than any
/// test, so a change they report came from an event, never from that check.
const NO_CHECK: Duration = Duration::from_secs(3600);

fn roots(space: &SpaceId) -> WatchRoots {
    let mut roots = WatchRoots::new();
    assert!(roots.add(space));
    assert!(!roots.add(space));
    roots
}

fn event(kind: EventKind, paths: &[&Path]) -> Event {
    let mut event = Event::new(kind);
    for path in paths {
        event = event.add_path(path.to_path_buf());
    }
    event
}

fn layout_change(space: &SpaceId) -> FileChange {
    FileChange {
        space: space.clone(),
        path: "layout.json".to_string(),
    }
}

fn panels_change(space: &SpaceId, path: &str) -> FileChange {
    FileChange {
        space: space.clone(),
        path: path.to_string(),
    }
}

#[test]
fn classify_ignores_temp_files() {
    let space = SpaceId("/work/project".to_string());
    let roots = roots(&space);
    for name in [
        ".layout.json.tmp-123-0",
        ".layout.json",
        ".layout.json.tmp-9-1",
    ] {
        let path = PathBuf::from("/work/project/.workspace").join(name);
        for kind in [
            EventKind::Create(CreateKind::File),
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            EventKind::Remove(RemoveKind::File),
        ] {
            assert_eq!(classify(&event(kind, &[&path]), &roots), [], "{name}");
        }
    }
}

#[test]
fn classify_reports_rename_onto_layout() {
    let space = SpaceId("/work/project".to_string());
    let roots = roots(&space);
    let temp = Path::new("/work/project/.workspace/.layout.json.tmp-1-0");
    let layout = Path::new("/work/project/.workspace/layout.json");
    let renamed = event(
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        &[temp, layout],
    );
    assert_eq!(classify(&renamed, &roots), [layout_change(&space)]);
    let to = event(
        EventKind::Modify(ModifyKind::Name(RenameMode::To)),
        &[layout],
    );
    assert_eq!(classify(&to, &roots), [layout_change(&space)]);
}

#[test]
fn classify_reports_remove() {
    let space = SpaceId("/work/project".to_string());
    let roots = roots(&space);
    let layout = Path::new("/work/project/.workspace/layout.json");
    for kind in [
        EventKind::Remove(RemoveKind::File),
        EventKind::Create(CreateKind::File),
        EventKind::Modify(ModifyKind::Data(DataChange::Content)),
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)),
        EventKind::Access(AccessKind::Close(AccessMode::Write)),
        EventKind::Any,
        EventKind::Other,
    ] {
        assert_eq!(
            classify(&event(kind, &[layout]), &roots),
            [layout_change(&space)],
            "{kind:?}"
        );
    }
}

#[test]
fn classify_ignores_other_files() {
    let space = SpaceId("/work/project".to_string());
    let roots = roots(&space);
    let created = EventKind::Create(CreateKind::File);
    for path in [
        "/work/project/.workspace/other.json",
        "/work/project/.workspace/layout.json.bak",
        "/work/project/layout.json",
        "/work/project/.workspace/sub/layout.json",
        "/work/other/.workspace/layout.json",
        "/work/project/.workspace",
        "/work/project",
    ] {
        assert_eq!(
            classify(&event(created, &[Path::new(path)]), &roots),
            [],
            "{path}"
        );
    }
    let read = Path::new("/work/project/.workspace/layout.json");
    for kind in [
        EventKind::Access(AccessKind::Open(AccessMode::Any)),
        EventKind::Access(AccessKind::Close(AccessMode::Read)),
        EventKind::Access(AccessKind::Read),
    ] {
        assert_eq!(classify(&event(kind, &[read]), &roots), [], "{kind:?}");
    }
    assert_eq!(classify(&event(created, &[]), &roots), []);
}

#[test]
fn classify_reports_panel_files() {
    let space = SpaceId("/work/project".to_string());
    let roots = roots(&space);
    for file in ["panel.json", "diagram.mmd", "content.md"] {
        let path = PathBuf::from("/work/project/.workspace/panels/p1").join(file);
        for kind in [
            EventKind::Create(CreateKind::File),
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            EventKind::Remove(RemoveKind::File),
        ] {
            assert_eq!(
                classify(&event(kind, &[&path]), &roots),
                [panels_change(&space, &format!("panels/p1/{file}"))],
                "{file} {kind:?}"
            );
        }
    }
    let temp = Path::new("/work/project/.workspace/panels/p1/.panel.json.tmp-1-0");
    let target = Path::new("/work/project/.workspace/panels/p1/panel.json");
    let renamed = event(
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        &[temp, target],
    );
    assert_eq!(
        classify(&renamed, &roots),
        [panels_change(&space, "panels/p1/panel.json")]
    );
}

#[test]
fn classify_ignores_panel_temp_and_other_files() {
    let space = SpaceId("/work/project".to_string());
    let roots = roots(&space);
    let created = EventKind::Create(CreateKind::File);
    for path in [
        "/work/project/.workspace/panels/p1/.panel.json.tmp-1-0",
        "/work/project/.workspace/panels/p1/.DS_Store",
        "/work/project/.workspace/panels/p1/scratch.json",
        "/work/project/.workspace/panels/p1/other.txt",
        "/work/project/.workspace/panels/p1/sub/panel.json",
        "/work/project/.workspace/panels/.p1/panel.json",
        "/work/project/.workspace/panels/.DS_Store",
        "/work/project/.workspace/other/p1/panel.json",
        "/work/other/.workspace/panels/p1/panel.json",
    ] {
        assert_eq!(
            classify(&event(created, &[Path::new(path)]), &roots),
            [],
            "{path}"
        );
    }
    let read = Path::new("/work/project/.workspace/panels/p1/panel.json");
    for kind in [
        EventKind::Access(AccessKind::Open(AccessMode::Any)),
        EventKind::Access(AccessKind::Close(AccessMode::Read)),
        EventKind::Access(AccessKind::Read),
    ] {
        assert_eq!(classify(&event(kind, &[read]), &roots), [], "{kind:?}");
    }
}

#[test]
fn classify_reports_panel_folder_events_as_panels() {
    let space = SpaceId("/work/project".to_string());
    let roots = roots(&space);
    let panels = Path::new("/work/project/.workspace/panels");
    let folder = Path::new("/work/project/.workspace/panels/p1");
    let away = Path::new("/work/project/.workspace/panels/.p1.old");
    let hidden = Path::new("/work/project/.workspace/panels/.p2");
    let one = [panels_change(&space, "panels")];
    for kind in [
        EventKind::Create(CreateKind::Folder),
        EventKind::Remove(RemoveKind::Folder),
    ] {
        assert_eq!(classify(&event(kind, &[panels]), &roots), one, "{kind:?}");
        assert_eq!(classify(&event(kind, &[folder]), &roots), one, "{kind:?}");
    }
    let renamed = event(
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        &[folder, away],
    );
    assert_eq!(classify(&renamed, &roots), one);
    let replaced = event(
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        &[away, folder],
    );
    assert_eq!(classify(&replaced, &roots), one);
    assert_eq!(
        classify(
            &event(EventKind::Create(CreateKind::Folder), &[hidden]),
            &roots
        ),
        []
    );
}

fn start(space: &SpaceId) -> (SpaceWatcher, UnboundedReceiver<FileChange>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let watcher = SpaceWatcher::with_arm_retry(tx, NO_CHECK).unwrap();
    watcher.watch(space).unwrap();
    (watcher, rx)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

/// Runs `cause` until the change `path` for `space` arrives, for at most [`BOUND`].
fn change_after(
    rx: &mut UnboundedReceiver<FileChange>,
    space: &SpaceId,
    path: &str,
    mut cause: impl FnMut(),
) -> bool {
    let rt = runtime();
    let deadline = Instant::now() + BOUND;
    while Instant::now() < deadline {
        cause();
        let next = rt.block_on(async { tokio::time::timeout(SLICE, rx.recv()).await });
        if let Ok(Some(change)) = next {
            if change.space == *space && change.path == path {
                return true;
            }
        }
    }
    false
}

/// Changes `workspace`'s mode, an event that names the folder itself, as its creation does.
/// Called each slice while a test waits for a re-arm, it keeps the test from resting on one
/// creation event, which FSEvents can hold back, or report without the removal of the folder it
/// replaced, under load (#81). The watcher settles the space on any event naming its
/// `.workspace/`, and [`start`]'s watcher never runs its periodic check, so a broken re-arm
/// still fails.
fn nudge(workspace: &Path, round: &mut u32) {
    *round += 1;
    let mode = if round.is_multiple_of(2) {
        0o755
    } else {
        0o700
    };
    let _ = fs::set_permissions(workspace, fs::Permissions::from_mode(mode));
}

fn scratch_space() -> (ShortTempDir, SpaceId) {
    let scratch = ShortTempDir::new();
    let folder = scratch.path().join("project");
    fs::create_dir(&folder).unwrap();
    let space = SpaceId(folder.to_str().unwrap().to_string());
    (scratch, space)
}

#[test]
fn workspace_creation_rearms_watch() {
    let (_scratch, space) = scratch_space();
    let (watcher, mut rx) = start(&space);
    let workspace = Path::new(&space.0).join(".workspace");
    assert!(!watcher.is_armed(&space));
    fs::create_dir(&workspace).unwrap();
    let mut nudges = 0;
    assert!(
        change_after(&mut rx, &space, "layout.json", || nudge(
            &workspace,
            &mut nudges
        )),
        "creating .workspace reported no layout change"
    );
    assert!(watcher.is_armed(&space));
    std::thread::sleep(SLICE);
    while rx.try_recv().is_ok() {}
    let armed = watcher.arms();
    let mut round = 0;
    let seen = change_after(&mut rx, &space, "layout.json", || {
        round += 1;
        fs::write(workspace.join("layout.json"), format!("{round}")).unwrap();
    });
    assert!(seen, "no change from the re-armed watch");
    assert_eq!(watcher.arms(), armed, "the change was a new re-arm's");

    fs::remove_dir_all(&workspace).unwrap();
    while rx.try_recv().is_ok() {}
    fs::create_dir(&workspace).unwrap();
    assert!(
        change_after(&mut rx, &space, "layout.json", || nudge(
            &workspace,
            &mut nudges
        )),
        "re-creating .workspace reported no layout change"
    );
}

#[test]
fn rearm_never_registers_a_path_twice() {
    let (_scratch, space) = scratch_space();
    let (watcher, mut rx) = start(&space);
    let workspace = Path::new(&space.0).join(".workspace");
    for cycle in 1..=3 {
        fs::create_dir(&workspace).unwrap();
        let mut nudges = 0;
        assert!(
            change_after(&mut rx, &space, "layout.json", || nudge(
                &workspace,
                &mut nudges
            )),
            "cycle {cycle}: creating .workspace reported no layout change"
        );
        let paths = watcher.watched_paths();
        assert_eq!(
            paths.iter().filter(|path| **path == workspace).count(),
            1,
            "cycle {cycle}: {paths:?}"
        );
        let mut round = 0;
        let seen = change_after(&mut rx, &space, "layout.json", || {
            round += 1;
            fs::write(workspace.join("layout.json"), format!("{cycle} {round}")).unwrap();
        });
        assert!(seen, "cycle {cycle}: a write was not reported");
        fs::remove_dir_all(&workspace).unwrap();
        while rx.try_recv().is_ok() {}
    }
}

#[test]
fn failed_watch_is_not_registered() {
    let scratch = ShortTempDir::new();
    let folder = scratch.path().join("later");
    let space = SpaceId(folder.to_str().unwrap().to_string());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let watcher = SpaceWatcher::with_arm_retry(tx, NO_CHECK).unwrap();
    assert!(watcher.watch(&space).is_err());
    fs::create_dir_all(folder.join(".workspace")).unwrap();
    watcher.watch(&space).unwrap();
    let mut round = 0;
    let seen = change_after(&mut rx, &space, "layout.json", || {
        round += 1;
        fs::write(folder.join(".workspace/layout.json"), format!("{round}")).unwrap();
    });
    assert!(seen, "the space was not watched after the second attempt");
}

#[test]
fn classify_maps_a_rescan_to_every_space() {
    let (a, b) = (
        SpaceId("/work/a".to_string()),
        SpaceId("/work/b".to_string()),
    );
    let mut roots = roots(&a);
    assert!(roots.add(&b));
    let rescan = Event::new(EventKind::Other).set_flag(Flag::Rescan);
    assert_eq!(
        classify(&rescan, &roots),
        [
            layout_change(&a),
            panels_change(&a, "panels"),
            layout_change(&b),
            panels_change(&b, "panels")
        ]
    );
    assert_eq!(classify(&Event::new(EventKind::Other), &roots), []);
}

#[test]
fn real_watcher_reports_outside_edit() {
    let (_scratch, space) = scratch_space();
    let workspace = Path::new(&space.0).join(".workspace");
    fs::create_dir(&workspace).unwrap();
    let (_watcher, mut rx) = start(&space);
    let mut round = 0;
    let seen = change_after(&mut rx, &space, "layout.json", || {
        round += 1;
        fs::write(workspace.join("layout.json"), format!("edit {round}")).unwrap();
    });
    assert!(seen, "the edit was never reported");
    let seen = change_after(&mut rx, &space, "layout.json", || {
        let _ = fs::remove_file(workspace.join("layout.json"));
    });
    assert!(seen, "the removal was never reported");
}

#[test]
fn real_watcher_reports_rename_write() {
    let (_scratch, space) = scratch_space();
    let workspace = Path::new(&space.0).join(".workspace");
    fs::create_dir(&workspace).unwrap();
    let (_watcher, mut rx) = start(&space);
    let mut round = 0;
    let seen = change_after(&mut rx, &space, "layout.json", || {
        round += 1;
        let temp = workspace.join(format!(".layout.json.tmp-{round}"));
        fs::write(&temp, format!("renamed {round}")).unwrap();
        fs::rename(&temp, workspace.join("layout.json")).unwrap();
    });
    assert!(seen, "the rename was never reported");
}

#[test]
fn real_watcher_reports_panel_file_edits() {
    let (_scratch, space) = scratch_space();
    let folder = Path::new(&space.0).join(".workspace/panels/p1");
    fs::create_dir_all(&folder).unwrap();
    let (_watcher, mut rx) = start(&space);
    let mut round = 0;
    let seen = change_after(&mut rx, &space, "panels/p1/panel.json", || {
        round += 1;
        fs::write(folder.join("panel.json"), format!("edit {round}")).unwrap();
    });
    assert!(seen, "the panel.json edit was never reported");
    let seen = change_after(&mut rx, &space, "panels/p1/diagram.mmd", || {
        round += 1;
        let temp = folder.join(format!(".diagram.mmd.tmp-{round}"));
        fs::write(&temp, format!("graph {round}")).unwrap();
        fs::rename(&temp, folder.join("diagram.mmd")).unwrap();
    });
    assert!(seen, "the diagram.mmd rename was never reported");
    let seen = change_after(&mut rx, &space, "panels", || {
        round += 1;
        fs::create_dir_all(Path::new(&space.0).join(format!(".workspace/panels/q{round}")))
            .unwrap();
    });
    assert!(seen, "a new panel folder was never reported");
}
