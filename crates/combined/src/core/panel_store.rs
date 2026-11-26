//! The panel store: view panels kept in a space's `.workspace/panels/` folders, their revisions
//! and their validation. No I/O.
//!
//! The store answers the panel API synchronously and asks its caller for work through
//! [`PanelEffect`]s: reads, saves, deletes and diagram checks. The caller reports each result
//! back through the matching `apply_*` method. Time arrives as [`Millis`] and as a plain `u64`
//! wall-clock value; the store reads no clock.
//!
//! A panel's revision comes from the same counter as every other ID ([`IdSource`]), so
//! revisions never repeat. A check result is applied only to the revision it was asked for.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use combine_protocol::{CreatedBy, PanelError, PanelState, Renderer, SkippedPanel, MAX_FRAME_LEN};

use crate::core::layout_sync::store_error_detail;
use crate::core::machine::Core;
use crate::core::panel_file::{
    clip_text, content_file, created_by_refusal, encode_view_panel, panel_reply_len,
    parse_view_panel, valid_idempotency_key, valid_placement, valid_title, KEY_RULE,
    PANEL_ERROR_TEXT_MAX, PLACEMENT_RULE, SCRATCH_FILE, SOURCE_MAX_BYTES, TITLE_RULE,
};
use crate::core::panels::{
    panel_folder_refusal, valid_panel_folder, PanelSkip, MAX_PANELS, PANELS_DIR, PANEL_FILE,
    PANEL_LIST_MAX,
};
use crate::core::spawn::PanelRead;
use crate::model::{ContentHash, Lifetime, Millis, SpaceId, ViewPanelFile};
use crate::ports::{
    DiagramCheckError, DiagramVerdict, DirListing, EntryKind, FileMark, ScratchDir, ScratchListing,
    StoreError,
};

mod scratch;

/// The most diagram checks that run at once.
pub const MAX_CHECKS_IN_FLIGHT: usize = 2;
/// How long a scratch panel lives after its last change.
pub const SCRATCH_TTL_MS: u64 = 604_800_000;
/// How many removed panel IDs a space remembers.
pub const REMOVED_IDS_MAX: usize = 1_000;
/// The most scratch folders one listing returns.
pub const SCRATCH_LIST_MAX: usize = 4_096;
/// Why a `.workspace/panels/<id>` folder is skipped while a scratch panel holds its ID.
pub const SCRATCH_ID_OPEN: &str = "a scratch panel with this ID is open; close or pin it first";

const HUNDRED_REASON: &str = "the space already has 100 panels; this folder isn't read";

/// Where the next ID comes from. Panel revisions and daemon-made panel IDs use it.
pub trait IdSource {
    fn take_id(&mut self) -> u64;
}

impl IdSource for Core {
    fn take_id(&mut self) -> u64 {
        Core::take_id(self)
    }
}

/// How long the store waits before it acts. The `check_retry_ms` values are successive delays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelTiming {
    pub debounce_ms: u64,
    pub max_wait_ms: u64,
    pub retry_ms: u64,
    pub check_retry_ms: [u64; 3],
    pub sweep_every_ms: u64,
}

impl Default for PanelTiming {
    fn default() -> PanelTiming {
        PanelTiming {
            debounce_ms: 500,
            max_wait_ms: 2_000,
            retry_ms: 5_000,
            check_retry_ms: [1_000, 5_000, 30_000],
            sweep_every_ms: 3_600_000,
        }
    }
}

/// A created panel, and whether this call made it.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateOutcome {
    pub view: PanelView,
    /// False when an idempotency key found the panel an earlier call made.
    pub created: bool,
}

/// A request to make a panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelCreate {
    pub id: Option<String>,
    pub lifetime: Lifetime,
    pub renderer: Renderer,
    pub title: Option<String>,
    pub idempotency_key: Option<String>,
    pub created_by: Option<CreatedBy>,
    pub placement: Option<String>,
    pub content: String,
}

/// A request to change a panel. `title: Some(None)` clears the title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelUpdate {
    pub content: Option<String>,
    pub title: Option<Option<String>>,
}

/// One panel as the API shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelView {
    pub space: SpaceId,
    pub id: String,
    pub lifetime: Lifetime,
    pub file: ViewPanelFile,
    pub revision: u64,
    pub state: PanelState,
    pub error: Option<PanelError>,
    pub file_error: Option<String>,
    pub dropped: Option<u64>,
    /// The latest read of the panel's files came from the fallback copy.
    pub read_only: bool,
}

/// A space's panels, the folders that offer none, and why the listing is incomplete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelList {
    pub panels: Vec<PanelView>,
    pub skipped: Vec<SkippedPanel>,
    pub error: Option<String>,
}

/// Where a panel's files are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelFolder {
    Workspace { space: SpaceId, panel: String },
    Scratch(ScratchDir),
}

/// Names one save or delete while the caller runs it. It never goes on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SaveTicket(pub u64);

/// One file of a save. `expected` is what the file must be right before it is written; `None`
/// means it is not compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileWrite {
    pub file: &'static str,
    pub bytes: Vec<u8>,
    pub expected: Option<FileMark>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveReport {
    /// The files written before the save ended, with the hashes written.
    pub written: Vec<(&'static str, ContentHash)>,
    pub end: SaveEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveEnd {
    Done,
    /// `file` was not what the save expected, and nothing further was written.
    Conflict {
        file: &'static str,
    },
    Failed {
        file: &'static str,
        error: StoreError,
    },
}

/// Work for the server to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelEffect {
    /// Start reporting changes in the space; answer nothing.
    Watch {
        space: SpaceId,
    },
    /// List `.workspace/panels`; answer with [`PanelStore::apply_listing`].
    ListWorkspace {
        space: SpaceId,
    },
    /// List the scratch folders; answer with [`PanelStore::apply_scratch_listing`].
    ListScratch,
    /// Read `file` of the folder with its bound; answer with [`PanelStore::apply_read`].
    Read {
        at: PanelFolder,
        file: &'static str,
    },
    /// Write the files in order; answer with [`PanelStore::apply_saved`].
    Save {
        ticket: SaveTicket,
        at: PanelFolder,
        files: Vec<FileWrite>,
    },
    /// Workspace: compare every file, then remove them in order, then `.DS_Store`, then the
    /// folder if it is empty. Scratch: `files` is empty and the whole folder goes. Answer with
    /// [`PanelStore::apply_deleted`].
    Delete {
        ticket: SaveTicket,
        at: PanelFolder,
        files: Vec<(&'static str, FileMark)>,
    },
    /// Check a Mermaid source; answer with [`PanelStore::apply_check`].
    Check {
        space: SpaceId,
        panel: String,
        revision: u64,
        source: String,
    },
    Log {
        text: String,
    },
}

/// What changed, for the caller to tell its clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelEvent {
    /// A panel's revision, state, error, file, lifetime, file error, read-only flag, or its
    /// removal changed.
    Changed {
        space: SpaceId,
        panel: String,
        revision: u64,
        state: PanelState,
    },
    /// The folders skipped, or the listing error, changed.
    Listing { space: SpaceId },
}

/// Why the panel API refused a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelApiError {
    UnknownSpace,
    SpaceLoading,
    UnknownPanel { id: String },
    Removed { id: String },
    ReadOnly,
    InvalidId { why: String },
    IdTaken { id: String },
    InvalidField { field: &'static str, rule: String },
    PanelLimit { count: usize, limit: usize },
    TooLarge { encoded: usize },
    NotScratch { id: String },
    ListingFailed { detail: String },
    ScratchListingFailed { detail: String },
    PanelFileInvalid { reason: String },
    Unreadable { path: String, detail: String },
    DeleteFailed { id: String, detail: String },
    Stopped,
}

impl fmt::Display for PanelApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PanelApiError::UnknownSpace => f.write_str("the host lists no such space"),
            PanelApiError::SpaceLoading => {
                f.write_str("the space's panels are still being read; try again")
            }
            PanelApiError::UnknownPanel { id } => write!(f, "the space has no panel {id}"),
            PanelApiError::Removed { id } => write!(f, "the panel {id} was removed"),
            PanelApiError::ReadOnly => {
                f.write_str("the panel's folder is read-only; its files can't be changed")
            }
            PanelApiError::InvalidId { why } => write!(f, "the panel ID isn't valid: {why}"),
            PanelApiError::IdTaken { id } => {
                write!(f, "the space already has a panel folder {id}")
            }
            PanelApiError::InvalidField { field, rule } => write!(f, "{field} {rule}"),
            PanelApiError::PanelLimit { count, limit } => {
                write!(f, "the space already has {count} panels; the limit is {limit}")
            }
            PanelApiError::TooLarge { encoded } => write!(
                f,
                "the source can't be sent in one frame: its panel reply would be {encoded} bytes; the limit is {MAX_FRAME_LEN}"
            ),
            PanelApiError::NotScratch { id } => {
                write!(f, "the panel {id} is already in .workspace/")
            }
            PanelApiError::ListingFailed { detail } => {
                write!(f, "the space's panels couldn't be listed: {detail}")
            }
            PanelApiError::ScratchListingFailed { detail } => {
                write!(f, "the scratch panels couldn't be listed: {detail}")
            }
            PanelApiError::PanelFileInvalid { reason } => write!(
                f,
                "panel.json can't be used ({reason}), so the daemon doesn't rewrite it"
            ),
            PanelApiError::Unreadable { path, detail } => write!(
                f,
                "couldn't read {path} ({detail}), so the panel isn't changed until it can be read"
            ),
            PanelApiError::DeleteFailed { id, detail } => write!(
                f,
                "couldn't delete the scratch panel {id} ({detail}), so it is kept"
            ),
            PanelApiError::Stopped => f.write_str("the server has stopped"),
        }
    }
}

