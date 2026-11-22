//! Layout sync: one state machine for every space's `layout.json`. No I/O, no clock.
//!
//! The server feeds it what it learned from the file store (reads, write outcomes, change
//! events), what clients asked for (watch, unwatch, mutate) and the time. It answers with
//! effects to run, replies to send and spaces whose layout changed. Time arrives as [`Millis`].
//!
//! Per space it keeps the layout as last read from disk (`disk`), that layout with the pending
//! ops applied (`current`), and the hash of the last bytes read or written (`known_hash`), which
//! is how the daemon tells its own writes from outside edits.

use std::collections::{BTreeMap, BTreeSet};

use combine_protocol as wire;

use crate::core::effects::ConnectionId;
use crate::core::layout::{
    apply_ops, default_layout, diff_fields, encode_layout, identity_origins, parse_layout,
    validate_layout, FieldPath, LayoutError, LayoutOp, Origins, LAYOUT_MAX_BYTES,
};
use crate::model::{
    ContentHash, LayoutRevision, Millis, PanelId, SpaceId, SpaceLayout, SplitDir, SplitNode, TileId,
};
use crate::ports::{StoreError, StoredFile, WriteOutcome};

/// How long the sync waits before writing, and before retrying a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutTiming {
    /// Quiet time after the latest mutation before the batch is written.
    pub debounce_ms: u64,
    /// The longest a batch may wait, counted from its first mutation, however busy.
    pub max_wait_ms: u64,
    /// The wait before trying again after a failed or read-only write.
    pub retry_ms: u64,
}

impl Default for LayoutTiming {
    fn default() -> LayoutTiming {
        LayoutTiming {
            debounce_ms: 500,
            max_wait_ms: 2_000,
            retry_ms: 5_000,
        }
    }
}

/// What the file store found when it read a space's `layout.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutRead {
    Missing,
    Found {
        bytes: Vec<u8>,
        hash: ContentHash,
        read_only: bool,
    },
    /// The read itself failed, so nothing is known about the file's content.
    Failed(String),
}

/// How a write ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutWritten {
    Ok {
        hash: ContentHash,
        read_only: bool,
    },
    /// The project can't be written and there is no fallback.
    ReadOnly(String),
    Failed(String),
}

impl From<StoredFile> for LayoutRead {
    fn from(file: StoredFile) -> LayoutRead {
        LayoutRead::Found {
            bytes: file.bytes,
            hash: file.hash,
            read_only: file.read_only,
        }
    }
}

impl From<Option<StoredFile>> for LayoutRead {
    fn from(file: Option<StoredFile>) -> LayoutRead {
        file.map_or(LayoutRead::Missing, LayoutRead::from)
    }
}

impl From<StoreError> for LayoutRead {
    fn from(error: StoreError) -> LayoutRead {
        LayoutRead::Failed(store_error_detail(error))
    }
}

impl From<WriteOutcome> for LayoutWritten {
    fn from(outcome: WriteOutcome) -> LayoutWritten {
        LayoutWritten::Ok {
            hash: outcome.hash,
            read_only: outcome.read_only,
        }
    }
}

impl From<StoreError> for LayoutWritten {
    fn from(error: StoreError) -> LayoutWritten {
        match error {
            StoreError::ReadOnly(detail) => LayoutWritten::ReadOnly(detail),
            other => LayoutWritten::Failed(store_error_detail(other)),
        }
    }
}

fn store_error_detail(error: StoreError) -> String {
    match error {
        StoreError::InvalidPath => "invalid path".to_string(),
        StoreError::Unavailable(detail) | StoreError::ReadOnly(detail) => detail,
    }
}

/// Work for the server to do and report back with [`Layouts::apply_read`] or
/// [`Layouts::apply_write`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutEffect {
    Watch {
        space: SpaceId,
    },
    Read {
        space: SpaceId,
    },
    Write {
        space: SpaceId,
        bytes: Vec<u8>,
        op_count: usize,
    },
}

