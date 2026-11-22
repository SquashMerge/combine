#![cfg(feature = "test-support")]
//! The layout watcher: what `classify` keeps, and real filesystem events.

mod daemon_support;

use std::fs;
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

const BOUND: Duration = Duration::from_secs(10);
const SLICE: Duration = Duration::from_millis(200);

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

fn start(space: &SpaceId) -> (SpaceWatcher, UnboundedReceiver<FileChange>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let watcher = SpaceWatcher::new(tx).unwrap();
    watcher.watch(space).unwrap();
    (watcher, rx)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

/// Runs `cause` until a change for `space` arrives, for at most [`BOUND`].
fn change_after(
    rx: &mut UnboundedReceiver<FileChange>,
    space: &SpaceId,
    mut cause: impl FnMut(),
) -> bool {
    let rt = runtime();
    let deadline = Instant::now() + BOUND;
    while Instant::now() < deadline {
        cause();
        let next = rt.block_on(async { tokio::time::timeout(SLICE, rx.recv()).await });
        if let Ok(Some(change)) = next {
            if change.space == *space && change.path == "layout.json" {
                return true;
            }
        }
    }
    false
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
    let (_watcher, mut rx) = start(&space);
    let workspace = Path::new(&space.0).join(".workspace");
    fs::create_dir(&workspace).unwrap();
    assert!(
        change_after(&mut rx, &space, || {}),
        "creating .workspace reported no layout change"
    );
    let mut round = 0;
    let seen = change_after(&mut rx, &space, || {
        round += 1;
        fs::write(workspace.join("layout.json"), format!("{round}")).unwrap();
    });
    assert!(seen, "no change from the re-armed watch");

    fs::remove_dir_all(&workspace).unwrap();
    while rx.try_recv().is_ok() {}
    fs::create_dir(&workspace).unwrap();
    assert!(
        change_after(&mut rx, &space, || {}),
        "re-creating .workspace reported no layout change"
    );
}

#[test]
fn failed_watch_is_not_registered() {
    let scratch = ShortTempDir::new();
    let folder = scratch.path().join("later");
    let space = SpaceId(folder.to_str().unwrap().to_string());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let watcher = SpaceWatcher::new(tx).unwrap();
    assert!(watcher.watch(&space).is_err());
    fs::create_dir_all(folder.join(".workspace")).unwrap();
    watcher.watch(&space).unwrap();
    let mut round = 0;
    let seen = change_after(&mut rx, &space, || {
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
        [layout_change(&a), layout_change(&b)]
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
    let seen = change_after(&mut rx, &space, || {
        round += 1;
        fs::write(workspace.join("layout.json"), format!("edit {round}")).unwrap();
    });
    assert!(seen, "the edit was never reported");
    let seen = change_after(&mut rx, &space, || {
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
    let seen = change_after(&mut rx, &space, || {
        round += 1;
        let temp = workspace.join(format!(".layout.json.tmp-{round}"));
        fs::write(&temp, format!("renamed {round}")).unwrap();
        fs::rename(&temp, workspace.join("layout.json")).unwrap();
    });
    assert!(seen, "the rename was never reported");
}