impl std::error::Error for PanelApiError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FolderState {
    Reading,
    Panel,
    Empty,
    Symlink,
    NotRead,
    /// Its ID is held by a scratch panel: it is never read and never counted.
    Shadowed {
        symlink: bool,
    },
}

/// What `scratch.json` of a folder said when it was last written or read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    space: SpaceId,
    changed_at_ms: u64,
}

/// A scratch panel being loaded at start: its `panel.json`, once parsed, waits for the content.
struct ScratchLoad {
    dir: ScratchDir,
    file: Option<(ViewPanelFile, FileMark)>,
}

/// The scratch state that belongs to no one space.
#[derive(Default)]
struct ScratchGlobal {
    listed: bool,
    list_failed: Option<String>,
    pending: BTreeSet<ScratchDir>,
    stamped: BTreeMap<ScratchDir, Stamp>,
    /// Closes whose delete failed, until the server answers them.
    failures: Vec<DeleteFailure>,
}

impl ScratchGlobal {
    /// The scratch folders were listed and every `scratch.json` was answered.
    fn done(&self) -> bool {
        self.listed && self.list_failed.is_none() && self.pending.is_empty()
    }
}

struct UnreadInfo {
    file: &'static str,
    detail: String,
}

struct Record {
    lifetime: Lifetime,
    file: ViewPanelFile,
    source: String,
    /// The source is kept for saving but never served.
    hidden: bool,
    core_error: Option<PanelError>,
    revision: u64,
    state: PanelState,
    error: Option<PanelError>,
    file_error: Option<String>,
    save_failed: bool,
    dropped: Option<u64>,
    known: (FileMark, FileMark),
    invalid: Option<String>,
    unread: Option<UnreadInfo>,
    read_only: bool,
    pending: BTreeSet<&'static str>,
    busy: Option<SaveTicket>,
    save_retry_at: Option<Millis>,
    conflict_read: bool,
    delete_wanted: bool,
    wants_check: bool,
    retries_done: usize,
    check_retry_at: Option<Millis>,
    pinning: bool,
    /// A pin was accepted while the scratch folder might lack some of the panel: the rollback
    /// of that pin saves all of it again.
    scratch_unsaved: bool,
    /// The scratch Save error the pin accept set aside, shown again by the rollback.
    scratch_failure: Option<String>,
    before_remove: Option<(PanelState, Option<PanelError>)>,
}

impl Record {
    fn new(file: ViewPanelFile, source: String, revision: u64) -> Record {
        Record {
            lifetime: Lifetime::Persistent,
            file,
            source,
            hidden: false,
            core_error: None,
            revision,
            state: PanelState::Validating,
            error: None,
            file_error: None,
            save_failed: false,
            dropped: None,
            known: (FileMark::Missing, FileMark::Missing),
            invalid: None,
            unread: None,
            read_only: false,
            pending: BTreeSet::new(),
            busy: None,
            save_retry_at: None,
            conflict_read: false,
            delete_wanted: false,
            wants_check: false,
            retries_done: 0,
            check_retry_at: None,
            pinning: false,
            scratch_unsaved: false,
            scratch_failure: None,
            before_remove: None,
        }
    }

    fn live(&self) -> bool {
        self.state != PanelState::Removed
    }

    /// A create's first save, or a retry of it.
    fn is_create_save(&self) -> bool {
        self.lifetime == Lifetime::Persistent && !self.pinning && self.known.0 == FileMark::Missing
    }
}

struct Parsed {
    file: ViewPanelFile,
    json_mark: FileMark,
    json_fallback: bool,
}

struct RemovedId {
    id: String,
    lifetime: Lifetime,
    file: ViewPanelFile,
    revision: u64,
}

enum OpKind {
    Save,
    Remove,
    ScratchSave,
    /// A scratch folder goes. `swept` is the stamp a sweep acted on.
    ScratchGone {
        dir: ScratchDir,
        swept: Option<Stamp>,
        /// A close or remove keeps what it dropped, to put it back when the delete fails.
        restore: Option<Box<Restore>>,
    },
    /// The undo of a pin that failed.
    Cleanup,
}

/// A scratch panel a close dropped: its record and the stamp of its folder.
struct Restore {
    record: Record,
    stamp: Option<Stamp>,
}

/// A close or remove of a scratch panel whose folder couldn't be deleted.
pub struct DeleteFailure {
    pub space: SpaceId,
    pub panel: String,
    pub detail: String,
}

struct SaveOp {
    space: SpaceId,
    panel: String,
    kind: OpKind,
}

#[derive(Default)]
struct Out {
    effects: Vec<PanelEffect>,
    events: Vec<PanelEvent>,
    queue: VecDeque<(SpaceId, String)>,
    queued: BTreeSet<(SpaceId, String)>,
    in_flight: BTreeMap<(SpaceId, String), u64>,
    saves: BTreeMap<SaveTicket, SaveOp>,
    next_ticket: u64,
    scratch: ScratchGlobal,
}

impl Out {
    fn enqueue(&mut self, key: (SpaceId, String)) {
        if self.queued.insert(key.clone()) {
            self.queue.push_back(key);
        }
    }

    fn ticket(&mut self, space: &SpaceId, panel: &str, kind: OpKind) -> SaveTicket {
        self.next_ticket += 1;
        let ticket = SaveTicket(self.next_ticket);
        self.saves.insert(
            ticket,
            SaveOp {
                space: space.clone(),
                panel: panel.to_owned(),
                kind,
            },
        );
        ticket
    }
}

/// What the store read of one file.
enum Got {
    Missing,
    Bytes { bytes: Vec<u8>, hash: ContentHash },
    TooLarge { size: u64 },
    Failed(String),
}

fn split_read(read: PanelRead) -> (Got, bool) {
    let fallback = read.is_fallback();
    let got = match read {
        PanelRead::Missing => Got::Missing,
        PanelRead::Found { bytes, hash } | PanelRead::FallbackFound { bytes, hash } => {
            Got::Bytes { bytes, hash }
        }
        PanelRead::TooLarge { size } | PanelRead::FallbackTooLarge { size } => {
            Got::TooLarge { size }
        }
        PanelRead::Failed(detail) => Got::Failed(detail),
    };
    (got, fallback)
}

fn clip_error(mut error: PanelError) -> PanelError {
    error.message = clip_text(&error.message, PANEL_ERROR_TEXT_MAX).to_owned();
    error.snippet = error
        .snippet
        .map(|snippet| clip_text(&snippet, PANEL_ERROR_TEXT_MAX).to_owned());
    error
}

fn plain_error(code: &str, message: String) -> PanelError {
    clip_error(PanelError {
        code: code.to_owned(),
        message,
        line: None,
        column: None,
        snippet: None,
    })
}

fn too_large_source(size: u64) -> PanelError {
    plain_error(
        "too_large",
        format!("the source is {size} bytes; the limit is {SOURCE_MAX_BYTES}"),
    )
}

fn too_large_frame(encoded: usize) -> PanelError {
    plain_error(
        "too_large",
        format!(
            "the source can't be sent in one frame: its panel reply would be {encoded} bytes; the limit is {MAX_FRAME_LEN}"
        ),
    )
}

/// What the core checks make of a source read from disk: the source to serve, and why it
/// failed, if it did.
fn outside_source(
    space: &SpaceId,
    id: &str,
    renderer: Renderer,
    bytes: Vec<u8>,
) -> (String, Option<PanelError>) {
    if bytes.len() > SOURCE_MAX_BYTES {
        return (String::new(), Some(too_large_source(bytes.len() as u64)));
    }
    match String::from_utf8(bytes) {
        Err(_) => (
            String::new(),
            Some(plain_error(
                "invalid_utf8",
                "the content is not valid UTF-8".to_owned(),
            )),
        ),
        Ok(text) => {
            let encoded = panel_reply_len(space, id, renderer, &text);
            if encoded > MAX_FRAME_LEN as usize {
                (String::new(), Some(too_large_frame(encoded)))
            } else {
                (text, None)
            }
        }
    }
}

fn file_path(id: &str, file: &str) -> String {
    format!(".workspace/{PANELS_DIR}/{id}/{file}")
}

type Debounce = BTreeMap<String, (Millis, Millis)>;

fn bump(map: &mut Debounce, id: &str, now: Millis) {
    let entry = map.entry(id.to_owned()).or_insert((now, now));
    entry.1 = now;
}

struct SpaceState {
    id: SpaceId,
    timing: PanelTiming,
    listed: bool,
    loaded: bool,
    listing_failed: Option<String>,
    list_note: Option<String>,
    over_hundred: bool,
    relist_at: Option<Millis>,
    relist_debounce: Option<(Millis, Millis)>,
    folders: BTreeMap<String, FolderState>,
    records: BTreeMap<String, Record>,
    skipped: BTreeMap<String, String>,
    reading: BTreeMap<String, Parsed>,
    debounce: Debounce,
    reread_at: BTreeMap<String, Millis>,
    deferred: BTreeSet<String>,
    /// How many `Read`s of each panel were emitted and not yet answered.
    reads_out: BTreeMap<String, u32>,
    /// How many of those were emitted before a Save or Delete began; their results are dropped.
    stale_reads: BTreeMap<String, u32>,
    removed: VecDeque<RemovedId>,
    /// Scratch folders of this space waiting for its first listing, or for workspace reads that
    /// may still free a slot. `loaded` waits until this is empty.
    waiting_scratch: Vec<ScratchDir>,
    /// Scratch folders of this space whose files are being read.
    scratch_reading: BTreeMap<String, ScratchLoad>,
}

