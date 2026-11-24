//! The scratch half of the panel store: scratch panels, their load at start, the stamps and the
//! 7-day sweep, pin, and the rule that keeps a `.workspace/panels/<id>` folder out of the way of
//! an open scratch panel with the same ID.
//!
//! This is a child module of the store, so it works on the store's private state directly.

use combine_protocol::PanelState;

use super::{
    file_path, outside_source, split_read, too_large_source, unread_refusal, FileWrite,
    FolderState, Got, IdSource, Missing, OpKind, Out, PanelApiError, PanelEffect, PanelEvent,
    PanelFolder, PanelStore, PanelView, Record, SaveEnd, SaveReport, SaveTicket, ScratchGlobal,
    ScratchLoad, SpaceState, Stamp, SCRATCH_ID_OPEN, SCRATCH_LIST_MAX, SCRATCH_TTL_MS,
};
use crate::core::layout_sync::store_error_detail;
use crate::core::panel_file::{
    content_file, encode_scratch_meta, encode_view_panel, parse_scratch_meta, parse_view_panel,
    SCRATCH_FILE, SCRATCH_FILE_MAX_BYTES,
};
use crate::core::panels::{panel_folder_refusal, PanelSkip, PANELS_DIR, PANEL_FILE};
use crate::core::spawn::PanelRead;
use crate::model::{Lifetime, Millis, SpaceId};
use crate::ports::{FileMark, ScratchDir, ScratchListing};

/// Why a scratch folder's name can't be one of ours, if it can't.
fn dir_refusal(dir: &ScratchDir) -> Option<String> {
    let hex = dir.key.len() == 64
        && dir
            .key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !hex {
        return Some("the folder name isn't a space key".to_owned());
    }
    panel_folder_refusal(&dir.panel)
}

fn skipped_dir(dir: &ScratchDir, reason: &str) -> PanelEffect {
    PanelEffect::Log {
        text: format!(
            "the scratch folder {}/{} is skipped: {reason}",
            dir.key, dir.panel
        ),
    }
}

/// The scratch-save failure inside a file error, without a `Couldn't pin: ` reason before it.
fn raw_save_failure(error: &str) -> Option<String> {
    const RAW: &str = "Couldn't save the scratch panel ";
    if !error.starts_with("Couldn't pin: ") {
        return Some(error.to_owned());
    }
    error
        .find(RAW)
        .and_then(|at| error.get(at..))
        .map(str::to_owned)
}

impl SpaceState {
    pub(super) fn scratch_dir(&self, id: &str) -> ScratchDir {
        ScratchDir {
            key: self.id.sha256_hex(),
            panel: id.to_owned(),
        }
    }

    /// The ID is held by a live Scratch record or by a scratch panel still being read.
    pub(super) fn scratch_held(&self, id: &str) -> bool {
        self.scratch_reading.contains_key(id)
            || self
                .records
                .get(id)
                .is_some_and(|rec| rec.lifetime == Lifetime::Scratch && rec.live())
    }

    /// A scratch folder of this space is still being read: any, or the one named `id`.
    pub(super) fn scratch_loading(&self, scratch: &ScratchGlobal, id: Option<&str>) -> bool {
        let key = self.id.sha256_hex();
        let named = |dir: &ScratchDir| id.is_none_or(|id| dir.panel == id);
        scratch
            .pending
            .iter()
            .any(|dir| dir.key == key && named(dir))
            || self.waiting_scratch.iter().any(named)
            || self.scratch_reading.values().any(|load| named(&load.dir))
    }

    // ---- the shadow rule

    /// The folder of a scratch-held ID is never read: it is listed as skipped instead.
    pub(super) fn shadow(&mut self, id: &str, symlink: bool) {
        self.folders
            .insert(id.to_owned(), FolderState::Shadowed { symlink });
        self.skipped
            .insert(id.to_owned(), SCRATCH_ID_OPEN.to_owned());
        self.debounce.remove(id);
        self.reread_at.remove(id);
    }

    /// The symlink flag a folder that is shadowed again keeps.
    pub(super) fn shadow_symlink(&self, id: &str) -> bool {
        matches!(
            self.folders.get(id),
            Some(FolderState::Shadowed { symlink: true } | FolderState::Symlink)
        )
    }

    /// A read of a workspace folder came back for an ID a scratch panel has taken since the read
    /// was asked for: it is dropped, and no record is made.
    pub(super) fn drop_held_read(&mut self, id: &str) {
        self.reading.remove(id);
        self.shadow(id, false);
    }