/// A reply owed to a client: the space's [`SpaceLayoutStatus`] at the time of sending, tagged
/// with the request it answers. A client that got a reply for the current revision doesn't need
/// the push for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutReply {
    pub conn: ConnectionId,
    pub request: u64,
    pub space: SpaceId,
}

/// Why a mutation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutateError {
    UnknownSpace,
    NotWatched,
    Stale,
    Invalid(String),
}

/// Why a watch was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchError {
    UnknownSpace,
}

/// A batch of ops that was thrown away because an outside edit conflicted with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedBatch {
    pub op_count: usize,
    pub fields: BTreeSet<FieldPath>,
}

/// What clients are told about a space.
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceLayoutStatus {
    pub layout: SpaceLayout,
    pub layout_revision: LayoutRevision,
    pub read_only: bool,
    pub error: Option<String>,
    pub dropped: Option<DroppedBatch>,
}

/// The most ops one `mutate_layout` may carry.
pub const MAX_WIRE_OPS: usize = 32;

/// The wire form of a space's layout for its clients. The wire tree has no `groups` or unknown
/// keys.
pub fn layout_snapshot(space: &SpaceId, status: &SpaceLayoutStatus) -> wire::LayoutSnapshot {
    wire::LayoutSnapshot {
        space: space.0.clone(),
        layout_revision: status.layout_revision.0,
        root: wire_node(&status.layout.root),
        read_only: status.read_only,
        error: status.error.clone(),
        dropped: status.dropped.as_ref().map(dropped_notice),
    }
}

fn wire_node(node: &SplitNode) -> wire::LayoutNode {
    match node {
        SplitNode::Split(split) => wire::LayoutNode::Split {
            dir: match split.dir {
                SplitDir::H => wire::SplitDirection::H,
                SplitDir::V => wire::SplitDirection::V,
            },
            ratio: wire::Ratio::new(split.ratio).unwrap_or(wire::Ratio::HALF),
            a: Box::new(wire_node(&split.a)),
            b: Box::new(wire_node(&split.b)),
        },
        SplitNode::Tile(tile) => wire::LayoutNode::Tile {
            id: tile.id.as_str().to_string(),
            panels: tile
                .panels
                .iter()
                .map(|panel| panel.as_str().to_string())
                .collect(),
            active: tile.active.as_ref().map(|panel| panel.as_str().to_string()),
        },
    }
}

fn dropped_notice(dropped: &DroppedBatch) -> wire::DroppedNotice {
    wire::DroppedNotice {
        op_count: u32::try_from(dropped.op_count).unwrap_or(u32::MAX),
        message: format!(
            "{} pending change(s) were dropped because the layout was edited outside Combine",
            dropped.op_count
        ),
    }
}

/// Converts the ops of a `mutate_layout` for [`Layouts::mutate`]. The batch holds 1 to
/// [`MAX_WIRE_OPS`] ops, and every ID must be valid. The error text is for the client.
pub fn ops_from_wire(ops: Vec<wire::LayoutOp>) -> Result<Vec<LayoutOp>, String> {
    if ops.is_empty() || ops.len() > MAX_WIRE_OPS {
        return Err(format!(
            "a mutation carries 1 to {MAX_WIRE_OPS} ops, not {}",
            ops.len()
        ));
    }
    ops.into_iter().map(op_from_wire).collect()
}