type Signature = (BTreeMap<String, String>, Option<String>);

impl SpaceState {
    fn new(id: SpaceId, timing: PanelTiming) -> SpaceState {
        SpaceState {
            id,
            timing,
            listed: false,
            loaded: false,
            listing_failed: None,
            list_note: None,
            over_hundred: false,
            relist_at: None,
            relist_debounce: None,
            folders: BTreeMap::new(),
            records: BTreeMap::new(),
            skipped: BTreeMap::new(),
            reading: BTreeMap::new(),
            debounce: BTreeMap::new(),
            reread_at: BTreeMap::new(),
            deferred: BTreeSet::new(),
            reads_out: BTreeMap::new(),
            stale_reads: BTreeMap::new(),
            removed: VecDeque::new(),
            waiting_scratch: Vec::new(),
            scratch_reading: BTreeMap::new(),
        }
    }

    fn list_error(&self) -> Option<String> {
        match &self.listing_failed {
            Some(detail) => Some(format!("Couldn't list .workspace/{PANELS_DIR} ({detail})")),
            None => self.list_note.clone(),
        }
    }

    fn signature(&self) -> Signature {
        (self.skipped.clone(), self.list_error())
    }

    fn debounce_due(&self, range: (Millis, Millis)) -> Millis {
        let by_quiet = range.1.saturating_add(Millis(self.timing.debounce_ms));
        let by_wait = range.0.saturating_add(Millis(self.timing.max_wait_ms));
        by_quiet.min(by_wait)
    }

    fn update_loaded(&mut self, scratch: &ScratchGlobal) {
        let scratch_settled = scratch.list_failed.is_some()
            || (scratch.done()
                && self.waiting_scratch.is_empty()
                && self.scratch_reading.is_empty());
        if !self.loaded
            && self.listed
            && !self.folders.values().any(|f| *f == FolderState::Reading)
            && self.reading.is_empty()
            && scratch_settled
        {
            self.loaded = true;
        }
    }

    /// The panels the space holds or has promised to: folders holding a panel or being read
    /// (an empty folder only while a read of it is out), symlinked folders, live Scratch records and Scratch folders being read. Every admission
    /// path checks this one budget, so a read that is asked for but not yet answered already
    /// takes its place.
    fn held(&self) -> usize {
        self.held_with(true)
    }

    /// `held`, with or without the workspace reads that are out. Without them it is what the
    /// space keeps whichever way those reads end: a read that finds no `panel.json` frees its slot.
    fn held_with(&self, reads: bool) -> usize {
        let folders = self
            .folders
            .iter()
            .filter(|(id, state)| match state {
                FolderState::Panel | FolderState::Symlink => true,
                FolderState::Reading => reads,
                FolderState::Empty => reads && self.reads_out.contains_key(id.as_str()),
                FolderState::NotRead | FolderState::Shadowed { .. } => false,
            })
            .count();
        let scratch = self
            .records
            .values()
            .filter(|rec| rec.lifetime == Lifetime::Scratch && rec.live())
            .count();
        folders + scratch + self.scratch_reading.len()
    }

    /// `held`, at least 100 when the listing had more.
    fn count(&self) -> usize {
        self.count_with(true)
    }

    fn count_with(&self, reads: bool) -> usize {
        let count = self.held_with(reads);
        if self.over_hundred {
            count.max(MAX_PANELS)
        } else {
            count
        }
    }

    fn id_taken(&self, id: &str) -> bool {
        let folder = matches!(
            self.folders.get(id),
            Some(
                FolderState::Panel
                    | FolderState::Symlink
                    | FolderState::Reading
                    | FolderState::NotRead
                    | FolderState::Shadowed { .. }
            )
        );
        folder || self.records.contains_key(id) || self.reading.contains_key(id)
    }

    fn view_of(&self, id: &str, rec: &Record) -> PanelView {
        PanelView {
            space: self.id.clone(),
            id: id.to_owned(),
            lifetime: rec.lifetime,
            file: rec.file.clone(),
            revision: rec.revision,
            state: rec.state,
            error: if rec.live() { rec.error.clone() } else { None },
            file_error: rec.file_error.clone(),
            dropped: rec.dropped,
            read_only: rec.read_only,
        }
    }

    fn removed_view(&self, entry: &RemovedId) -> PanelView {
        PanelView {
            space: self.id.clone(),
            id: entry.id.clone(),
            lifetime: entry.lifetime,
            file: entry.file.clone(),
            revision: entry.revision,
            state: PanelState::Removed,
            error: None,
            file_error: None,
            dropped: None,
            read_only: false,
        }
    }

    fn remembered(&self, id: &str) -> Option<&RemovedId> {
        self.removed.iter().find(|entry| entry.id == id)
    }

    fn remember(&mut self, id: &str, lifetime: Lifetime, file: ViewPanelFile, revision: u64) {
        self.forget(id);
        self.removed.push_back(RemovedId {
            id: id.to_owned(),
            lifetime,
            file,
            revision,
        });
        while self.removed.len() > REMOVED_IDS_MAX {
            self.removed.pop_front();
        }
    }

    fn forget(&mut self, id: &str) {
        self.removed.retain(|entry| entry.id != id);
    }

    fn changed(&self, out: &mut Out, id: &str) {
        if let Some(rec) = self.records.get(id) {
            out.events.push(PanelEvent::Changed {
                space: self.id.clone(),
                panel: id.to_owned(),
                revision: rec.revision,
                state: rec.state,
            });
        }
    }

    fn emit_read(&mut self, out: &mut Out, id: &str, file: &'static str) {
        *self.reads_out.entry(id.to_owned()).or_insert(0) += 1;
        out.effects.push(PanelEffect::Read {
            at: PanelFolder::Workspace {
                space: self.id.clone(),
                panel: id.to_owned(),
            },
            file,
        });
    }

    fn log(&self, out: &mut Out, text: String) {
        out.effects.push(PanelEffect::Log { text });
    }

    /// The gate for a call about `id` when the space holds no such record.
    fn missing(
        &self,
        scratch: &ScratchGlobal,
        id: &str,
        answer: Missing,
    ) -> Result<PanelView, PanelApiError> {
        if let Some(detail) = &self.listing_failed {
            return Err(PanelApiError::ListingFailed {
                detail: detail.clone(),
            });
        }
        if !self.loaded {
            return Err(PanelApiError::SpaceLoading);
        }
        if let Some(entry) = self.remembered(id) {
            return match answer {
                Missing::Get | Missing::Close => Ok(self.removed_view(entry)),
                Missing::Refuse => Err(PanelApiError::Removed { id: id.to_owned() }),
            };
        }
        if let Some(detail) = &scratch.list_failed {
            return Err(scratch_list_refusal(detail));
        }
        if self.scratch_loading(scratch, Some(id)) {
            return Err(PanelApiError::SpaceLoading);
        }
        Err(PanelApiError::UnknownPanel { id: id.to_owned() })
    }

    // ---- validation

    /// Starts validating the record's current source. `core_error` is what the core's own
    /// checks found; Markdown ends validated at once and Mermaid is queued.
    fn begin_validation(&mut self, out: &mut Out, id: &str, core_error: Option<PanelError>) {
        let renderer = {
            let Some(rec) = self.records.get_mut(id) else {
                return;
            };
            rec.core_error = core_error.clone();
            rec.state = PanelState::Validating;
            rec.error = None;
            rec.wants_check = false;
            rec.retries_done = 0;
            rec.check_retry_at = None;
            rec.file.renderer
        };
        self.changed(out, id);
        if let Some(rec) = self.records.get_mut(id) {
            match core_error {
                Some(error) => {
                    rec.state = PanelState::Failed;
                    rec.error = Some(error);
                }
                None => match renderer {
                    Renderer::Markdown => rec.state = PanelState::Validated,
                    Renderer::Mermaid => {
                        rec.wants_check = true;
                        out.enqueue((self.id.clone(), id.to_owned()));
                        return;
                    }
                },
            }
        }
        self.changed(out, id);
    }

    // ---- create, update, close, remove, pin

