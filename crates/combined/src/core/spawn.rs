//! Spawns: listing a space's terminal templates and starting terminals on request. No I/O.
//!
//! Two requests drive it, `list_templates` and `spawn_terminal`. Each takes a ticket and answers
//! with effects to run, reported back through the `apply_*` methods, and replies to send. A
//! `Spawn` effect comes only from a `spawn_terminal` request: nothing else starts a terminal.
//! The daemon reads `panel.json` again at spawn time; a client names a template by its panel ID
//! and never supplies a command.

use std::collections::BTreeMap;

use combine_protocol::{
    DaemonMessage, ErrorCode, ErrorMessage, RequestId, SkippedPanel, SpawnSource, Spawned,
    TemplateInfo, Templates, TermId,
};

use crate::core::effects::ConnectionId;
use crate::core::layout_sync::store_error_detail;
use crate::core::panels::{
    panel_folder_refusal, parse_terminal_panel, shell_spec, template_info, template_spec,
    valid_panel_folder, CwdRefusal, PanelSkip, TerminalTemplate, MAX_PANELS, PANELS_DIR,
    PANEL_LIST_MAX,
};
use crate::model::{SpaceId, SpawnSpec};
use crate::ports::{BackendResult, DirListing, EntryKind, ProjectDirs, StoreError, StoredFile};

/// How many template and spawn requests one connection may have unanswered.
pub const MAX_SPAWN_IN_FLIGHT: usize = 4;

/// Names one request's work while the caller runs its effects. It never goes on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SpawnTicket(pub u64);

/// What the file store found when it read one `panel.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelRead {
    Missing,
    Found(Vec<u8>),
    /// The file is larger than the bound the read was given.
    TooLarge {
        size: u64,
    },
    Failed(String),
}

impl From<Result<Option<StoredFile>, StoreError>> for PanelRead {
    fn from(result: Result<Option<StoredFile>, StoreError>) -> PanelRead {
        match result {
            Ok(None) => PanelRead::Missing,
            Ok(Some(file)) => PanelRead::Found(file.bytes),
            Err(StoreError::TooLarge { size, .. }) => PanelRead::TooLarge { size },
            Err(other) => PanelRead::Failed(store_error_detail(other)),
        }
    }
}

/// Work for the server to do and report back with the matching `apply_*` method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnEffect {
    /// List `.workspace/panels` of the space; answer with [`Spawns::apply_listing`].
    ListPanels { ticket: SpawnTicket, space: SpaceId },
    /// Read `panels/<panel>/panel.json` for each panel, bounded by `PANEL_MAX_BYTES`; answer with
    /// [`Spawns::apply_panels`], one entry per panel.
    ReadPanels {
        ticket: SpawnTicket,
        space: SpaceId,
        panels: Vec<String>,
    },
    /// Start a terminal in the backend; answer with [`Spawns::apply_spawned`].
    Spawn {
        ticket: SpawnTicket,
        spec: SpawnSpec,
    },
}

impl SpawnEffect {
    fn ticket(&self) -> SpawnTicket {
        match self {
            SpawnEffect::ListPanels { ticket, .. }
            | SpawnEffect::ReadPanels { ticket, .. }
            | SpawnEffect::Spawn { ticket, .. } => *ticket,
        }
    }
}

#[derive(Debug)]
enum Step {
    /// Waiting for the `.workspace/panels` listing.
    Listing,
    /// Waiting for the panel files of a listing; `skipped` and `note` are what the listing
    /// already decided.
    ReadingList {
        panels: Vec<String>,
        skipped: BTreeMap<String, String>,
        note: Option<String>,
    },
    /// Waiting for the one panel file a spawn names.
    ReadingTemplate { panel: String },
    /// Waiting for the backend.
    Spawning,
}

#[derive(Debug)]
struct Pending {
    conn: ConnectionId,
    request: RequestId,
    space: SpaceId,
    step: Step,
}

pub struct Spawns {
    dirs: Box<dyn ProjectDirs>,
    next_ticket: u64,
    in_flight: BTreeMap<ConnectionId, usize>,
    tickets: BTreeMap<SpawnTicket, Pending>,
    effects: Vec<SpawnEffect>,
    replies: Vec<(ConnectionId, DaemonMessage)>,
}