    /// The scratch panel no longer holds `id`: a shadowed folder is the workspace's again.
    pub(super) fn release_shadow(&mut self, out: &mut Out, id: &str) {
        let Some(FolderState::Shadowed { symlink }) = self.folders.get(id).copied() else {
            return;
        };
        self.skipped.remove(id);
        if symlink {
            self.folders.insert(id.to_owned(), FolderState::Symlink);
            self.skipped
                .insert(id.to_owned(), PanelSkip::Symlink.reason());
        } else {
            self.folders.remove(id);
            self.first_sight(out, id);
        }
    }

    // ---- close, remove, sweep

    /// A scratch panel ends: its record is dropped and remembered as removed.
    fn drop_scratch(&mut self, out: &mut Out, id: &str) {
        let Some(rec) = self.records.remove(id) else {
            return;
        };
        out.events.push(PanelEvent::Changed {
            space: self.id.clone(),
            panel: id.to_owned(),
            revision: rec.revision,
            state: PanelState::Removed,
        });
        self.remember(id, Lifetime::Scratch, rec.file, rec.revision);
        self.debounce.remove(id);
        self.reread_at.remove(id);
        self.deferred.remove(id);
    }

    fn delete_scratch_dir(&mut self, out: &mut Out, dir: ScratchDir, swept: Option<Stamp>) {
        let ticket = out.ticket(
            &self.id,
            &dir.panel,
            OpKind::ScratchGone {
                dir: dir.clone(),
                swept,
            },
        );
        out.effects.push(PanelEffect::Delete {
            ticket,
            at: PanelFolder::Scratch(dir),
            files: Vec::new(),
        });
    }

    /// `close` and `remove` of a scratch panel.
    pub(super) fn end_scratch(
        &mut self,
        out: &mut Out,
        id: &str,
    ) -> Result<PanelView, PanelApiError> {
        let dir = self.scratch_dir(id);
        out.scratch.stamped.remove(&dir);
        self.drop_scratch(out, id);
        self.delete_scratch_dir(out, dir, None);
        self.release_shadow(out, id);
        match self.remembered(id) {
            Some(entry) => Ok(self.removed_view(entry)),
            None => Err(PanelApiError::UnknownPanel { id: id.to_owned() }),
        }
    }

    /// Whether the sweep leaves `dir` for a later sweep.
    fn sweep_waits(&self, dir: &ScratchDir) -> bool {
        self.records
            .get(&dir.panel)
            .is_some_and(|rec| rec.pinning || rec.busy.is_some())
            || self.waiting_scratch.contains(dir)
            || self
                .scratch_reading
                .get(&dir.panel)
                .is_some_and(|load| load.dir == *dir)
    }

    /// An expired folder is deleted: a live Scratch record of this folder is dropped as close
    /// drops it, and nothing else with that ID is touched.
    fn sweep_drop(&mut self, out: &mut Out, dir: &ScratchDir) {
        let id = dir.panel.as_str();
        let ours = dir.key == self.id.sha256_hex()
            && self
                .records
                .get(id)
                .is_some_and(|rec| rec.lifetime == Lifetime::Scratch && rec.live());
        if ours {
            self.drop_scratch(out, id);
            self.release_shadow(out, id);
        }
    }

    // ---- saving

    /// Starts the next Save of a scratch panel, if it may run now.
    pub(super) fn service_scratch(&mut self, out: &mut Out, id: &str) {
        let dir = self.scratch_dir(id);
        let stamped = out
            .scratch
            .stamped
            .get(&dir)
            .map(|stamp| stamp.changed_at_ms);
        let Some(rec) = self.records.get_mut(id) else {
            return;
        };
        if stamped.is_none() {
            rec.pending.remove(SCRATCH_FILE);
        }
        if rec.pending.is_empty() {
            return;
        }
        let content = content_file(rec.file.renderer);
        let mut files = Vec::new();
        if let (true, Some(changed_at_ms)) = (rec.pending.contains(SCRATCH_FILE), stamped) {
            files.push(FileWrite {
                file: SCRATCH_FILE,
                bytes: encode_scratch_meta(&self.id, changed_at_ms),
                expected: None,
            });
        }
        if rec.pending.contains(content) {
            files.push(FileWrite {
                file: content,
                bytes: rec.source.as_bytes().to_vec(),
                expected: None,
            });
        }
        if rec.pending.contains(PANEL_FILE) {
            files.push(FileWrite {
                file: PANEL_FILE,
                bytes: encode_view_panel(&rec.file),
                expected: None,
            });
        }
        rec.save_retry_at = None;
        let ticket = out.ticket(&self.id, id, OpKind::ScratchSave);
        rec.busy = Some(ticket);
        out.effects.push(PanelEffect::Save {
            ticket,
            at: PanelFolder::Scratch(dir),
            files,
        });
    }