    fn create(
        &mut self,
        out: &mut Out,
        create: PanelCreate,
        ids: &mut dyn IdSource,
        wall_ms: u64,
    ) -> Result<(PanelView, bool), PanelApiError> {
        if let Some(detail) = &self.listing_failed {
            return Err(PanelApiError::ListingFailed {
                detail: detail.clone(),
            });
        }
        if !self.loaded {
            return Err(PanelApiError::SpaceLoading);
        }
        if create.lifetime == Lifetime::Scratch {
            if let Some(detail) = &out.scratch.list_failed {
                return Err(scratch_list_refusal(detail));
            }
        }
        if self.scratch_loading(&out.scratch, None) {
            return Err(PanelApiError::SpaceLoading);
        }
        if let Some(title) = &create.title {
            if !valid_title(title) {
                return Err(invalid_field("title", TITLE_RULE));
            }
        }
        if let Some(key) = &create.idempotency_key {
            if !valid_idempotency_key(key) {
                return Err(invalid_field("idempotency_key", KEY_RULE));
            }
        }
        if let Some(created_by) = &create.created_by {
            if let Some((field, rule)) = created_by_refusal(created_by) {
                return Err(invalid_field(field, rule));
            }
        }
        if let Some(tile) = &create.placement {
            if !valid_placement(tile) {
                return Err(invalid_field("placement", PLACEMENT_RULE));
            }
        }
        if let Some(key) = &create.idempotency_key {
            let same = self.records.iter().find(|(_, rec)| {
                rec.live() && rec.file.idempotency_key.as_deref() == Some(key.as_str())
            });
            if let Some((id, rec)) = same {
                return Ok((self.view_of(id, rec), false));
            }
        }
        let id = match create.id {
            Some(id) => {
                if let Some(why) = panel_folder_refusal(&id) {
                    return Err(PanelApiError::InvalidId { why });
                }
                if self.id_taken(&id) {
                    return Err(PanelApiError::IdTaken { id });
                }
                id
            }
            None => loop {
                let id = format!("panel-{}", ids.take_id());
                if !self.id_taken(&id) && valid_panel_folder(&id) {
                    break id;
                }
            },
        };
        let count = self.count();
        if count >= MAX_PANELS {
            return Err(PanelApiError::PanelLimit {
                count,
                limit: MAX_PANELS,
            });
        }
        let encoded = panel_reply_len(&self.id, &id, create.renderer, &create.content);
        if encoded > MAX_FRAME_LEN as usize {
            return Err(PanelApiError::TooLarge { encoded });
        }

        let revision = ids.take_id();
        let renderer = create.renderer;
        let file = ViewPanelFile {
            renderer,
            title: create.title,
            idempotency_key: create.idempotency_key,
            created_by: create.created_by,
            placement: create.placement,
        };
        let core_error = (create.content.len() > SOURCE_MAX_BYTES)
            .then(|| too_large_source(create.content.len() as u64));
        let mut rec = Record::new(file, create.content, revision);
        rec.hidden = core_error.is_some();
        rec.pending.insert(content_file(renderer));
        rec.pending.insert(PANEL_FILE);
        self.forget(&id);
        self.skipped.remove(&id);
        match create.lifetime {
            Lifetime::Persistent => {
                self.folders.insert(id.clone(), FolderState::Panel);
            }
            Lifetime::Scratch => {
                rec.lifetime = Lifetime::Scratch;
                rec.pending.insert(SCRATCH_FILE);
                let stamp = Stamp {
                    space: self.id.clone(),
                    changed_at_ms: wall_ms,
                };
                out.scratch.stamped.insert(self.scratch_dir(&id), stamp);
            }
        }
        self.records.insert(id.clone(), rec);
        self.service(out, &id);
        self.begin_validation(out, &id, core_error);
        self.view_now(&id).map(|view| (view, true))
    }

    fn view_now(&self, id: &str) -> Result<PanelView, PanelApiError> {
        match self.records.get(id) {
            Some(rec) => Ok(self.view_of(id, rec)),
            None => Err(PanelApiError::UnknownPanel { id: id.to_owned() }),
        }
    }

    fn update(
        &mut self,
        out: &mut Out,
        id: &str,
        update: PanelUpdate,
        ids: &mut dyn IdSource,
        wall_ms: u64,
    ) -> Result<PanelView, PanelApiError> {
        let Some(rec) = self.records.get(id) else {
            return self.missing(&out.scratch, id, Missing::Refuse);
        };
        if !rec.live() {
            return Err(PanelApiError::Removed { id: id.to_owned() });
        }
        if rec.read_only {
            return Err(PanelApiError::ReadOnly);
        }
        if let Some(unread) = &rec.unread {
            return Err(unread_refusal(id, unread));
        }
        if let Some(Some(title)) = &update.title {
            if !valid_title(title) {
                return Err(invalid_field("title", TITLE_RULE));
            }
        }
        if update.content.is_none() && update.title.is_none() {
            return Err(invalid_field(
                "update",
                "must change the content or the title",
            ));
        }
        if update.title.is_some() {
            if let Some(reason) = &rec.invalid {
                return Err(PanelApiError::PanelFileInvalid {
                    reason: reason.clone(),
                });
            }
        }
        if let Some(content) = &update.content {
            let encoded = panel_reply_len(&self.id, id, rec.file.renderer, content);
            if encoded > MAX_FRAME_LEN as usize {
                return Err(PanelApiError::TooLarge { encoded });
            }
        }

        let revision = ids.take_id();
        let mut core_error = None;
        let dir = self.scratch_dir(id);
        if let Some(rec) = self.records.get_mut(id) {
            if rec.lifetime == Lifetime::Scratch || rec.pinning {
                if let Some(stamp) = out.scratch.stamped.get_mut(&dir) {
                    stamp.changed_at_ms = wall_ms;
                }
                if rec.lifetime == Lifetime::Scratch {
                    rec.pending.insert(SCRATCH_FILE);
                }
            }
            rec.revision = revision;
            rec.dropped = None;
            rec.save_retry_at = None;
            core_error = rec.core_error.clone();
            if let Some(content) = update.content {
                core_error = (content.len() > SOURCE_MAX_BYTES)
                    .then(|| too_large_source(content.len() as u64));
                rec.hidden = core_error.is_some();
                rec.source = content;
                rec.pending.insert(content_file(rec.file.renderer));
            }
            if let Some(title) = update.title {
                if rec.file.title != title {
                    rec.file.title = title;
                    rec.pending.insert(PANEL_FILE);
                }
            }
        }
        self.service(out, id);
        self.begin_validation(out, id, core_error);
        self.view_now(id)
    }

    fn close(&mut self, out: &mut Out, id: &str) -> Result<PanelView, PanelApiError> {
        let Some(rec) = self.records.get_mut(id) else {
            return self.missing(&out.scratch, id, Missing::Close);
        };
        if !rec.live() {
            return self.view_now(id);
        }
        if rec.lifetime == Lifetime::Scratch {
            return self.end_scratch(out, id);
        }
        rec.state = PanelState::Removed;
        rec.pending.clear();
        rec.save_retry_at = None;
        rec.wants_check = false;
        rec.check_retry_at = None;
        self.changed(out, id);
        self.view_now(id)
    }

    fn remove(&mut self, out: &mut Out, id: &str) -> Result<PanelView, PanelApiError> {
        let Some(rec) = self.records.get(id) else {
            return self.missing(&out.scratch, id, Missing::Close);
        };
        if !rec.live() {
            return self.view_now(id);
        }
        if rec.read_only {
            return Err(PanelApiError::ReadOnly);
        }
        if let Some(unread) = &rec.unread {
            return Err(unread_refusal(id, unread));
        }
        if rec.lifetime == Lifetime::Scratch {
            return self.end_scratch(out, id);
        }
        let (file, revision) = (rec.file.clone(), rec.revision);
        if let Some(rec) = self.records.get_mut(id) {
            rec.before_remove = Some((rec.state, rec.error.take()));
            rec.state = PanelState::Removed;
            rec.pending.clear();
            rec.save_retry_at = None;
            rec.wants_check = false;
            rec.check_retry_at = None;
            rec.delete_wanted = true;
        }
        self.remember(id, Lifetime::Persistent, file, revision);
        self.changed(out, id);
        self.service(out, id);
        self.view_now(id)
    }

    // ---- saving

    /// Starts the next Delete or Save of a panel, if it may run now.
    fn service(&mut self, out: &mut Out, id: &str) {
        let Some(rec) = self.records.get_mut(id) else {
            return;
        };
        if rec.busy.is_some() || rec.unread.is_some() || rec.read_only {
            return;
        }
        if rec.lifetime == Lifetime::Scratch {
            self.service_scratch(out, id);
            return;
        }
        let at = PanelFolder::Workspace {
            space: self.id.clone(),
            panel: id.to_owned(),
        };
        if rec.delete_wanted {
            rec.delete_wanted = false;
            let ticket = out.ticket(&self.id, id, OpKind::Remove);
            rec.busy = Some(ticket);
            out.effects.push(PanelEffect::Delete {
                ticket,
                at,
                files: vec![
                    (PANEL_FILE, rec.known.0),
                    (content_file(rec.file.renderer), rec.known.1),
                ],
            });
            self.abandon_reads(id);
            return;
        }
        if !rec.live() {
            return;
        }
        if rec.invalid.is_some() {
            rec.pending.remove(PANEL_FILE);
        }
        if rec.pending.is_empty() {
            return;
        }
        let content = content_file(rec.file.renderer);
        let create = rec.is_create_save();
        let mut files = Vec::new();
        if rec.pending.contains(content) {
            files.push(FileWrite {
                file: content,
                bytes: rec.source.as_bytes().to_vec(),
                expected: if create { None } else { Some(rec.known.1) },
            });
        }
        if rec.pending.contains(PANEL_FILE) {
            files.push(FileWrite {
                file: PANEL_FILE,
                bytes: encode_view_panel(&rec.file),
                expected: Some(rec.known.0),
            });
        }
        rec.save_retry_at = None;
        let ticket = out.ticket(&self.id, id, OpKind::Save);
        rec.busy = Some(ticket);
        out.effects.push(PanelEffect::Save { ticket, at, files });
        self.abandon_reads(id);
    }

