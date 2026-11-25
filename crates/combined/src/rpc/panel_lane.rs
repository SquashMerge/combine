//! The panel lane: the panel store's file effects run on a blocking thread, one at a time, so
//! a slow store never holds the server loop. The loop applies each outcome.

use std::sync::Arc;

use futures::future::BoxFuture;
use tokio::task::JoinError;

use crate::core::{
    panel_file_bound, FileWrite, PanelEffect, PanelFolder, PanelRead, SaveEnd, SaveReport,
    SaveTicket, DS_STORE, PANELS_DIR, PANEL_FILE, PANEL_LIST_MAX, SCRATCH_FILE, SCRATCH_LIST_MAX,
};
use crate::model::{ContentHash, SpaceId};
use crate::ports::{DirListing, FileMark, FileStore, ScratchListing, ScratchStore, StoreError};

/// The stores the lane's effects use.
pub(super) struct PanelIo {
    store: Arc<dyn FileStore>,
    scratch: Arc<dyn ScratchStore>,
}

/// What one effect found, for the store's `apply_*` call.
pub(super) enum LaneOutcome {
    Watched {
        space: SpaceId,
        result: Result<(), StoreError>,
    },
    Listed {
        space: SpaceId,
        listing: Result<Option<DirListing>, StoreError>,
    },
    ScratchListed {
        listing: Result<ScratchListing, StoreError>,
    },
    Read {
        at: PanelFolder,
        file: &'static str,
        read: PanelRead,
    },
    Saved {
        ticket: SaveTicket,
        report: SaveReport,
    },
    Deleted {
        ticket: SaveTicket,
        report: SaveReport,
    },
}

impl PanelIo {
    pub(super) fn new(store: Arc<dyn FileStore>, scratch: Arc<dyn ScratchStore>) -> PanelIo {
        PanelIo { store, scratch }
    }

    /// Starts `effect` on a blocking thread, which drives the store's futures to the end. For
    /// `Watch`, `ListWorkspace`, `ListScratch`, `Read`, `Save` and `Delete` only: the server
    /// runs `Check` and `Log` itself.
    ///
    /// It must be called inside the server's runtime.
    pub(super) fn start(
        &self,
        effect: PanelEffect,
    ) -> BoxFuture<'static, Result<LaneOutcome, JoinError>> {
        let store = Arc::clone(&self.store);
        let scratch = Arc::clone(&self.scratch);
        let work = async move { run(&*store, &*scratch, effect).await };
        let handle = tokio::runtime::Handle::current();
        Box::pin(tokio::task::spawn_blocking(move || handle.block_on(work)))
    }
}

async fn run(
    store: &dyn FileStore,
    scratch: &dyn ScratchStore,
    effect: PanelEffect,
) -> LaneOutcome {
    match effect {
        PanelEffect::Watch { space } => {
            let result = store.watch(&space).await;
            LaneOutcome::Watched { space, result }
        }
        PanelEffect::ListWorkspace { space } => {
            let listing = store.list_dir(&space, PANELS_DIR, PANEL_LIST_MAX).await;
            LaneOutcome::Listed { space, listing }
        }
        PanelEffect::ListScratch => LaneOutcome::ScratchListed {
            listing: scratch.list(SCRATCH_LIST_MAX).await,
        },
        PanelEffect::Read { at, file } => {
            let bound = panel_file_bound(file);
            let read = match &at {
                PanelFolder::Workspace { space, panel } => {
                    let path = format!("{PANELS_DIR}/{panel}/{file}");
                    store.read(space, &path, bound).await
                }
                PanelFolder::Scratch(dir) => scratch.read(dir, file, bound).await,
            };
            LaneOutcome::Read {
                at,
                file,
                read: PanelRead::from(read),
            }
        }
        PanelEffect::Save { ticket, at, files } => LaneOutcome::Saved {
            ticket,
            report: run_save(store, scratch, &at, files).await,
        },
        PanelEffect::Delete { ticket, at, files } => LaneOutcome::Deleted {
            ticket,
            report: run_delete(store, scratch, &at, files).await,
        },
        PanelEffect::Check { .. } | PanelEffect::Log { .. } => {
            unreachable!("the server runs checks and log lines itself")
        }
    }
}