fn op_from_wire(op: wire::LayoutOp) -> Result<LayoutOp, String> {
    let tile = |id: String| TileId::new(id).map_err(|e| format!("invalid tile ID: {e}"));
    let panel = |id: String| PanelId::new(id).map_err(|e| format!("invalid panel ID: {e}"));
    Ok(match op {
        wire::LayoutOp::SetActive { tile: t, panel: p } => LayoutOp::SetActive {
            tile: tile(t)?,
            panel: panel(p)?,
        },
        wire::LayoutOp::SetRatio { leaves, ratio } => LayoutOp::SetRatio {
            leaves: leaves.into_iter().map(tile).collect::<Result<_, _>>()?,
            ratio: ratio.get(),
        },
        wire::LayoutOp::MovePanel {
            panel: p,
            to_tile,
            index,
        } => LayoutOp::MovePanel {
            panel: panel(p)?,
            to_tile: tile(to_tile)?,
            index: usize::try_from(index).unwrap_or(usize::MAX),
        },
        wire::LayoutOp::SplitTile {
            tile: t,
            dir,
            new_tile,
            ratio,
            panel: p,
        } => LayoutOp::SplitTile {
            tile: tile(t)?,
            dir: match dir {
                wire::SplitDirection::H => SplitDir::H,
                wire::SplitDirection::V => SplitDir::V,
            },
            new_tile: tile(new_tile)?,
            ratio: ratio.get(),
            panel: p.map(panel).transpose()?,
        },
        wire::LayoutOp::CloseTile { tile: t } => LayoutOp::CloseTile { tile: tile(t)? },
    })
}

fn invalid_file_error(reason: &str) -> String {
    format!(
        "layout.json can't be used: {reason}. Showing the last good layout. Changes made now \
         aren't saved, and are lost if the daemon restarts, until layout.json is fixed."
    )
}

fn read_failed_error(detail: &str) -> String {
    format!("Couldn't read layout.json ({detail}); showing the last good layout.")
}

fn read_only_error(detail: &str) -> String {
    format!("Can't write .workspace/ ({detail}). Changes aren't saved, and are lost if the daemon restarts.")
}

fn write_failed_error(detail: &str) -> String {
    format!("Couldn't save layout.json ({detail}); retrying.")
}

/// What an error is about, which decides what clears it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorSource {
    /// A read found a file that can't be used. A valid read clears it.
    Invalid,
    /// A read failed. A successful read clears it.
    Read,
    /// A write failed. A successful write clears it.
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpaceError {
    source: ErrorSource,
    text: String,
}

#[derive(Debug, Clone)]
struct TimedOp {
    op: LayoutOp,
    at: Millis,
}

#[derive(Debug, Clone)]
struct Pending {
    ops: Vec<TimedOp>,
    /// Fields the ops touch, in the coordinates of `disk`.
    touched: BTreeSet<FieldPath>,
}

impl Pending {
    fn plain_ops(&self) -> Vec<LayoutOp> {
        self.ops.iter().map(|timed| timed.op.clone()).collect()
    }

    fn first_at(&self) -> Option<Millis> {
        self.ops.first().map(|timed| timed.at)
    }
}

/// A write that was emitted and not yet acknowledged.
#[derive(Debug, Clone)]
struct Writing {
    layout: SpaceLayout,
    /// How many of the pending ops the write carries.
    op_count: usize,
    /// The batch was dropped while the write was in flight, so every pending op is newer.
    cut_off: bool,
}

#[derive(Debug)]
struct SpaceState {
    loaded: bool,
    disk: SpaceLayout,
    current: SpaceLayout,
    origins: Origins,
    pending: Option<Pending>,
    deadline: Option<Millis>,
    writing: Option<Writing>,
    known_hash: Option<ContentHash>,
    revision: LayoutRevision,
    error: Option<SpaceError>,
    read_only: bool,
    dropped: Option<DroppedBatch>,
    /// The file can't be used, so nothing is written over it.
    invalid: bool,
    /// Watchers that asked before the first read finished.
    waiting: Vec<(ConnectionId, u64)>,
}

impl SpaceState {
    fn new() -> SpaceState {
        let layout = default_layout();
        SpaceState {
            loaded: false,
            origins: identity_origins(&layout),
            disk: layout.clone(),
            current: layout,
            pending: None,
            deadline: None,
            writing: None,
            known_hash: None,
            revision: LayoutRevision(0),
            error: None,
            read_only: false,
            dropped: None,
            invalid: false,
            waiting: Vec::new(),
        }
    }

