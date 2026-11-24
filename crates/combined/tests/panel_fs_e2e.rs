#![cfg(feature = "test-support")]
//! The panels over real folders: `FsFileStore` and its watcher, `FsScratchStore`, no client and
//! no layout watch. FSEvents can be slow, so a change that the watcher must report gets the
//! bound `fs_watcher.rs` uses and is repeated until it shows.

mod common;
mod daemon_support;
mod panel_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use combine_protocol::PanelState;
use combined::adapters::fs::file_store::FsFileStore;
use combined::adapters::fs::scratch_store::FsScratchStore;
use combined::core::{PanelApiError, PanelUpdate, PanelView};
use combined::model::{Lifetime, SpaceId};
use combined::ports::{FileStore, ScratchStore};
use combined::rpc::{PanelHandle, ServerHandle};
use combined::testing::{FakeBackend, FakeConfig, FakeDiagramCheck, FakeWallClock};
use daemon_support::{wait_until, work_dir, DaemonPaths, ShortTempDir};
use panel_support::{launch, markdown, named, scratch, START_WALL_MS, TEST_TIMING};

const BOUND: Duration = Duration::from_secs(10);

struct FsRig {
    server: ServerHandle,
    panels: PanelHandle,
    space: SpaceId,
    work: PathBuf,
    data: PathBuf,
    _dir: ShortTempDir,
}

impl FsRig {
    fn new() -> FsRig {
        let dir = ShortTempDir::new();
        let work = work_dir(&dir);
        fs::create_dir_all(work.join(".git")).unwrap();
        let data = DaemonPaths::in_scratch(&dir, "p").data_dir;
        let (backend, _control) = FakeBackend::new(FakeConfig::default());
        let served: Arc<dyn FileStore> = Arc::new(FsFileStore::new());
        let scratch: Arc<dyn ScratchStore> = Arc::new(FsScratchStore::new(data.join("scratch")));
        let (server, _socket) = launch(
            &dir,
            1,
            &backend,
            &served,
            scratch,
            &Arc::new(FakeDiagramCheck::new()),
            &Arc::new(FakeWallClock::new(START_WALL_MS)),
            TEST_TIMING,
        );
        FsRig {
            panels: server.panels(),
            server,
            space: SpaceId(work.to_str().unwrap().to_string()),
            work,
            data,
            _dir: dir,
        }
    }

    fn panel_dir(&self, id: &str) -> PathBuf {
        self.work.join(".workspace/panels").join(id)
    }

    fn get(&self, id: &str) -> Result<(PanelView, String), PanelApiError> {
        self.panels.get(&self.space, id)
    }

    fn wait_for(
        &self,
        id: &str,
        what: &str,
        ready: impl Fn(&PanelView, &str) -> bool,
    ) -> (PanelView, String) {
        let mut last = None;
        let held = wait_until(BOUND, || {
            last = Some(self.get(id));
            matches!(&last, Some(Ok((view, source))) if ready(view, source))
        });
        assert!(held, "{id} never {what}; it is {last:?}");
        last.unwrap().unwrap()
    }