fn error(request: RequestId, code: ErrorCode, message: String) -> DaemonMessage {
    DaemonMessage::Error(ErrorMessage {
        code,
        message,
        request: Some(request),
        attachment: None,
    })
}

fn clip_reason(skip: &PanelSkip) -> String {
    skip.reason()
}

impl Spawns {
    pub fn new(dirs: Box<dyn ProjectDirs>) -> Spawns {
        Spawns {
            dirs,
            next_ticket: 0,
            in_flight: BTreeMap::new(),
            tickets: BTreeMap::new(),
            effects: Vec::new(),
            replies: Vec::new(),
        }
    }

    /// `listed` is whether the host lists `space`.
    pub fn list_templates(
        &mut self,
        conn: ConnectionId,
        request: RequestId,
        space: SpaceId,
        listed: bool,
    ) {
        let Some(ticket) = self.admit(conn, request, listed, None) else {
            return;
        };
        self.tickets.insert(
            ticket,
            Pending {
                conn,
                request,
                space: space.clone(),
                step: Step::Listing,
            },
        );
        self.effects.push(SpawnEffect::ListPanels { ticket, space });
    }

    /// `listed` is whether the host lists `space`.
    pub fn spawn_terminal(
        &mut self,
        conn: ConnectionId,
        request: RequestId,
        space: SpaceId,
        source: SpawnSource,
        listed: bool,
    ) {
        let panel = match &source {
            SpawnSource::Shell => None,
            SpawnSource::Template { panel } => Some(panel.as_str()),
        };
        let Some(ticket) = self.admit(conn, request, listed, panel) else {
            return;
        };
        let step = match source {
            SpawnSource::Shell => Step::Spawning,
            SpawnSource::Template { panel } => Step::ReadingTemplate { panel },
        };
        let pending = Pending {
            conn,
            request,
            space: space.clone(),
            step,
        };
        let effect = match &pending.step {
            Step::ReadingTemplate { panel } => Some(SpawnEffect::ReadPanels {
                ticket,
                space: space.clone(),
                panels: vec![panel.clone()],
            }),
            _ => match self.dirs.check_dir(&space, "") {
                Ok(()) => Some(SpawnEffect::Spawn {
                    ticket,
                    spec: shell_spec(&space),
                }),
                Err(refusal) => {
                    self.tickets.insert(ticket, pending);
                    let why = CwdRefusal::Dir(refusal);
                    let message = format!("the project folder {} {why}", space.0);
                    self.fail(ticket, ErrorCode::CwdRefused, message);
                    return;
                }
            },
        };
        self.tickets.insert(ticket, pending);
        self.effects.extend(effect);
    }

    pub fn apply_listing(
        &mut self,
        ticket: SpawnTicket,
        listing: Result<Option<DirListing>, StoreError>,
    ) {
        let Some(pending) = self.tickets.get(&ticket) else {
            return;
        };
        if !matches!(pending.step, Step::Listing) {
            return;
        }
        let (request, space) = (pending.request, pending.space.clone());
        let templates = |error: Option<String>| {
            DaemonMessage::Templates(Templates {
                request,
                space: space.0.clone(),
                templates: Vec::new(),
                skipped: Vec::new(),
                error,
            })
        };
        let listing = match listing {
            Ok(None) => return self.finish(ticket, templates(None)),
            Err(failure) => {
                let detail = store_error_detail(failure);
                return self.finish(
                    ticket,
                    templates(Some(format!(
                        "Couldn't list .workspace/{PANELS_DIR} ({detail})"
                    ))),
                );
            }
            Ok(Some(listing)) => listing,
        };

        let mut folders: Vec<(String, EntryKind)> = listing
            .entries
            .into_iter()
            .filter(|entry| matches!(entry.kind, EntryKind::Directory | EntryKind::Symlink))
            .filter(|entry| valid_panel_folder(&entry.name))
            .map(|entry| (entry.name, entry.kind))
            .collect();
        folders.sort_by(|a, b| a.0.cmp(&b.0));
        let mut notes = Vec::new();
        if listing.truncated {
            notes.push(format!(
                ".workspace/{PANELS_DIR} has more than {PANEL_LIST_MAX} entries; only some were read"
            ));
        }
        if folders.len() > MAX_PANELS {
            folders.truncate(MAX_PANELS);
            notes.push(format!(
                ".workspace/{PANELS_DIR} has more than {MAX_PANELS} panel folders; only the first {MAX_PANELS} by name were read"
            ));
        }
        let note = (!notes.is_empty()).then(|| notes.join("; "));

        let mut skipped = BTreeMap::new();
        let mut panels = Vec::new();
        for (name, kind) in folders {
            if kind == EntryKind::Symlink {
                skipped.insert(name, clip_reason(&PanelSkip::Symlink));
            } else {
                panels.push(name);
            }
        }
        if panels.is_empty() {
            let message = Self::templates_reply(request, &space, Vec::new(), skipped, note);
            return self.finish(ticket, message);
        }
        self.effects.push(SpawnEffect::ReadPanels {
            ticket,
            space,
            panels: panels.clone(),
        });
        if let Some(pending) = self.tickets.get_mut(&ticket) {
            pending.step = Step::ReadingList {
                panels,
                skipped,
                note,
            };
        }
    }