    pub(super) fn scratch_saved(
        &mut self,
        out: &mut Out,
        id: &str,
        ticket: SaveTicket,
        report: SaveReport,
        now: Millis,
    ) {
        let Some(rec) = self.records.get_mut(id) else {
            return;
        };
        if rec.busy != Some(ticket) {
            return;
        }
        rec.busy = None;
        for (file, _) in &report.written {
            rec.pending.remove(file);
        }
        let detail = match report.end {
            SaveEnd::Done => {
                if rec.save_failed {
                    rec.save_failed = false;
                    rec.file_error = None;
                    self.changed(out, id);
                }
                self.service(out, id);
                return;
            }
            SaveEnd::Conflict { file } => format!("{file} changed"),
            SaveEnd::Failed { error, .. } => store_error_detail(error),
        };
        rec.file_error = Some(format!(
            "Couldn't save the scratch panel {id} ({detail}); retrying."
        ));
        rec.save_failed = true;
        rec.save_retry_at = Some(now.saturating_add(Millis(self.timing.retry_ms)));
        self.changed(out, id);
    }

    // ---- pin

    pub(super) fn pin(&mut self, out: &mut Out, id: &str) -> Result<PanelView, PanelApiError> {
        let Some(rec) = self.records.get(id) else {
            return self.missing(&out.scratch, id, Missing::Refuse);
        };
        if !rec.live() {
            return Err(PanelApiError::Removed { id: id.to_owned() });
        }
        if rec.read_only {
            return Err(PanelApiError::ReadOnly);
        }
        if rec.lifetime == Lifetime::Persistent {
            return Err(PanelApiError::NotScratch { id: id.to_owned() });
        }
        if let Some(unread) = &rec.unread {
            return Err(unread_refusal(id, unread));
        }
        let folder = self.folders.get(id).copied();
        if matches!(
            folder,
            Some(
                FolderState::Panel
                    | FolderState::Symlink
                    | FolderState::Reading
                    | FolderState::NotRead
                    | FolderState::Shadowed { symlink: true }
            )
        ) {
            return Err(PanelApiError::IdTaken { id: id.to_owned() });
        }
        let into_hand_made = matches!(folder, Some(FolderState::Shadowed { symlink: false }));
        self.skipped.remove(id);
        self.folders.insert(id.to_owned(), FolderState::Panel);
        let at = PanelFolder::Workspace {
            space: self.id.clone(),
            panel: id.to_owned(),
        };
        let ticket = out.ticket(&self.id, id, OpKind::Save);
        if let Some(rec) = self.records.get_mut(id) {
            rec.lifetime = Lifetime::Persistent;
            rec.pinning = true;
            rec.scratch_unsaved = rec.save_failed || rec.busy.is_some() || !rec.pending.is_empty();
            rec.scratch_failure = if rec.save_failed {
                rec.file_error
                    .take()
                    .and_then(|error| raw_save_failure(&error))
            } else {
                None
            };
            rec.pending.clear();
            rec.save_retry_at = None;
            if rec.save_failed {
                rec.save_failed = false;
                rec.file_error = None;
            }
            rec.busy = Some(ticket);
            let files = vec![
                FileWrite {
                    file: content_file(rec.file.renderer),
                    bytes: rec.source.as_bytes().to_vec(),
                    expected: into_hand_made.then_some(FileMark::Missing),
                },
                FileWrite {
                    file: PANEL_FILE,
                    bytes: encode_view_panel(&rec.file),
                    expected: Some(FileMark::Missing),
                },
            ];
            out.effects.push(PanelEffect::Save { ticket, at, files });
        }
        self.abandon_reads(id);
        self.changed(out, id);
        self.view_now(id)
    }

    /// The Save of a pin ended.
    pub(super) fn pin_ended(&mut self, out: &mut Out, id: &str, report: SaveReport, now: Millis) {
        let SaveReport { written, end } = report;
        let reason = match end {
            SaveEnd::Done => {
                self.pin_done(out, id);
                return;
            }
            SaveEnd::Conflict { file } => {
                format!("{} changed while it was written", file_path(id, file))
            }
            SaveEnd::Failed { error, .. } => store_error_detail(error),
        };
        self.pin_undone(out, id, written, reason, now);
    }