/// Writes a save's files in order and stops at the first failure or conflict.
///
/// A workspace save writes only into the project, never the fallback copy, and compares
/// each file with what the store last saw right before it writes it. A create or pin lists
/// the content file first, so `panel.json` is compared up front: nothing is written into a
/// folder that already holds one. The window between a compare and the rename stays open:
/// an outside write landing in it is overwritten (at create and pin it runs from the
/// up-front compare to the content write). A transient error in the up-front read counts
/// as a conflict. A scratch save is not compared, because only the daemon writes there.
async fn run_save(
    store: &dyn FileStore,
    scratch: &dyn ScratchStore,
    at: &PanelFolder,
    files: Vec<FileWrite>,
) -> SaveReport {
    let mut written: Vec<(&'static str, ContentHash)> = Vec::new();
    match at {
        PanelFolder::Scratch(dir) => {
            for file in files {
                match scratch.write(dir, file.file, file.bytes).await {
                    Ok(outcome) => written.push((file.file, outcome.hash)),
                    Err(error) => {
                        return SaveReport {
                            written,
                            end: SaveEnd::Failed {
                                file: file.file,
                                error,
                            },
                        }
                    }
                }
            }
        }
        PanelFolder::Workspace { space, panel } => {
            let checks_first = files.first().is_some_and(|w| w.file != PANEL_FILE)
                && files
                    .iter()
                    .any(|w| w.file == PANEL_FILE && matches!(w.expected, Some(FileMark::Missing)));
            if checks_first {
                let path = format!("{PANELS_DIR}/{panel}/{PANEL_FILE}");
                let read = store.read(space, &path, panel_file_bound(PANEL_FILE)).await;
                if !FileMark::Missing.matches_project(&read) {
                    return SaveReport {
                        written,
                        end: SaveEnd::Conflict { file: PANEL_FILE },
                    };
                }
            }
            for file in files {
                let path = format!("{PANELS_DIR}/{panel}/{}", file.file);
                if let Some(expected) = file.expected {
                    let read = store.read(space, &path, panel_file_bound(file.file)).await;
                    if !expected.matches_project(&read) {
                        return SaveReport {
                            written,
                            end: SaveEnd::Conflict { file: file.file },
                        };
                    }
                }
                match store.write_project(space, &path, file.bytes).await {
                    Ok(outcome) => written.push((file.file, outcome.hash)),
                    Err(error) => {
                        return SaveReport {
                            written,
                            end: SaveEnd::Failed {
                                file: file.file,
                                error,
                            },
                        }
                    }
                }
            }
        }
    }
    SaveReport {
        written,
        end: SaveEnd::Done,
    }
}

/// Removes a panel's files. A workspace delete compares every listed file first and removes
/// nothing when one differs; then it removes them in order, then Finder's `.DS_Store`
/// (uncompared, and a failure is only logged), then the folder if it is empty. A folder
/// that stays behind with no `panel.json` is no panel, so that failure is only logged too.
async fn run_delete(
    store: &dyn FileStore,
    scratch: &dyn ScratchStore,
    at: &PanelFolder,
    files: Vec<(&'static str, FileMark)>,
) -> SaveReport {
    let done = |end| SaveReport {
        written: Vec::new(),
        end,
    };
    match at {
        PanelFolder::Scratch(dir) => match scratch.remove(dir).await {
            Ok(()) => done(SaveEnd::Done),
            Err(error) => done(SaveEnd::Failed {
                file: SCRATCH_FILE,
                error,
            }),
        },
        PanelFolder::Workspace { space, panel } => {
            for (file, expected) in &files {
                let path = format!("{PANELS_DIR}/{panel}/{file}");
                let read = store.read(space, &path, panel_file_bound(file)).await;
                if !expected.matches_project(&read) {
                    return done(SaveEnd::Conflict { file });
                }
            }
            for (file, _) in &files {
                let path = format!("{PANELS_DIR}/{panel}/{file}");
                if let Err(error) = store.remove_file(space, &path).await {
                    return done(SaveEnd::Failed { file, error });
                }
            }
            let path = format!("{PANELS_DIR}/{panel}/{DS_STORE}");
            if let Err(error) = store.remove_file(space, &path).await {
                log_remove_failure(&path, &error);
            }
            let folder = format!("{PANELS_DIR}/{panel}");
            if let Err(error) = store.remove_dir(space, &folder).await {
                log_remove_failure(&folder, &error);
            }
            done(SaveEnd::Done)
        }
    }
}

fn log_remove_failure(path: &str, error: &StoreError) {
    eprintln!("combined: cannot remove .workspace/{path}: {error}");
}