    pub fn apply_panels(&mut self, ticket: SpawnTicket, reads: Vec<(String, PanelRead)>) {
        let Some(pending) = self.tickets.get_mut(&ticket) else {
            return;
        };
        let step = std::mem::replace(&mut pending.step, Step::Spawning);
        let (request, space) = (pending.request, pending.space.clone());
        match step {
            Step::ReadingList {
                panels,
                mut skipped,
                note,
            } => {
                let mut reads: BTreeMap<String, PanelRead> = reads.into_iter().collect();
                let mut templates = Vec::new();
                for panel in panels {
                    let read = reads
                        .remove(&panel)
                        .unwrap_or_else(|| PanelRead::Failed("not read".to_owned()));
                    match self.template_of(&space, &panel, read) {
                        Ok(template) => templates.push(template_info(&template)),
                        Err(skip) => {
                            skipped.insert(panel, clip_reason(&skip));
                        }
                    }
                }
                let message = Self::templates_reply(request, &space, templates, skipped, note);
                self.finish(ticket, message);
            }
            Step::ReadingTemplate { panel } => {
                let read = reads
                    .into_iter()
                    .find(|(name, _)| *name == panel)
                    .map_or(PanelRead::Missing, |(_, read)| read);
                match self.template_of(&space, &panel, read) {
                    Ok(template) => {
                        let spec = template_spec(&space, &template);
                        self.effects.push(SpawnEffect::Spawn { ticket, spec });
                    }
                    Err(PanelSkip::Cwd { cwd, refusal }) => {
                        let message = format!("the template's cwd \"{cwd}\" {refusal}");
                        self.fail(ticket, ErrorCode::CwdRefused, message);
                    }
                    Err(skip) => {
                        let message = format!(
                            "no usable terminal template in {PANELS_DIR}/{panel}: {}",
                            clip_reason(&skip)
                        );
                        self.fail(ticket, ErrorCode::UnknownTemplate, message);
                    }
                }
            }
            other => {
                if let Some(pending) = self.tickets.get_mut(&ticket) {
                    pending.step = other;
                }
            }
        }
    }

    pub fn apply_spawned(&mut self, ticket: SpawnTicket, result: BackendResult<TermId>) {
        let Some(pending) = self.tickets.get(&ticket) else {
            return;
        };
        if !matches!(pending.step, Step::Spawning) {
            return;
        }
        let (request, space) = (pending.request, pending.space.0.clone());
        match result {
            Ok(terminal) => {
                let message = DaemonMessage::Spawned(Spawned {
                    request,
                    space,
                    terminal,
                });
                self.finish(ticket, message);
            }
            Err(failure) => {
                let message = format!("couldn't start the terminal: {failure}");
                self.fail(ticket, ErrorCode::SpawnFailed, message);
            }
        }
    }