    fn validated(&self, id: &str) -> (PanelView, String) {
        self.wait_for(id, "validated", |view, _| {
            view.state == PanelState::Validated
        })
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn real_watcher_reports_an_outside_panel_edit_with_no_client() {
    let rig = FsRig::new();
    rig.panels
        .create(&rig.space, named("p", markdown("one")))
        .unwrap();
    let (before, _) = rig.validated("p");

    let content = rig.panel_dir("p").join("content.md");
    let started = Instant::now();
    let mut n = 0;
    let (after, source) = loop {
        n += 1;
        fs::write(&content, format!("outside {n}")).unwrap();
        std::thread::sleep(Duration::from_millis(250));
        if let Ok((view, source)) = rig.get("p") {
            if source.starts_with("outside") && view.state == PanelState::Validated {
                break (view, source);
            }
        }
        assert!(
            started.elapsed() < BOUND,
            "the watcher never reported the edit"
        );
    };
    assert!(after.revision > before.revision);
    assert!(source.starts_with("outside"));
    assert_eq!(rig.server.stats().connections(), 0);
}

#[test]
fn daemon_writes_are_not_echoed_on_disk() {
    let rig = FsRig::new();
    rig.panels
        .create(&rig.space, named("p", markdown("one")))
        .unwrap();
    rig.validated("p");
    let updated = rig
        .panels
        .update(
            &rig.space,
            "p",
            PanelUpdate {
                content: Some("two".to_string()),
                title: Some(Some("Title".to_string())),
            },
        )
        .unwrap();
    rig.validated("p");

    std::thread::sleep(Duration::from_millis(1_500));
    let (view, source) = rig.get("p").unwrap();
    assert_eq!((view.revision, source.as_str()), (updated.revision, "two"));
    assert_eq!(read(&rig.panel_dir("p").join("content.md")), "two");
    assert!(read(&rig.panel_dir("p").join("panel.json")).contains("\"title\": \"Title\""));
    assert_eq!(rig.server.stats().connections(), 0);
}

#[test]
fn a_panel_folder_replaced_by_rename_is_reread() {
    let rig = FsRig::new();
    rig.panels
        .create(&rig.space, named("p", markdown("one")))
        .unwrap();
    let (before, _) = rig.validated("p");

    let panels = rig.work.join(".workspace/panels");
    let fresh = panels.join(".new");
    fs::create_dir(&fresh).unwrap();
    fs::write(
        fresh.join("panel.json"),
        r#"{"version":1,"renderer":"markdown"}"#,
    )
    .unwrap();
    fs::write(fresh.join("content.md"), "replaced").unwrap();
    fs::rename(panels.join("p"), panels.join(".old")).unwrap();
    fs::rename(&fresh, panels.join("p")).unwrap();

    let (after, source) = rig.wait_for("p", "was reread", |view, source| {
        source == "replaced" && view.state == PanelState::Validated
    });
    assert!(after.revision > before.revision);
    assert_eq!(source, "replaced");
    assert_eq!(rig.server.stats().connections(), 0);
}

#[test]
fn scratch_panel_lives_under_data_scratch_sha256() {
    let rig = FsRig::new();
    rig.panels
        .create(&rig.space, named("s", scratch(markdown("one"))))
        .unwrap();
    rig.validated("s");

    let folder = rig
        .data
        .join("scratch")
        .join(rig.space.sha256_hex())
        .join("s");
    assert_eq!(read(&folder.join("content.md")), "one");
    assert!(read(&folder.join("panel.json")).contains("\"renderer\": \"markdown\""));
    assert!(read(&folder.join("scratch.json")).contains(&rig.space.0));
    assert!(!rig.panel_dir("s").exists());
    assert!(!rig.work.join(".workspace").exists());
    assert_eq!(rig.server.stats().connections(), 0);
}

#[test]
fn pin_moves_files_into_dot_workspace_on_disk() {
    let rig = FsRig::new();
    rig.panels
        .create(&rig.space, named("s", scratch(markdown("one"))))
        .unwrap();
    rig.validated("s");
    let folder = rig
        .data
        .join("scratch")
        .join(rig.space.sha256_hex())
        .join("s");
    assert!(folder.exists());

    rig.panels.pin(&rig.space, "s").unwrap();
    let (view, source) = rig.wait_for("s", "became persistent", |view, _| {
        view.lifetime == Lifetime::Persistent && view.state == PanelState::Validated
    });
    assert_eq!(source, "one");
    assert!(view.file_error.is_none());
    assert_eq!(read(&rig.panel_dir("s").join("content.md")), "one");
    assert!(read(&rig.panel_dir("s").join("panel.json")).contains("\"renderer\": \"markdown\""));
    assert!(!folder.exists());
    assert_eq!(rig.server.stats().connections(), 0);
}