    fn status(&self) -> SpaceLayoutStatus {
        SpaceLayoutStatus {
            layout: self.current.clone(),
            layout_revision: self.revision,
            read_only: self.read_only,
            error: self.error.as_ref().map(|error| error.text.clone()),
            dropped: self.dropped.clone(),
        }
    }

    fn arm_deadline(&mut self, now: Millis, timing: LayoutTiming) {
        self.deadline = match (&self.pending, self.invalid, &self.writing) {
            (Some(pending), false, None) => pending.first_at().map(|first| {
                let quiet = now.saturating_add(Millis(timing.debounce_ms));
                let latest = first.saturating_add(Millis(timing.max_wait_ms));
                quiet.min(latest)
            }),
            _ => None,
        };
    }

    fn clear_error(&mut self, sources: &[ErrorSource]) -> bool {
        let matches = self
            .error
            .as_ref()
            .is_some_and(|error| sources.contains(&error.source));
        if matches {
            self.error = None;
        }
        matches
    }

    /// A failed read left its error up, so the next read must run even if the file looks the
    /// same, to clear it.
    fn read_failed(&self) -> bool {
        self.error
            .as_ref()
            .is_some_and(|error| error.source == ErrorSource::Read)
    }

    fn set_error(&mut self, source: ErrorSource, text: String) -> bool {
        let error = SpaceError { source, text };
        let changed = self.error.as_ref() != Some(&error);
        self.error = Some(error);
        changed
    }

    /// Takes in a read. Returns whether clients must hear about it: every read that isn't
    /// ignored counts, and so does a failed read that changes the error.
    fn apply_read(&mut self, read: LayoutRead, now: Millis, timing: LayoutTiming) -> bool {
        let first = !self.loaded;
        self.loaded = true;
        match read {
            LayoutRead::Failed(detail) => {
                let changed = self.set_error(ErrorSource::Read, read_failed_error(&detail));
                changed && !first
            }
            LayoutRead::Missing => {
                if !first && self.known_hash.is_none() && !self.read_failed() {
                    return false;
                }
                self.known_hash = None;
                self.read_only = false;
                self.reload(Ok(default_layout()), first, now, timing);
                !first
            }
            LayoutRead::Found {
                bytes,
                hash,
                read_only,
            } => {
                if !first && self.known_hash == Some(hash) && !self.read_failed() {
                    return false;
                }
                self.known_hash = Some(hash);
                self.read_only = read_only;
                self.reload(parse_bytes(&bytes), first, now, timing);
                !first
            }
        }
    }

    fn reload(
        &mut self,
        parsed: Result<SpaceLayout, LayoutError>,
        first: bool,
        now: Millis,
        timing: LayoutTiming,
    ) {
        self.dropped = None;
        match parsed {
            Err(why) => {
                self.invalid = true;
                self.error = Some(SpaceError {
                    source: ErrorSource::Invalid,
                    text: invalid_file_error(&why.to_string()),
                });
                self.deadline = None;
            }
            Ok(layout) => {
                let was_invalid = self.invalid;
                self.invalid = false;
                self.clear_error(&[ErrorSource::Invalid, ErrorSource::Read]);
                if first {
                    self.origins = identity_origins(&layout);
                    self.disk = layout.clone();
                    self.current = layout;
                } else {
                    self.rebase(layout);
                }
                if was_invalid && self.pending.is_some() && self.writing.is_none() {
                    self.deadline = Some(now.saturating_add(Millis(timing.debounce_ms)));
                }
            }
        }
    }