    /// Forgets the connection's requests: their tickets get no reply and take no further step.
    pub fn disconnect(&mut self, conn: ConnectionId) {
        self.in_flight.remove(&conn);
        self.tickets.retain(|_, pending| pending.conn != conn);
    }

    pub fn take_effects(&mut self) -> Vec<SpawnEffect> {
        let mut effects = std::mem::take(&mut self.effects);
        effects.retain(|effect| self.tickets.contains_key(&effect.ticket()));
        effects
    }

    pub fn take_replies(&mut self) -> Vec<(ConnectionId, DaemonMessage)> {
        std::mem::take(&mut self.replies)
    }

    fn in_flight_of(&self, conn: ConnectionId) -> usize {
        self.in_flight.get(&conn).copied().unwrap_or(0)
    }

    /// The cap, then the space check; on success a ticket is taken and `in_flight` raised.
    fn admit(
        &mut self,
        conn: ConnectionId,
        request: RequestId,
        listed: bool,
        panel: Option<&str>,
    ) -> Option<SpawnTicket> {
        if self.in_flight_of(conn) >= MAX_SPAWN_IN_FLIGHT {
            self.replies.push((
                conn,
                error(
                    request,
                    ErrorCode::InvalidRequest,
                    format!(
                        "too many template and spawn requests in flight; the limit is {MAX_SPAWN_IN_FLIGHT}"
                    ),
                ),
            ));
            return None;
        }
        if !listed {
            self.replies.push((
                conn,
                error(
                    request,
                    ErrorCode::UnknownSpace,
                    "the host lists no such space".to_owned(),
                ),
            ));
            return None;
        }
        if let Some(why) = panel.and_then(panel_folder_refusal) {
            self.replies.push((
                conn,
                error(
                    request,
                    ErrorCode::InvalidRequest,
                    format!("the panel ID isn't valid: {why}"),
                ),
            ));
            return None;
        }
        let ticket = SpawnTicket(self.next_ticket);
        self.next_ticket += 1;
        *self.in_flight.entry(conn).or_insert(0) += 1;
        Some(ticket)
    }

    /// Answers a live ticket with an error and retires it.
    fn fail(&mut self, ticket: SpawnTicket, code: ErrorCode, message: String) {
        let Some(pending) = self.tickets.get(&ticket) else {
            return;
        };
        let reply = error(pending.request, code, message);
        self.finish(ticket, reply);
    }

    fn finish(&mut self, ticket: SpawnTicket, message: DaemonMessage) {
        let Some(pending) = self.tickets.remove(&ticket) else {
            return;
        };
        if let Some(count) = self.in_flight.get_mut(&pending.conn) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.in_flight.remove(&pending.conn);
            }
        }
        self.replies.push((pending.conn, message));
    }

    /// The template a panel file defines, with its `cwd` checked against the project.
    fn template_of(
        &self,
        space: &SpaceId,
        panel: &str,
        read: PanelRead,
    ) -> Result<TerminalTemplate, PanelSkip> {
        let bytes = match read {
            PanelRead::Missing => return Err(PanelSkip::NoFile),
            PanelRead::TooLarge { size } => return Err(PanelSkip::TooLarge { size }),
            PanelRead::Failed(detail) => return Err(PanelSkip::ReadFailed(detail)),
            PanelRead::Found(bytes) => bytes,
        };
        let template = parse_terminal_panel(panel, &bytes)?;
        match self.dirs.check_dir(space, &template.cwd) {
            Ok(()) => Ok(template),
            Err(refusal) => Err(PanelSkip::Cwd {
                cwd: template.cwd,
                refusal: CwdRefusal::Dir(refusal),
            }),
        }
    }

    fn templates_reply(
        request: RequestId,
        space: &SpaceId,
        templates: Vec<TemplateInfo>,
        skipped: BTreeMap<String, String>,
        note: Option<String>,
    ) -> DaemonMessage {
        let mut templates = templates;
        templates.sort_by(|a, b| a.panel.cmp(&b.panel));
        DaemonMessage::Templates(Templates {
            request,
            space: space.0.clone(),
            templates,
            skipped: skipped
                .into_iter()
                .map(|(panel, reason)| SkippedPanel { panel, reason })
                .collect(),
            error: note,
        })
    }
}