    /// A Save or Delete of the panel starts: a half-read pair is dropped, the results of the
    /// reads already asked for are never applied, and the folder is read again once the report
    /// is applied.
    fn abandon_reads(&mut self, id: &str) {
        let half = self.reading.remove(id).is_some();
        let out = self.reads_out.get(id).copied().unwrap_or(0);
        if out > 0 {
            self.stale_reads.insert(id.to_owned(), out);
        }
        if half || out > 0 {
            self.deferred.insert(id.to_owned());
        }
    }

    /// Reads `panel.json` of a folder the store holds a record for, or defers the read until
    /// the record's save or delete ends.
    fn request_read(&mut self, out: &mut Out, id: &str) {
        if let Some(rec) = self.records.get(id) {
            if rec.lifetime == Lifetime::Persistent {
                if rec.busy.is_some() || rec.delete_wanted {
                    self.deferred.insert(id.to_owned());
                } else {
                    self.emit_read(out, id, PANEL_FILE);
                }
                return;
            }
        }
        if self.scratch_held(id) {
            let symlink = self.shadow_symlink(id);
            self.shadow(id, symlink);
            return;
        }
        match self.folders.get(id).copied() {
            Some(FolderState::Symlink | FolderState::Reading | FolderState::Shadowed { .. }) => {}
            Some(FolderState::Panel) => self.emit_read(out, id, PANEL_FILE),
            Some(FolderState::Empty) if self.count() < MAX_PANELS => {
                self.emit_read(out, id, PANEL_FILE);
            }
            Some(FolderState::Empty | FolderState::NotRead) | None => self.first_sight(out, id),
        }
    }

    fn first_sight(&mut self, out: &mut Out, id: &str) {
        self.admit_folder(out, id, self.count());
    }

    /// A folder the store doesn't read yet becomes `Reading` while the space holds fewer than
    /// 100 panels, counting the reads already asked for. Otherwise it gets the 100-folder reason.
    fn admit_folder(&mut self, out: &mut Out, id: &str, held: usize) {
        if held < MAX_PANELS {
            self.folders.insert(id.to_owned(), FolderState::Reading);
            self.skipped.remove(id);
            self.emit_read(out, id, PANEL_FILE);
        } else {
            self.folders.insert(id.to_owned(), FolderState::NotRead);
            self.skipped
                .insert(id.to_owned(), HUNDRED_REASON.to_owned());
        }
    }

    fn release_deferred(&mut self, out: &mut Out, id: &str) {
        if self.deferred.remove(id) {
            self.request_read(out, id);
        }
    }

    fn apply_report(
        &mut self,
        out: &mut Out,
        id: &str,
        ticket: SaveTicket,
        op: OpKind,
        report: SaveReport,
        now: Millis,
    ) {
        match op {
            OpKind::Save => self.saved(out, id, report, now),
            OpKind::Remove => self.deleted(out, id, report),
            OpKind::ScratchSave => self.scratch_saved(out, id, ticket, report, now),
            OpKind::Cleanup => self.cleanup_ended(id, report, out),
            OpKind::ScratchGone { .. } => {}
        }
    }

    fn saved(&mut self, out: &mut Out, id: &str, report: SaveReport, now: Millis) {
        let Some(rec) = self.records.get_mut(id) else {
            return;
        };
        rec.busy = None;
        for (file, hash) in &report.written {
            if *file == PANEL_FILE {
                rec.known.0 = FileMark::Hash(*hash);
            } else {
                rec.known.1 = FileMark::Hash(*hash);
            }
            rec.pending.remove(file);
        }
        if rec.pinning {
            self.pin_ended(out, id, report, now);
            return;
        }
        let create_conflict = matches!(&report.end, SaveEnd::Conflict { file } if *file == PANEL_FILE)
            && rec.is_create_save();
        match report.end {
            SaveEnd::Done => {
                if rec.save_failed {
                    rec.save_failed = false;
                    rec.file_error = None;
                    self.changed(out, id);
                }
                self.service(out, id);
                self.release_deferred(out, id);
            }
            SaveEnd::Conflict { .. } if create_conflict => {
                self.create_gives_way(out, id);
            }
            SaveEnd::Conflict { .. } if rec.delete_wanted => {
                self.service(out, id);
                self.release_deferred(out, id);
            }
            SaveEnd::Conflict { .. } => {
                rec.conflict_read = true;
                self.deferred.remove(id);
                self.emit_read(out, id, PANEL_FILE);
            }
            SaveEnd::Failed { file, error } => {
                let text = match error {
                    StoreError::ReadOnly(detail) => format!(
                        "Can't write .workspace/ ({detail}). The change isn't saved, and is lost if the daemon restarts."
                    ),
                    other => format!(
                        "Couldn't save {} ({}); retrying.",
                        file_path(id, file),
                        store_error_detail(other)
                    ),
                };
                rec.file_error = Some(text);
                rec.save_failed = true;
                rec.save_retry_at = Some(now.saturating_add(Millis(self.timing.retry_ms)));
                self.changed(out, id);
                self.release_deferred(out, id);
            }
        }
    }

    /// A create found a `panel.json`: the record is dropped as an outside removal drops it, and
    /// the folder is read as found.
    fn create_gives_way(&mut self, out: &mut Out, id: &str) {
        let Some(rec) = self.records.remove(id) else {
            return;
        };
        self.remember(id, Lifetime::Persistent, rec.file.clone(), rec.revision);
        out.events.push(PanelEvent::Changed {
            space: self.id.clone(),
            panel: id.to_owned(),
            revision: rec.revision,
            state: PanelState::Removed,
        });
        self.log(
            out,
            format!(
                "the panel {id} of {} isn't created: .workspace/{PANELS_DIR}/{id}/{PANEL_FILE} changed while it was written; the folder is read instead",
                self.id.0
            ),
        );
        self.debounce.remove(id);
        self.reread_at.remove(id);
        self.deferred.remove(id);
        self.reading.remove(id);
        self.skipped.remove(id);
        self.folders.remove(id);
        self.first_sight(out, id);
    }

    fn deleted(&mut self, out: &mut Out, id: &str, report: SaveReport) {
        let Some(rec) = self.records.get_mut(id) else {
            return;
        };
        rec.busy = None;
        match report.end {
            SaveEnd::Done => {
                self.records.remove(id);
                self.debounce.remove(id);
                self.reread_at.remove(id);
                self.deferred.remove(id);
                self.folders.insert(id.to_owned(), FolderState::Empty);
                self.skipped.remove(id);
                out.effects.push(PanelEffect::ListWorkspace {
                    space: self.id.clone(),
                });
            }
            SaveEnd::Conflict { .. } => {
                if let Some((state, error)) = rec.before_remove.take() {
                    rec.state = state;
                    rec.error = error;
                }
                if rec.state == PanelState::Validating {
                    let key = (self.id.clone(), id.to_owned());
                    if out.in_flight.get(&key) != Some(&rec.revision) {
                        rec.wants_check = true;
                        out.enqueue(key);
                    }
                }
                self.forget(id);
                self.changed(out, id);
                self.deferred.remove(id);
                self.emit_read(out, id, PANEL_FILE);
            }
            SaveEnd::Failed { file, error } => {
                let detail = store_error_detail(error);
                let text = format!(
                    "couldn't remove {} of {} ({detail})",
                    file_path(id, file),
                    self.id.0
                );
                self.log(out, text);
                self.release_deferred(out, id);
            }
        }
    }

    // ---- reads

    fn on_read(
        &mut self,
        out: &mut Out,
        id: &str,
        file: &str,
        read: PanelRead,
        now: Millis,
        ids: &mut dyn IdSource,
    ) {
        let stale = self.stale_result(id);
        if self.scratch_held(id) {
            self.drop_held_read(id);
            return;
        }
        if stale {
            return;
        }
        let (got, fallback) = split_read(read);
        if file == PANEL_FILE {
            self.on_json(out, id, got, fallback, now);
        } else {
            self.on_content(out, id, file, got, fallback, now, ids);
        }
    }