    /// Moves the pending ops onto an outside edit, or drops them when they conflict with it.
    fn rebase(&mut self, new: SpaceLayout) {
        let Some(pending) = self.pending.take() else {
            self.origins = identity_origins(&new);
            self.disk = new.clone();
            self.current = new;
            return;
        };
        let changed = diff_fields(&self.disk, &new);
        let overlap: BTreeSet<FieldPath> =
            pending.touched.intersection(&changed).cloned().collect();
        let replay = if overlap.is_empty() {
            apply_ops(&new, &identity_origins(&new), &pending.plain_ops()).ok()
        } else {
            None
        };
        match replay {
            Some(applied) => {
                self.disk = new;
                self.current = applied.layout;
                self.origins = applied.origins;
                self.pending = Some(Pending {
                    ops: pending.ops,
                    touched: applied.touched,
                });
            }
            None => {
                self.dropped = Some(DroppedBatch {
                    op_count: pending.ops.len(),
                    fields: if overlap.is_empty() { changed } else { overlap },
                });
                self.origins = identity_origins(&new);
                self.disk = new.clone();
                self.current = new;
                self.deadline = None;
                if let Some(writing) = &mut self.writing {
                    writing.cut_off = true;
                }
            }
        }
    }

    /// Takes in a write outcome. Returns whether clients must hear about it.
    fn apply_written(&mut self, written: LayoutWritten, now: Millis, timing: LayoutTiming) -> bool {
        let writing = self.writing.take();
        match written {
            LayoutWritten::Ok { hash, read_only } => {
                self.known_hash = Some(hash);
                let mut changed = self.read_only != read_only;
                self.read_only = read_only;
                changed |= self.clear_error(&[ErrorSource::Write]);
                if let Some(writing) = writing {
                    changed |= self.finish_write(writing, now, timing);
                }
                changed
            }
            LayoutWritten::ReadOnly(detail) => {
                let mut changed = !self.read_only;
                self.read_only = true;
                changed |= self.set_error(ErrorSource::Write, read_only_error(&detail));
                self.retry_later(now, timing);
                changed
            }
            LayoutWritten::Failed(detail) => {
                let changed = self.set_error(ErrorSource::Write, write_failed_error(&detail));
                self.retry_later(now, timing);
                changed
            }
        }
    }

    fn retry_later(&mut self, now: Millis, timing: LayoutTiming) {
        self.deadline = (self.pending.is_some() && !self.invalid)
            .then(|| now.saturating_add(Millis(timing.retry_ms)));
    }

    /// The file now holds what was written. Ops added since stay pending on top of it.
    /// Returns whether the layout clients see changed.
    fn finish_write(&mut self, writing: Writing, now: Millis, timing: LayoutTiming) -> bool {
        let before = self.current.clone();
        let mut ops = self
            .pending
            .take()
            .map(|pending| pending.ops)
            .unwrap_or_default();
        let skip = if writing.cut_off {
            0
        } else {
            writing.op_count.min(ops.len())
        };
        let remaining = ops.split_off(skip);
        self.disk = writing.layout;
        self.origins = identity_origins(&self.disk);
        self.current = self.disk.clone();
        self.deadline = None;
        if remaining.is_empty() {
            return self.current != before;
        }
        let plain: Vec<LayoutOp> = remaining.iter().map(|timed| timed.op.clone()).collect();
        match apply_ops(&self.disk, &self.origins, &plain) {
            Ok(applied) => {
                self.current = applied.layout;
                self.origins = applied.origins;
                self.pending = Some(Pending {
                    ops: remaining,
                    touched: applied.touched,
                });
                self.arm_deadline(now, timing);
            }
            Err(_) => {
                self.dropped = Some(DroppedBatch {
                    op_count: remaining.len(),
                    fields: BTreeSet::new(),
                });
            }
        }
        self.current != before
    }
}

fn parse_bytes(bytes: &[u8]) -> Result<SpaceLayout, LayoutError> {
    if bytes.len() > LAYOUT_MAX_BYTES {
        return Err(LayoutError::TooLarge(bytes.len()));
    }
    let value =
        serde_json::from_slice(bytes).map_err(|why| LayoutError::Malformed(why.to_string()))?;
    let layout = parse_layout(&value)?;
    validate_layout(&layout)?;
    Ok(layout)
}