    fn pin_done(&mut self, out: &mut Out, id: &str) {
        let dir = self.scratch_dir(id);
        if let Some(rec) = self.records.get_mut(id) {
            rec.pinning = false;
            rec.file_error = None;
        }
        self.changed(out, id);
        out.scratch.stamped.remove(&dir);
        self.delete_scratch_dir(out, dir, None);
        self.service(out, id);
        self.release_deferred(out, id);
    }

    /// The pin failed: the panel is a scratch panel again, and what the pin wrote is removed.
    fn pin_undone(
        &mut self,
        out: &mut Out,
        id: &str,
        written: Vec<(&'static str, crate::model::ContentHash)>,
        reason: String,
        now: Millis,
    ) {
        let dir = self.scratch_dir(id);
        let live = self.records.get(id).is_some_and(Record::live);
        if !written.is_empty() {
            let files = written
                .into_iter()
                .map(|(file, hash)| (file, FileMark::Hash(hash)))
                .collect();
            let ticket = out.ticket(&self.id, id, OpKind::Cleanup);
            out.effects.push(PanelEffect::Delete {
                ticket,
                at: PanelFolder::Workspace {
                    space: self.id.clone(),
                    panel: id.to_owned(),
                },
                files,
            });
        }
        self.deferred.remove(id);
        if live {
            if let Some(rec) = self.records.get_mut(id) {
                rec.lifetime = Lifetime::Scratch;
                rec.pinning = false;
                rec.known = (FileMark::Missing, FileMark::Missing);
                let unsaved = std::mem::take(&mut rec.scratch_unsaved);
                let failure = rec.scratch_failure.take();
                rec.file_error = Some(match &failure {
                    Some(failure) => format!("Couldn't pin: {reason}. {failure}"),
                    None => format!("Couldn't pin: {reason}"),
                });
                rec.save_failed = failure.is_some();
                if unsaved {
                    rec.pending.insert(SCRATCH_FILE);
                    rec.pending.insert(content_file(rec.file.renderer));
                    rec.pending.insert(PANEL_FILE);
                }
                if !rec.pending.is_empty() {
                    rec.pending.insert(SCRATCH_FILE);
                }
                if failure.is_some() {
                    rec.save_retry_at = Some(now.saturating_add(Millis(self.timing.retry_ms)));
                }
            }
            self.shadow(id, false);
            out.effects.push(PanelEffect::ListWorkspace {
                space: self.id.clone(),
            });
            self.changed(out, id);
            if self
                .records
                .get(id)
                .is_some_and(|rec| rec.save_retry_at.is_none())
            {
                self.service(out, id);
            }
        } else {
            if let Some(rec) = self.records.remove(id) {
                self.remember(id, Lifetime::Scratch, rec.file, rec.revision);
            }
            out.scratch.stamped.remove(&dir);
            self.delete_scratch_dir(out, dir, None);
            self.folders.remove(id);
            self.skipped.remove(id);
            out.effects.push(PanelEffect::ListWorkspace {
                space: self.id.clone(),
            });
        }
    }

    /// The report of the delete that undoes a pin changes nothing; a failure is logged.
    pub(super) fn cleanup_ended(&mut self, id: &str, report: SaveReport, out: &mut Out) {
        let detail = match report.end {
            SaveEnd::Done => return,
            SaveEnd::Conflict { file } => format!("{file} changed"),
            SaveEnd::Failed { error, .. } => store_error_detail(error),
        };
        self.log(
            out,
            format!(
                "couldn't undo the pin of the panel {id} of {} ({detail})",
                self.id.0
            ),
        );
    }

    // ---- the scratch load

    /// A scratch folder is read, once the space is listed: unless the workspace already holds
    /// its ID, which wins.
    pub(super) fn admit(&mut self, out: &mut Out, dir: ScratchDir) {
        let id = dir.panel.clone();
        let held = self.records.contains_key(&id)
            || matches!(
                self.folders.get(&id),
                Some(
                    FolderState::Panel
                        | FolderState::Symlink
                        | FolderState::Reading
                        | FolderState::NotRead
                )
            );
        if held {
            self.log(
                out,
                format!(
                    "the scratch panel {id} of {} isn't loaded: .workspace/{}/{id} already holds that ID; its scratch folder is deleted when it expires",
                    self.id.0,
                    PANELS_DIR
                ),
            );
            return;
        }
        out.effects.push(PanelEffect::Read {
            at: PanelFolder::Scratch(dir.clone()),
            file: PANEL_FILE,
        });
        self.scratch_reading
            .insert(id, ScratchLoad { dir, file: None });
    }

    pub(super) fn admit_waiting(&mut self, out: &mut Out) {
        for dir in std::mem::take(&mut self.waiting_scratch) {
            self.admit(out, dir);
        }
    }

    /// A scratch panel can't be loaded: it is logged, never stamped again, and its ID is free.
    fn scratch_skipped(&mut self, out: &mut Out, dir: &ScratchDir, reason: &str) {
        let id = dir.panel.as_str();
        self.scratch_reading.remove(id);
        out.scratch.stamped.remove(dir);
        self.log(
            out,
            format!(
                "the scratch panel {id} of {} is skipped: {reason}",
                self.id.0
            ),
        );
        self.release_shadow(out, id);
    }

    /// One result of the files of an admitted scratch folder.
    pub(super) fn on_scratch_file(
        &mut self,
        out: &mut Out,
        dir: &ScratchDir,
        file: &str,
        got: Got,
        ids: &mut dyn IdSource,
    ) {
        let id = dir.panel.as_str();
        let Some(load) = self.scratch_reading.get(id) else {
            return;
        };
        if load.dir != *dir {
            return;
        }
        match &load.file {
            None if file == PANEL_FILE => self.scratch_json(out, dir, got),
            Some((parsed, _)) if file == content_file(parsed.renderer) => {
                self.scratch_content(out, dir, file, got, ids);
            }
            None | Some(_) => {}
        }
    }

    fn scratch_json(&mut self, out: &mut Out, dir: &ScratchDir, got: Got) {
        let id = dir.panel.as_str();
        let skip = match got {
            Got::Bytes { bytes, hash } => match parse_view_panel(id, &bytes) {
                Ok(file) => {
                    let content = content_file(file.renderer);
                    if let Some(load) = self.scratch_reading.get_mut(id) {
                        load.file = Some((file, FileMark::Hash(hash)));
                    }
                    out.effects.push(PanelEffect::Read {
                        at: PanelFolder::Scratch(dir.clone()),
                        file: content,
                    });
                    return;
                }
                Err(skip) => skip,
            },
            Got::Missing => PanelSkip::NoFile,
            Got::TooLarge { size } => PanelSkip::TooLarge { size },
            Got::Failed(detail) => PanelSkip::ReadFailed(detail),
        };
        self.scratch_skipped(out, dir, &skip.reason());
    }

    fn scratch_content(
        &mut self,
        out: &mut Out,
        dir: &ScratchDir,
        file: &str,
        got: Got,
        ids: &mut dyn IdSource,
    ) {
        let id = dir.panel.as_str();
        let Some(load) = self.scratch_reading.remove(id) else {
            return;
        };
        let Some((parsed, _)) = load.file else {
            return;
        };
        let renderer = parsed.renderer;
        let (source, core_error) = match got {
            Got::Failed(detail) => {
                out.scratch.stamped.remove(dir);
                self.log(
                    out,
                    format!(
                        "the scratch panel {id} of {} is skipped: couldn't read {file} ({detail})",
                        self.id.0
                    ),
                );
                self.release_shadow(out, id);
                return;
            }
            Got::Missing => (String::new(), None),
            Got::TooLarge { size } => (String::new(), Some(too_large_source(size))),
            Got::Bytes { bytes, .. } => outside_source(&self.id, id, renderer, bytes),
        };
        let mut rec = Record::new(parsed, source, ids.take_id());
        rec.lifetime = Lifetime::Scratch;
        self.forget(id);
        self.records.insert(id.to_owned(), rec);
        self.begin_validation(out, id, core_error);
    }
}

impl PanelStore {
    /// The scratch folders were listed.
    pub(super) fn on_scratch_listing(&mut self, listing: ScratchListing) {
        self.out.scratch.listed = true;
        self.out.scratch.list_failed = None;
        if listing.truncated {
            self.out.effects.push(PanelEffect::Log {
                text: format!(
                    "there are more than {SCRATCH_LIST_MAX} scratch folders; only some were read"
                ),
            });
        }
        for dir in listing.dirs {
            if let Some(reason) = dir_refusal(&dir) {
                self.out.effects.push(skipped_dir(&dir, &reason));
                continue;
            }
            self.out.scratch.pending.insert(dir.clone());
            self.out.effects.push(PanelEffect::Read {
                at: PanelFolder::Scratch(dir),
                file: SCRATCH_FILE,
            });
        }
    }