    /// Counts a result in and tells whether it must be dropped: it was asked for before a Save
    /// or Delete began, or the panel is busy or has a remove pending.
    fn stale_result(&mut self, id: &str) -> bool {
        if let Some(count) = self.reads_out.get_mut(id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.reads_out.remove(id);
            }
        }
        let mut stale = false;
        if let Some(count) = self.stale_reads.get_mut(id) {
            *count -= 1;
            stale = true;
            if *count == 0 {
                self.stale_reads.remove(id);
            }
        }
        stale
            || self.records.get(id).is_some_and(|rec| {
                rec.lifetime == Lifetime::Persistent && (rec.busy.is_some() || rec.delete_wanted)
            })
    }

    fn on_json(&mut self, out: &mut Out, id: &str, got: Got, fallback: bool, now: Millis) {
        match got {
            Got::Missing => self.json_missing(out, id),
            Got::Failed(detail) => self.read_failed(out, id, PANEL_FILE, detail, now),
            Got::TooLarge { size } => {
                self.json_unusable(out, id, FileMark::TooLarge, PanelSkip::TooLarge { size });
            }
            Got::Bytes { bytes, hash } => match parse_view_panel(id, &bytes) {
                Ok(file) => {
                    self.reading.insert(
                        id.to_owned(),
                        Parsed {
                            json_mark: FileMark::Hash(hash),
                            json_fallback: fallback,
                            file: file.clone(),
                        },
                    );
                    if !self.records.contains_key(id) {
                        self.folders.insert(id.to_owned(), FolderState::Reading);
                        self.skipped.remove(id);
                    }
                    self.emit_read(out, id, content_file(file.renderer));
                }
                Err(skip) => self.json_unusable(out, id, FileMark::Hash(hash), skip),
            },
        }
    }

    fn json_missing(&mut self, out: &mut Out, id: &str) {
        self.reading.remove(id);
        self.debounce.remove(id);
        self.reread_at.remove(id);
        self.deferred.remove(id);
        self.skipped.remove(id);
        match self.records.remove(id) {
            Some(rec) => {
                let live = rec.live();
                self.remember(id, rec.lifetime, rec.file, rec.revision);
                if live {
                    out.events.push(PanelEvent::Changed {
                        space: self.id.clone(),
                        panel: id.to_owned(),
                        revision: rec.revision,
                        state: PanelState::Removed,
                    });
                }
                self.folders.insert(id.to_owned(), FolderState::Empty);
                out.effects.push(PanelEffect::ListWorkspace {
                    space: self.id.clone(),
                });
            }
            None => {
                self.folders.insert(id.to_owned(), FolderState::Empty);
            }
        }
    }

    /// `panel.json` can't be used: invalid, or too large.
    fn json_unusable(&mut self, out: &mut Out, id: &str, mark: FileMark, skip: PanelSkip) {
        self.reading.remove(id);
        let reason = skip.reason();
        match self.records.get_mut(id) {
            Some(rec) => {
                let file_error =
                    format!("panel.json can't be used: {reason}. Showing the last good spec.");
                let unchanged = rec.invalid.as_deref() == Some(reason.as_str())
                    && rec.known.0 == mark
                    && rec.unread.is_none();
                rec.invalid = Some(reason);
                rec.file_error = Some(file_error);
                rec.save_failed = false;
                rec.known.0 = mark;
                if rec
                    .unread
                    .as_ref()
                    .is_some_and(|unread| unread.file == PANEL_FILE)
                {
                    rec.unread = None;
                }
                if rec.pending.remove(PANEL_FILE) {
                    rec.dropped = Some(rec.revision);
                }
                if !unchanged {
                    self.changed(out, id);
                }
            }
            None => {
                self.skipped.insert(id.to_owned(), reason);
                self.folders.insert(id.to_owned(), FolderState::Panel);
            }
        }
    }

    fn read_failed(
        &mut self,
        out: &mut Out,
        id: &str,
        file: &'static str,
        detail: String,
        now: Millis,
    ) {
        self.reading.remove(id);
        match self.records.get_mut(id) {
            Some(rec) => {
                rec.file_error = Some(format!(
                    "Couldn't read {} ({detail}). Showing the last good revision. Changes can't be saved until it can be read.",
                    file_path(id, file)
                ));
                rec.save_failed = false;
                rec.unread = Some(UnreadInfo { file, detail });
                self.reread_at.insert(
                    id.to_owned(),
                    now.saturating_add(Millis(self.timing.retry_ms)),
                );
                self.changed(out, id);
            }
            None => {
                self.skipped
                    .insert(id.to_owned(), PanelSkip::ReadFailed(detail).reason());
                self.folders.insert(id.to_owned(), FolderState::Panel);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn on_content(
        &mut self,
        out: &mut Out,
        id: &str,
        file: &str,
        got: Got,
        fallback: bool,
        now: Millis,
        ids: &mut dyn IdSource,
    ) {
        let Some(parsed) = self.reading.get(id) else {
            return;
        };
        let renderer = parsed.file.renderer;
        if file != content_file(renderer) {
            return;
        }
        let Some(parsed) = self.reading.remove(id) else {
            return;
        };
        let (mark, source, core_error) = match got {
            Got::Failed(detail) => {
                self.read_failed(out, id, content_file(renderer), detail, now);
                return;
            }
            Got::Missing => (FileMark::Missing, String::new(), None),
            Got::TooLarge { size } => (
                FileMark::TooLarge,
                String::new(),
                Some(too_large_source(size)),
            ),
            Got::Bytes { bytes, hash } => {
                let (source, error) = outside_source(&self.id, id, renderer, bytes);
                (FileMark::Hash(hash), source, error)
            }
        };
        let pair = (parsed.json_mark, mark);
        let fallback = parsed.json_fallback || fallback;
        let revision_for_new = match self.records.contains_key(id) {
            true => None,
            false => Some(ids.take_id()),
        };
        match self.records.get_mut(id) {
            Some(rec) => {
                let unread = rec.unread.take().is_some();
                let read_only_changed = rec.read_only != fallback;
                rec.read_only = fallback;
                let ignored = pair == rec.known
                    && !unread
                    && rec.known.0 != FileMark::TooLarge
                    && rec.known.1 != FileMark::TooLarge;
                if ignored {
                    let retry_later = rec.conflict_read && !rec.pending.is_empty();
                    if retry_later {
                        rec.save_retry_at = Some(now.saturating_add(Millis(self.timing.retry_ms)));
                    }
                    rec.conflict_read = false;
                    if read_only_changed {
                        self.changed(out, id);
                    }
                    if !retry_later {
                        self.service(out, id);
                    }
                    return;
                }
                let old_revision = rec.revision;
                rec.conflict_read = false;
                rec.revision = ids.take_id();
                rec.file = parsed.file;
                rec.source = source;
                rec.hidden = false;
                rec.known = pair;
                rec.file_error = None;
                rec.save_failed = false;
                rec.invalid = None;
                rec.save_retry_at = None;
                if !rec.pending.is_empty() {
                    rec.dropped = Some(old_revision);
                    rec.pending.clear();
                }
                self.reread_at.remove(id);
                self.begin_validation(out, id, core_error);
                self.service(out, id);
                self.release_deferred(out, id);
            }
            None => {
                let revision = revision_for_new.unwrap_or_default();
                let mut rec = Record::new(parsed.file, source, revision);
                rec.known = pair;
                rec.read_only = fallback;
                self.forget(id);
                self.skipped.remove(id);
                self.folders.insert(id.to_owned(), FolderState::Panel);
                self.records.insert(id.to_owned(), rec);
                self.begin_validation(out, id, core_error);
            }
        }
    }

    // ---- outside changes

    fn on_change(&mut self, path: &str, now: Millis) {
        if path == PANELS_DIR {
            bump_range(&mut self.relist_debounce, now);
            let known: Vec<String> = self
                .folders
                .iter()
                .filter(|(_, state)| {
                    matches!(
                        state,
                        FolderState::Reading | FolderState::Panel | FolderState::Empty
                    )
                })
                .map(|(id, _)| id.clone())
                .chain(
                    self.records
                        .iter()
                        .filter(|(_, rec)| rec.lifetime == Lifetime::Persistent)
                        .map(|(id, _)| id.clone()),
                )
                .collect();
            for id in known {
                bump(&mut self.debounce, &id, now);
            }
            return;
        }
        let Some(rest) = path
            .strip_prefix(PANELS_DIR)
            .and_then(|r| r.strip_prefix('/'))
        else {
            return;
        };
        let Some((id, file)) = rest.split_once('/') else {
            return;
        };
        if !matches!(file, PANEL_FILE | "diagram.mmd" | "content.md") {
            return;
        }
        if !valid_panel_folder(id)
            || matches!(
                self.folders.get(id),
                Some(FolderState::Symlink | FolderState::Shadowed { .. })
            )
        {
            return;
        }
        bump(&mut self.debounce, id, now);
    }

    fn on_listing(
        &mut self,
        out: &mut Out,
        listing: Result<Option<DirListing>, StoreError>,
        now: Millis,
    ) {
        let listing = match listing {
            Err(error) => {
                self.listing_failed = Some(store_error_detail(error));
                self.relist_at = Some(now.saturating_add(Millis(self.timing.retry_ms)));
                return;
            }
            Ok(listing) => listing,
        };
        self.listed = true;
        self.listing_failed = None;
        self.relist_at = None;
        let (entries, truncated) = match listing {
            Some(listing) => (listing.entries, listing.truncated),
            None => (Vec::new(), false),
        };
        let mut names: Vec<(String, EntryKind)> = entries
            .into_iter()
            .filter(|entry| matches!(entry.kind, EntryKind::Directory | EntryKind::Symlink))
            .filter(|entry| valid_panel_folder(&entry.name))
            .map(|entry| (entry.name, entry.kind))
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        let listed: BTreeSet<String> = names.iter().map(|(name, _)| name.clone()).collect();
        let (shadowed, names): (Vec<_>, Vec<_>) = names
            .into_iter()
            .partition(|(name, _)| self.scratch_held(name));
        let mut notes = Vec::new();
        if truncated {
            notes.push(format!(
                ".workspace/{PANELS_DIR} has more than {PANEL_LIST_MAX} entries; only some were read"
            ));
        }
        if names.len() > MAX_PANELS {
            notes.push(format!(
                ".workspace/{PANELS_DIR} has more than {MAX_PANELS} panel folders; only the first {MAX_PANELS} by name were read"
            ));
        }
        self.over_hundred = truncated || names.len() > MAX_PANELS;
        self.list_note = (!notes.is_empty()).then(|| notes.join("; "));

        let gone: Vec<String> = self
            .folders
            .iter()
            .filter(|(name, state)| {
                !listed.contains(name.as_str()) && **state != FolderState::Reading
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in gone {
            let persistent = self
                .records
                .get(&name)
                .is_some_and(|rec| rec.lifetime == Lifetime::Persistent);
            if persistent {
                self.request_read(out, &name);
            } else {
                self.folders.remove(&name);
                self.skipped.remove(&name);
            }
        }
        for (name, kind) in &names {
            if *kind == EntryKind::Symlink {
                self.folders.insert(name.clone(), FolderState::Symlink);
                self.skipped
                    .insert(name.clone(), PanelSkip::Symlink.reason());
                continue;
            }
            if self.folders.get(name) == Some(&FolderState::Symlink) {
                self.folders.remove(name);
                self.skipped.remove(name);
            }
            let known = self.records.contains_key(name)
                || self
                    .folders
                    .get(name)
                    .is_some_and(|state| *state != FolderState::NotRead);
            if !known {
                self.admit_folder(out, name, self.held());
            }
        }
        for (name, kind) in &shadowed {
            self.shadow(name, *kind == EntryKind::Symlink);
        }
        self.admit_waiting(out);
    }

    // ---- time

    fn tick(&mut self, out: &mut Out, now: Millis) {
        let relist_due = self
            .relist_debounce
            .is_some_and(|range| self.debounce_due(range) <= now)
            || self.relist_at.is_some_and(|at| at <= now);
        if relist_due {
            self.relist_debounce = None;
            self.relist_at = None;
            out.effects.push(PanelEffect::ListWorkspace {
                space: self.id.clone(),
            });
        }
        let mut due: BTreeSet<String> = self
            .debounce
            .iter()
            .filter(|(_, range)| self.debounce_due(**range) <= now)
            .map(|(id, _)| id.clone())
            .collect();
        due.extend(
            self.reread_at
                .iter()
                .filter(|(_, at)| **at <= now)
                .map(|(id, _)| id.clone()),
        );
        for id in due {
            self.debounce.remove(&id);
            self.reread_at.remove(&id);
            self.request_read(out, &id);
        }
        let retries: Vec<String> = self
            .records
            .iter()
            .filter(|(_, rec)| rec.save_retry_at.is_some_and(|at| at <= now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in retries {
            if let Some(rec) = self.records.get_mut(&id) {
                rec.save_retry_at = None;
            }
            self.service(out, &id);
        }
        let checks: Vec<String> = self
            .records
            .iter()
            .filter(|(_, rec)| rec.check_retry_at.is_some_and(|at| at <= now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in checks {
            if let Some(rec) = self.records.get_mut(&id) {
                rec.check_retry_at = None;
                rec.wants_check = rec.live();
                if rec.wants_check {
                    out.enqueue((self.id.clone(), id.clone()));
                }
            }
        }
    }

    fn next_deadline(&self) -> Option<Millis> {
        let debounces = self
            .debounce
            .values()
            .map(|range| self.debounce_due(*range));
        let relist = self
            .relist_debounce
            .map(|range| self.debounce_due(range))
            .into_iter()
            .chain(self.relist_at);
        let rereads = self.reread_at.values().copied();
        let saves = self.records.values().filter_map(|rec| rec.save_retry_at);
        let checks = self.records.values().filter_map(|rec| rec.check_retry_at);
        debounces
            .chain(relist)
            .chain(rereads)
            .chain(saves)
            .chain(checks)
            .min()
    }

    // ---- listing and checks

    fn apply_verdict(
        &mut self,
        out: &mut Out,
        id: &str,
        result: Result<DiagramVerdict, DiagramCheckError>,
        now: Millis,
    ) {
        let timing = self.timing;
        let Some(rec) = self.records.get_mut(id) else {
            return;
        };
        match result {
            Ok(DiagramVerdict::Validated) => {
                rec.state = PanelState::Validated;
                rec.error = None;
            }
            Ok(DiagramVerdict::ValidatedParserOnly) => {
                rec.state = PanelState::ValidatedParserOnly;
                rec.error = None;
            }
            Ok(DiagramVerdict::Failed(failure)) => {
                rec.state = PanelState::Failed;
                rec.error = Some(clip_error(PanelError {
                    code: failure.code.as_str().to_owned(),
                    message: failure.message,
                    line: failure.line,
                    column: failure.column,
                    snippet: failure.snippet,
                }));
            }
            Err(DiagramCheckError::NoVerdict(detail)) => {
                let message = if detail.is_empty() {
                    "the diagram validator gave no verdict".to_owned()
                } else {
                    detail
                };
                rec.state = PanelState::Failed;
                rec.error = Some(plain_error("no_verdict", message));
            }
            Err(DiagramCheckError::Unavailable(reason)) => {
                match timing.check_retry_ms.get(rec.retries_done) {
                    Some(delay) => {
                        rec.check_retry_at = Some(now.saturating_add(Millis(*delay)));
                        rec.retries_done += 1;
                        return;
                    }
                    None => {
                        rec.state = PanelState::Failed;
                        rec.error = Some(plain_error(
                            "unavailable",
                            format!("validation is unavailable: {reason}"),
                        ));
                    }
                }
            }
        }
        self.changed(out, id);
    }
}

enum Missing {
    Get,
    Close,
    Refuse,
}

fn bump_range(range: &mut Option<(Millis, Millis)>, now: Millis) {
    let first = range.map_or(now, |(first, _)| first);
    *range = Some((first, now));
}

fn invalid_field(field: &'static str, rule: &str) -> PanelApiError {
    PanelApiError::InvalidField {
        field,
        rule: rule.to_owned(),
    }
}

fn scratch_list_refusal(detail: &str) -> PanelApiError {
    PanelApiError::ScratchListingFailed {
        detail: detail.to_owned(),
    }
}

fn unread_refusal(id: &str, unread: &UnreadInfo) -> PanelApiError {
    PanelApiError::Unreadable {
        path: file_path(id, unread.file),
        detail: unread.detail.clone(),
    }
}

/// The daemon's panels: every space's records, the checks running, and the work asked for.
pub struct PanelStore {
    timing: PanelTiming,
    spaces: BTreeMap<SpaceId, SpaceState>,
    out: Out,
    scratch_retry_at: Option<Millis>,
    next_sweep: Millis,
}

impl PanelStore {
    pub fn new(timing: PanelTiming) -> PanelStore {
        PanelStore {
            timing,
            spaces: BTreeMap::new(),
            out: Out::default(),
            scratch_retry_at: None,
            next_sweep: Millis(0),
        }
    }

    /// Asks for the scratch folders to be listed.
    pub fn start(&mut self, _now: Millis) {
        self.out.effects.push(PanelEffect::ListScratch);
    }

    /// Starts watching and listing a space, once.
    pub fn add_space(&mut self, space: &SpaceId, _now: Millis) {
        if !space.0.starts_with('/') {
            self.out.effects.push(PanelEffect::Log {
                text: format!(
                    "the space {} is not an absolute folder; its panels are ignored",
                    space.0
                ),
            });
            return;
        }
        if self.spaces.contains_key(space) {
            return;
        }
        self.spaces
            .insert(space.clone(), SpaceState::new(space.clone(), self.timing));
        self.out.effects.push(PanelEffect::Watch {
            space: space.clone(),
        });
        self.out.effects.push(PanelEffect::ListWorkspace {
            space: space.clone(),
        });
        self.finish();
    }

    /// Runs `call` on the space and reports a change of its skipped folders or listing error.
    fn in_space<R>(
        &mut self,
        space: &SpaceId,
        call: impl FnOnce(&mut SpaceState, &mut Out) -> R,
    ) -> Option<R> {
        let state = self.spaces.get_mut(space)?;
        let before = state.signature();
        let result = call(state, &mut self.out);
        if state.signature() != before {
            self.out.events.push(PanelEvent::Listing {
                space: space.clone(),
            });
        }
        Some(result)
    }

    pub fn create(
        &mut self,
        space: &SpaceId,
        create: PanelCreate,
        now: Millis,
        wall_ms: u64,
        ids: &mut dyn IdSource,
    ) -> Result<PanelView, PanelApiError> {
        self.create_with_outcome(space, create, now, wall_ms, ids)
            .map(|outcome| outcome.view)
    }

    /// `create`, also saying whether the call made the panel (false for an idempotent repeat).
    pub fn create_with_outcome(
        &mut self,
        space: &SpaceId,
        create: PanelCreate,
        _now: Millis,
        wall_ms: u64,
        ids: &mut dyn IdSource,
    ) -> Result<CreateOutcome, PanelApiError> {
        let result = self
            .in_space(space, |state, out| state.create(out, create, ids, wall_ms))
            .unwrap_or(Err(PanelApiError::UnknownSpace));
        self.finish();
        result.map(|(view, created)| CreateOutcome { view, created })
    }

    pub fn update(
        &mut self,
        space: &SpaceId,
        id: &str,
        update: PanelUpdate,
        _now: Millis,
        wall_ms: u64,
        ids: &mut dyn IdSource,
    ) -> Result<PanelView, PanelApiError> {
        let result = self
            .in_space(space, |state, out| {
                state.update(out, id, update, ids, wall_ms)
            })
            .unwrap_or(Err(PanelApiError::UnknownSpace));
        self.finish();
        result
    }

    pub fn remove(
        &mut self,
        space: &SpaceId,
        id: &str,
        _now: Millis,
    ) -> Result<PanelView, PanelApiError> {
        let result = self
            .in_space(space, |state, out| state.remove(out, id))
            .unwrap_or(Err(PanelApiError::UnknownSpace));
        self.finish();
        result
    }

    pub fn close(
        &mut self,
        space: &SpaceId,
        id: &str,
        _now: Millis,
    ) -> Result<PanelView, PanelApiError> {
        let result = self
            .in_space(space, |state, out| state.close(out, id))
            .unwrap_or(Err(PanelApiError::UnknownSpace));
        self.finish();
        result
    }

    pub fn pin(
        &mut self,
        space: &SpaceId,
        id: &str,
        _now: Millis,
    ) -> Result<PanelView, PanelApiError> {
        let result = self
            .in_space(space, |state, out| state.pin(out, id))
            .unwrap_or(Err(PanelApiError::UnknownSpace));
        self.finish();
        result
    }

    pub fn list(&self, space: &SpaceId) -> Result<PanelList, PanelApiError> {
        let state = self.spaces.get(space).ok_or(PanelApiError::UnknownSpace)?;
        let error = match (&self.out.scratch.list_failed, state.list_error()) {
            (Some(detail), Some(error)) => Some(format!(
                "{error}; Couldn't list the scratch panels ({detail})"
            )),
            (Some(detail), None) => Some(format!("Couldn't list the scratch panels ({detail})")),
            (None, error) => error,
        };
        if state.listing_failed.is_none() && !state.loaded {
            return Err(PanelApiError::SpaceLoading);
        }
        Ok(PanelList {
            panels: state
                .records
                .iter()
                .map(|(id, rec)| state.view_of(id, rec))
                .collect(),
            skipped: state
                .skipped
                .iter()
                .map(|(panel, reason)| SkippedPanel {
                    panel: panel.clone(),
                    reason: reason.clone(),
                })
                .collect(),
            error,
        })
    }

    /// The panel and its served source.
    pub fn get(&self, space: &SpaceId, id: &str) -> Result<(PanelView, &str), PanelApiError> {
        let state = self.spaces.get(space).ok_or(PanelApiError::UnknownSpace)?;
        match state.records.get(id) {
            Some(rec) => {
                let source = if rec.live() && !rec.hidden {
                    rec.source.as_str()
                } else {
                    ""
                };
                Ok((state.view_of(id, rec), source))
            }
            None => state
                .missing(&self.out.scratch, id, Missing::Get)
                .map(|view| (view, "")),
        }
    }

    /// A file under `.workspace/` of the space changed; `path` is relative to it.
    pub fn apply_change(&mut self, space: &SpaceId, path: &str, now: Millis) {
        if let Some(state) = self.spaces.get_mut(space) {
            state.on_change(path, now);
        }
    }

    pub fn apply_listing(
        &mut self,
        space: &SpaceId,
        listing: Result<Option<DirListing>, StoreError>,
        now: Millis,
    ) {
        self.in_space(space, |state, out| state.on_listing(out, listing, now));
        self.finish();
    }

    pub fn apply_scratch_listing(
        &mut self,
        listing: Result<ScratchListing, StoreError>,
        now: Millis,
    ) {
        let before = self.out.scratch.list_failed.clone();
        match listing {
            Ok(listing) => {
                self.scratch_retry_at = None;
                self.on_scratch_listing(listing);
            }
            Err(error) => {
                let detail = store_error_detail(error);
                if self.out.scratch.list_failed.as_ref() != Some(&detail) {
                    self.out.effects.push(PanelEffect::Log {
                        text: format!("the scratch panels couldn't be listed: {detail}"),
                    });
                }
                self.out.scratch.list_failed = Some(detail);
                self.scratch_retry_at = Some(now.saturating_add(Millis(self.timing.retry_ms)));
            }
        }
        if self.out.scratch.list_failed != before {
            for (space, state) in &self.spaces {
                if state.loaded || state.listing_failed.is_some() {
                    self.out.events.push(PanelEvent::Listing {
                        space: space.clone(),
                    });
                }
            }
        }
        self.finish();
    }

    pub fn apply_read(
        &mut self,
        at: &PanelFolder,
        file: &str,
        read: PanelRead,
        now: Millis,
        ids: &mut dyn IdSource,
    ) {
        match at {
            PanelFolder::Workspace { space, panel } => {
                self.in_space(space, |state, out| {
                    state.on_read(out, panel, file, read, now, ids);
                });
            }
            PanelFolder::Scratch(dir) => self.on_scratch_read(dir, file, read, now, ids),
        }
        self.finish();
    }

    pub fn apply_saved(&mut self, ticket: SaveTicket, report: SaveReport, now: Millis) {
        self.apply_report(ticket, report, now);
    }

    pub fn apply_deleted(&mut self, ticket: SaveTicket, report: SaveReport, now: Millis) {
        self.apply_report(ticket, report, now);
    }

    fn apply_report(&mut self, ticket: SaveTicket, report: SaveReport, now: Millis) {
        let Some(op) = self.out.saves.remove(&ticket) else {
            return;
        };
        match op.kind {
            OpKind::ScratchGone {
                dir,
                swept,
                restore,
            } => self.scratch_gone(&op.space, &dir, swept, restore, report),
            kind => {
                self.in_space(&op.space, |state, out| {
                    state.apply_report(out, &op.panel, ticket, kind, report, now);
                });
            }
        }
        self.finish();
    }

    pub fn apply_check(
        &mut self,
        space: &SpaceId,
        panel: &str,
        revision: u64,
        result: Result<DiagramVerdict, DiagramCheckError>,
        now: Millis,
    ) {
        let key = (space.clone(), panel.to_owned());
        if self.out.in_flight.get(&key) != Some(&revision) {
            return;
        }
        self.out.in_flight.remove(&key);
        self.in_space(space, |state, out| {
            let current = state
                .records
                .get(panel)
                .is_some_and(|rec| rec.live() && rec.revision == revision);
            if current {
                state.apply_verdict(out, panel, result, now);
            } else if state.records.get(panel).is_some_and(|rec| rec.wants_check) {
                out.enqueue(key.clone());
            }
        });
        self.finish();
    }

    pub fn tick(&mut self, now: Millis, wall_ms: u64) {
        let spaces: Vec<SpaceId> = self.spaces.keys().cloned().collect();
        for space in spaces {
            self.in_space(&space, |state, out| state.tick(out, now));
        }
        if self.scratch_retry_at.is_some_and(|at| at <= now) {
            self.scratch_retry_at = None;
            self.out.effects.push(PanelEffect::ListScratch);
        }
        if self.out.scratch.done() && self.next_sweep <= now {
            self.next_sweep = now.saturating_add(Millis(self.timing.sweep_every_ms));
            self.sweep(wall_ms);
        }
        self.finish();
    }

    pub fn take_effects(&mut self) -> Vec<PanelEffect> {
        std::mem::take(&mut self.out.effects)
    }

    /// The closes and removes whose scratch folder couldn't be deleted since the last call.
    pub fn take_delete_failures(&mut self) -> Vec<DeleteFailure> {
        std::mem::take(&mut self.out.scratch.failures)
    }

    /// A close or remove of this scratch panel waits for its folder's delete to end.
    pub fn close_pending(&self, space: &SpaceId, panel: &str) -> bool {
        self.out.saves.values().any(|op| {
            op.space == *space
                && op.panel == panel
                && matches!(
                    &op.kind,
                    OpKind::ScratchGone {
                        restore: Some(_),
                        ..
                    }
                )
        })
    }

    pub fn take_events(&mut self) -> Vec<PanelEvent> {
        std::mem::take(&mut self.out.events)
    }

    pub fn next_deadline(&self) -> Option<Millis> {
        self.spaces
            .values()
            .filter_map(SpaceState::next_deadline)
            .chain(self.scratch_retry_at)
            .chain(self.out.scratch.done().then_some(self.next_sweep))
            .min()
    }

    /// The number of checks running.
    pub fn in_flight(&self) -> usize {
        self.out.in_flight.len()
    }

    /// Ends every call: scratch folders waiting on workspace reads are tried again, a space whose
    /// load is settled stops loading, and queued checks start.
    fn finish(&mut self) {
        for state in self.spaces.values_mut() {
            if state.listed && !state.waiting_scratch.is_empty() {
                state.admit_waiting(&mut self.out);
            }
            state.update_loaded(&self.out.scratch);
        }
        self.pump();
    }

    /// Starts queued checks while a slot is free.
    fn pump(&mut self) {
        while self.out.in_flight.len() < MAX_CHECKS_IN_FLIGHT {
            let Some(key) = self.out.queue.pop_front() else {
                return;
            };
            self.out.queued.remove(&key);
            if self.out.in_flight.contains_key(&key) {
                continue;
            }
            let Some(rec) = self
                .spaces
                .get_mut(&key.0)
                .and_then(|state| state.records.get_mut(&key.1))
            else {
                continue;
            };
            if !rec.live() || !rec.wants_check {
                continue;
            }
            rec.wants_check = false;
            self.out.in_flight.insert(key.clone(), rec.revision);
            self.out.effects.push(PanelEffect::Check {
                space: key.0,
                panel: key.1,
                revision: rec.revision,
                source: rec.source.clone(),
            });
        }
    }
}