/// Every space's layout state, the clients watching them and the work they cause.
#[derive(Debug)]
pub struct Layouts {
    timing: LayoutTiming,
    spaces: BTreeMap<SpaceId, SpaceState>,
    watching: BTreeMap<ConnectionId, SpaceId>,
    effects: Vec<LayoutEffect>,
    changed: BTreeSet<SpaceId>,
    replies: Vec<LayoutReply>,
}

impl Layouts {
    pub fn new(timing: LayoutTiming) -> Layouts {
        Layouts {
            timing,
            spaces: BTreeMap::new(),
            watching: BTreeMap::new(),
            effects: Vec::new(),
            changed: BTreeSet::new(),
            replies: Vec::new(),
        }
    }

    /// Subscribes `conn` to `space`, replacing and flushing its previous watch. `known` says
    /// whether the space is one the daemon lists. The reply is queued now if the space has been
    /// read, and when the first read lands otherwise.
    pub fn watch(
        &mut self,
        conn: ConnectionId,
        space: SpaceId,
        request: u64,
        known: bool,
        now: Millis,
    ) -> Result<(), WatchError> {
        if !known {
            return Err(WatchError::UnknownSpace);
        }
        if self
            .watching
            .get(&conn)
            .is_some_and(|watched| *watched != space)
        {
            self.unwatch(conn, now);
        }
        self.watching.insert(conn, space.clone());
        match self.spaces.get_mut(&space) {
            Some(state) if state.loaded => self.replies.push(LayoutReply {
                conn,
                request,
                space,
            }),
            Some(state) => state.waiting.push((conn, request)),
            None => {
                let mut state = SpaceState::new();
                state.waiting.push((conn, request));
                self.spaces.insert(space.clone(), state);
                self.effects.push(LayoutEffect::Watch {
                    space: space.clone(),
                });
                self.effects.push(LayoutEffect::Read { space });
            }
        }
        Ok(())
    }

    /// Ends `conn`'s watch, also on disconnect, and writes the space's pending ops now.
    pub fn unwatch(&mut self, conn: ConnectionId, now: Millis) {
        let Some(space) = self.watching.remove(&conn) else {
            return;
        };
        if let Some(state) = self.spaces.get_mut(&space) {
            state.waiting.retain(|(waiting, _)| *waiting != conn);
        }
        self.flush(&space, now);
    }

    /// A change event arrived for `space`: read it again.
    pub fn apply_change(&mut self, space: &SpaceId) {
        if self.spaces.contains_key(space) {
            self.effects.push(LayoutEffect::Read {
                space: space.clone(),
            });
        }
    }

    pub fn apply_read(&mut self, space: &SpaceId, read: LayoutRead, now: Millis) {
        let Some(state) = self.spaces.get_mut(space) else {
            return;
        };
        let first = !state.loaded;
        let publish = state.apply_read(read, now, self.timing);
        if first {
            for (conn, request) in std::mem::take(&mut state.waiting) {
                self.replies.push(LayoutReply {
                    conn,
                    request,
                    space: space.clone(),
                });
            }
        }
        if publish {
            self.publish(space);
        }
    }

    /// Applies `ops` for `conn`, which must be watching `space` at revision `base`.
    pub fn mutate(
        &mut self,
        conn: ConnectionId,
        space: &SpaceId,
        request: u64,
        base: LayoutRevision,
        ops: Vec<LayoutOp>,
        now: Millis,
    ) -> Result<(), MutateError> {
        if self.watching.get(&conn) != Some(space) {
            return Err(if self.spaces.contains_key(space) {
                MutateError::NotWatched
            } else {
                MutateError::UnknownSpace
            });
        }
        let state = self
            .spaces
            .get_mut(space)
            .ok_or(MutateError::UnknownSpace)?;
        if !state.loaded || state.revision != base {
            return Err(MutateError::Stale);
        }
        if ops.is_empty() {
            return Err(MutateError::Invalid(
                "a change needs at least one op".to_string(),
            ));
        }
        let applied = apply_ops(&state.current, &state.origins, &ops)
            .map_err(|why| MutateError::Invalid(why.to_string()))?;
        state.current = applied.layout;
        state.origins = applied.origins;
        let timed = ops.into_iter().map(|op| TimedOp { op, at: now });
        match &mut state.pending {
            Some(pending) => {
                pending.ops.extend(timed);
                pending.touched.extend(applied.touched);
            }
            None => {
                state.pending = Some(Pending {
                    ops: timed.collect(),
                    touched: applied.touched,
                });
            }
        }
        state.dropped = None;
        state.arm_deadline(now, self.timing);
        self.replies.push(LayoutReply {
            conn,
            request,
            space: space.clone(),
        });
        self.publish(space);
        Ok(())
    }