    /// One result of a scratch folder's file.
    pub(super) fn on_scratch_read(
        &mut self,
        dir: &ScratchDir,
        file: &str,
        read: PanelRead,
        now: Millis,
        ids: &mut dyn IdSource,
    ) {
        let (got, _) = split_read(read);
        if file == SCRATCH_FILE {
            self.on_scratch_meta(dir, got, now);
            return;
        }
        let Some(space) = self
            .out
            .scratch
            .stamped
            .get(dir)
            .map(|stamp| stamp.space.clone())
        else {
            return;
        };
        self.in_space(&space, |state, out| {
            state.on_scratch_file(out, dir, file, got, ids);
        });
    }

    fn on_scratch_meta(&mut self, dir: &ScratchDir, got: Got, now: Millis) {
        if !self.out.scratch.pending.remove(dir) {
            return;
        }
        let reason = match got {
            Got::Bytes { bytes, .. } => match parse_scratch_meta(&dir.key, &bytes) {
                Ok(meta) => {
                    self.out.scratch.stamped.insert(
                        dir.clone(),
                        Stamp {
                            space: meta.space.clone(),
                            changed_at_ms: meta.changed_at_ms,
                        },
                    );
                    self.add_space(&meta.space, now);
                    self.in_space(&meta.space, |state, out| {
                        if state.listed {
                            state.admit(out, dir.clone());
                        } else {
                            state.waiting_scratch.push(dir.clone());
                        }
                    });
                    return;
                }
                Err(reason) => reason,
            },
            Got::Missing => format!("{SCRATCH_FILE} is missing"),
            Got::TooLarge { size } => {
                format!("{SCRATCH_FILE} is {size} bytes; the limit is {SCRATCH_FILE_MAX_BYTES}")
            }
            Got::Failed(detail) => format!("couldn't read {SCRATCH_FILE} ({detail})"),
        };
        self.out.effects.push(skipped_dir(dir, &reason));
    }