    /// Writes every space whose deadline is at or before `now`.
    pub fn tick(&mut self, now: Millis) {
        let due: Vec<SpaceId> = self
            .spaces
            .iter()
            .filter(|(_, state)| state.deadline.is_some_and(|deadline| deadline <= now))
            .map(|(space, _)| space.clone())
            .collect();
        for space in due {
            self.flush(&space, now);
        }
    }

    /// Writes `space`'s pending ops now. Nothing happens while the file is invalid, while a
    /// write is in flight, or when nothing is pending.
    pub fn flush(&mut self, space: &SpaceId, now: Millis) {
        let Some(state) = self.spaces.get_mut(space) else {
            return;
        };
        state.deadline = None;
        if state.invalid || state.writing.is_some() {
            return;
        }
        let Some(pending) = &state.pending else {
            return;
        };
        let op_count = pending.ops.len();
        match encode_layout(&state.current) {
            Ok(bytes) => {
                state.writing = Some(Writing {
                    layout: state.current.clone(),
                    op_count,
                    cut_off: false,
                });
                self.effects.push(LayoutEffect::Write {
                    space: space.clone(),
                    bytes,
                    op_count,
                });
            }
            Err(why) => {
                self.apply_write(space, LayoutWritten::Failed(why.to_string()), now);
            }
        }
    }

    pub fn flush_all(&mut self, now: Millis) {
        let spaces: Vec<SpaceId> = self.spaces.keys().cloned().collect();
        for space in spaces {
            self.flush(&space, now);
        }
    }

    pub fn apply_write(&mut self, space: &SpaceId, written: LayoutWritten, now: Millis) {
        let Some(state) = self.spaces.get_mut(space) else {
            return;
        };
        if state.apply_written(written, now, self.timing) {
            self.publish(space);
        }
    }

    fn publish(&mut self, space: &SpaceId) {
        if let Some(state) = self.spaces.get_mut(space) {
            state.revision = LayoutRevision(state.revision.0 + 1);
            self.changed.insert(space.clone());
        }
    }

    pub fn take_effects(&mut self) -> Vec<LayoutEffect> {
        std::mem::take(&mut self.effects)
    }

    /// The earliest moment [`Layouts::tick`] has work, if any.
    pub fn next_deadline(&self) -> Option<Millis> {
        self.spaces
            .values()
            .filter_map(|state| state.deadline)
            .min()
    }

    /// The spaces whose revision went up since the last call, for pushing to their watchers.
    pub fn take_changed(&mut self) -> Vec<SpaceId> {
        std::mem::take(&mut self.changed).into_iter().collect()
    }

    pub fn take_replies(&mut self) -> Vec<LayoutReply> {
        std::mem::take(&mut self.replies)
    }

    /// The connections watching `space`.
    pub fn subscribers(&self, space: &SpaceId) -> Vec<ConnectionId> {
        self.watching
            .iter()
            .filter(|(_, watched)| *watched == space)
            .map(|(conn, _)| *conn)
            .collect()
    }

    /// The space's layout and state, once it has been read.
    pub fn status(&self, space: &SpaceId) -> Option<SpaceLayoutStatus> {
        self.spaces
            .get(space)
            .filter(|state| state.loaded)
            .map(SpaceState::status)
    }
}