    /// Deletes every folder whose panel was last changed 7 days ago or more.
    pub(super) fn sweep(&mut self, wall_ms: u64) {
        let expired: Vec<(ScratchDir, Stamp)> = self
            .out
            .scratch
            .stamped
            .iter()
            .filter(|(_, stamp)| wall_ms >= stamp.changed_at_ms.saturating_add(SCRATCH_TTL_MS))
            .map(|(dir, stamp)| (dir.clone(), stamp.clone()))
            .collect();
        for (dir, stamp) in expired {
            let waits = self
                .spaces
                .get(&stamp.space)
                .is_some_and(|state| state.sweep_waits(&dir))
                || self.out.saves.values().any(
                    |op| matches!(&op.kind, OpKind::ScratchGone { dir: gone, .. } if *gone == dir),
                );
            if waits {
                continue;
            }
            let ticket = self.out.ticket(
                &stamp.space,
                &dir.panel,
                OpKind::ScratchGone {
                    dir: dir.clone(),
                    swept: Some(stamp.clone()),
                },
            );
            self.out.effects.push(PanelEffect::Delete {
                ticket,
                at: PanelFolder::Scratch(dir.clone()),
                files: Vec::new(),
            });
            self.in_space(&stamp.space, |state, out| state.sweep_drop(out, &dir));
        }
    }

    /// A scratch folder's delete ended. A sweep clears only the stamp it acted on.
    pub(super) fn scratch_gone(
        &mut self,
        space: &SpaceId,
        dir: &ScratchDir,
        swept: Option<Stamp>,
        report: SaveReport,
    ) {
        let detail = match report.end {
            SaveEnd::Done => {
                if let Some(swept) = swept {
                    if self.out.scratch.stamped.get(dir) == Some(&swept) {
                        self.out.scratch.stamped.remove(dir);
                    }
                }
                return;
            }
            SaveEnd::Conflict { file } => format!("{file} changed"),
            SaveEnd::Failed { error, .. } => store_error_detail(error),
        };
        self.out.effects.push(PanelEffect::Log {
            text: format!(
                "couldn't remove the scratch folder {}/{} of {} ({detail})",
                dir.key, dir.panel, space.0
            ),
        });
    }
}
