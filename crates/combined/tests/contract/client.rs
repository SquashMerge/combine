//! A simulated client: it applies what the daemon sends the way SPEC.md says a client does,
//! and records every way the stream breaks the protocol's invariants.
//!
//! The simulator does no I/O. [`Harness`](super::harness::Harness) feeds it messages and sends
//! what it produces (acks and automatic replies).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use combine_protocol::{
    Ack, AttachmentId, AutomaticReply, ClientMessage, CloseReason, Cursor, DaemonMessage, Display,
    Epoch, ErrorCode, ErrorMessage, Fidelity, OwnershipGeneration, RequestId, Started, TermId,
};
use combined::core::ConnectionId;
use sha2::{Digest, Sha256};

/// The terminal query the simulator answers: a cursor position report.
const QUERY: &[u8] = b"\x1b[?6n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadPolicy {
    /// The harness reads for the client in every settle round.
    Auto,
    /// Only `read_step` and `read_until` read.
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckPolicy {
    /// Acks the last applied event after each read.
    AfterEachRead,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyPolicy {
    /// Replies to live events only, and only while this attachment owns the terminal.
    AsSpec,
    /// Replies to every query.
    Always,
    Never,
}

/// What a client applied, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    Bytes(Vec<u8>),
    Resized(u16, u16),
    Facts(Display),
    Resynced,
    Closed(CloseReason),
    Lost,
}

/// How an attachment's stream ended for the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    Closed(CloseReason),
    Lost,
    Gap,
    ResumeRefused,
}

/// What the client knows of one attachment.
pub struct View {
    pub attachment: AttachmentId,
    pub terminal: TermId,
    pub epoch: Option<Epoch>,
    pub size: (u16, u16),
    pub fidelity: Option<Fidelity>,
    pub started: Vec<Started>,
    pub last_applied: Option<Cursor>,
    pub replay_through: Option<Cursor>,
    pub ownership: Option<(OwnershipGeneration, Option<AttachmentId>)>,
    /// The display of the last `Facts` applied, in any epoch.
    pub last_facts: Option<Display>,
    /// The current epoch, from the measured marker on, with consecutive `Bytes` coalesced.
    pub transcript: Vec<Applied>,
    pub measured_len: u64,
    pub ending: Option<Ending>,
    /// The client's current marker has appeared in this epoch's output.
    pub marker_seen: bool,
    measuring: bool,
    hasher: Sha256,
    scan: Vec<u8>,
}

impl View {
    fn new(attachment: AttachmentId, terminal: TermId) -> Self {
        View {
            attachment,
            terminal,
            epoch: None,
            size: (0, 0),
            fidelity: None,
            started: Vec::new(),
            last_applied: None,
            replay_through: None,
            ownership: None,
            last_facts: None,
            transcript: Vec::new(),
            measured_len: 0,
            ending: None,
            marker_seen: false,
            measuring: false,
            hasher: Sha256::new(),
            scan: Vec::new(),
        }
    }

    pub fn measured_sha256(&self) -> [u8; 32] {
        self.hasher.clone().finalize().into()
    }

    /// Whether the marker has been applied in the current epoch.
    pub fn measuring(&self) -> bool {
        self.measuring
    }

    /// The stream is still open for events.
    pub fn live(&self) -> bool {
        self.ending.is_none()
    }

    pub fn owns(&self) -> bool {
        self.ownership
            .is_some_and(|(_, owner)| owner == Some(self.attachment))
    }

    /// The transcript's bytes, in order, without the other applied items.
    pub fn bytes(&self) -> Vec<u8> {
        self.transcript
            .iter()
            .filter_map(|item| match item {
                Applied::Bytes(bytes) => Some(bytes.as_slice()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect()
    }

    /// The sizes the transcript's `Resized` items name, in order.
    pub fn resizes(&self) -> Vec<(u16, u16)> {
        self.transcript
            .iter()
            .filter_map(|item| match item {
                Applied::Resized(cols, rows) => Some((*cols, *rows)),
                _ => None,
            })
            .collect()
    }

    fn reset_epoch(&mut self, started: &Started) {
        self.epoch = Some(started.cursor.epoch);
        self.size = (started.cols, started.rows);
        self.fidelity = Some(started.fidelity);
        self.started.push(started.clone());
        self.transcript.clear();
        self.measured_len = 0;
        self.hasher = Sha256::new();
        self.measuring = false;
        self.marker_seen = false;
        self.scan.clear();
    }

    /// Takes in program output: finds the marker across event boundaries, and measures what
    /// follows it.
    fn feed(&mut self, bytes: &[u8], marker: Option<&[u8]>, keep_raw: bool) {
        let mut from = self.measuring.then_some(0);
        if let (false, Some(marker)) = (self.marker_seen, marker) {
            let carried = self.scan.len();
            let mut window = std::mem::take(&mut self.scan);
            window.extend_from_slice(bytes);
            match window.windows(marker.len()).position(|w| w == marker) {
                Some(at) => {
                    self.marker_seen = true;
                    if from.is_none() {
                        from = Some(at + marker.len() - carried);
                        self.measuring = true;
                    }
                }
                None => {
                    let keep = marker.len() - 1;
                    self.scan = window[window.len().saturating_sub(keep)..].to_vec();
                }
            }
        }
        if let Some(from) = from {
            self.measure(&bytes[from..], keep_raw);
        }
    }

    fn measure(&mut self, bytes: &[u8], keep_raw: bool) {
        if bytes.is_empty() {
            return;
        }
        self.hasher.update(bytes);
        self.measured_len += bytes.len() as u64;
        if !keep_raw {
            return;
        }
        match self.transcript.last_mut() {
            Some(Applied::Bytes(last)) => last.extend_from_slice(bytes),
            _ => self.transcript.push(Applied::Bytes(bytes.to_vec())),
        }
    }

    fn note(&mut self, item: Applied) {
        if self.measuring {
            self.transcript.push(item);
        }
    }
}

/// The simulated client. Its fields are public so a case can change a policy mid-run.
pub struct ClientSim {
    pub connection: Option<ConnectionId>,
    pub read: ReadPolicy,
    pub ack: AckPolicy,
    pub replies: ReplyPolicy,
    /// Keep the measured bytes in each transcript. Off for a very large payload, which is
    /// then checked by length and SHA-256 alone.
    pub keep_raw: bool,
    /// Set by `Harness::mark`.
    pub measure_marker: Option<Vec<u8>>,
    pub views: BTreeMap<AttachmentId, View>,
    /// Every message that is not part of an attachment's stream, in arrival order.
    pub controls: Vec<DaemonMessage>,
    /// Every way the stream broke an invariant.
    pub violations: Vec<String>,
    /// Messages pulled from the daemon and not yet applied.
    pub inbox: VecDeque<DaemonMessage>,
    /// Automatic replies produced and not yet sent.
    pub outgoing: Vec<ClientMessage>,
    /// Attachments with an applied event the client has not acked.
    pub unacked: BTreeSet<AttachmentId>,
    attaches: BTreeMap<RequestId, TermId>,
    next_request: u64,
}

impl ClientSim {
    pub fn new() -> Self {
        ClientSim {
            connection: None,
            read: ReadPolicy::Auto,
            ack: AckPolicy::AfterEachRead,
            replies: ReplyPolicy::AsSpec,
            keep_raw: true,
            measure_marker: None,
            views: BTreeMap::new(),
            controls: Vec::new(),
            violations: Vec::new(),
            inbox: VecDeque::new(),
            outgoing: Vec::new(),
            unacked: BTreeSet::new(),
            attaches: BTreeMap::new(),
            next_request: 1,
        }
    }

    pub fn view(&self, attachment: AttachmentId) -> Option<&View> {
        self.views.get(&attachment)
    }

    pub fn next_request(&mut self) -> RequestId {
        let request = RequestId(self.next_request);
        self.next_request += 1;
        request
    }

    /// Remembers which terminal an attach request names, for the view `attached` creates.
    pub fn expect_attach(&mut self, request: RequestId, terminal: TermId) {
        self.attaches.insert(request, terminal);
    }

    /// Starts watching every view of the client for a new marker.
    pub fn expect_marker(&mut self, marker: Vec<u8>) {
        self.measure_marker = Some(marker);
        for view in self.views.values_mut() {
            view.marker_seen = false;
            view.scan.clear();
        }
    }

    /// The answer to an attach request: the attachment, or the error.
    pub fn attach_answer(&self, request: RequestId) -> Option<Result<AttachmentId, ErrorMessage>> {
        self.controls.iter().find_map(|message| match message {
            DaemonMessage::Attached(attached) if attached.request == request => {
                Some(Ok(attached.attachment))
            }
            DaemonMessage::Error(error) if error.request == Some(request) => {
                Some(Err(error.clone()))
            }
            _ => None,
        })
    }

    /// Whether a resume request got either answer.
    pub fn resume_answered(&self, request: RequestId) -> bool {
        self.controls.iter().any(|message| match message {
            DaemonMessage::Resumed(resumed) => resumed.request == request,
            DaemonMessage::Gap(gap) => gap.request == Some(request),
            _ => false,
        })
    }

    /// Whether a resume of the attachment was answered with a gap.
    pub fn resume_refused(&self, attachment: AttachmentId) -> bool {
        self.controls.iter().any(|message| {
            matches!(message, DaemonMessage::Gap(gap)
                if gap.attachment == attachment && gap.request.is_some())
        })
    }

    pub fn error_codes(&self) -> Vec<ErrorCode> {
        self.controls
            .iter()
            .filter_map(|message| match message {
                DaemonMessage::Error(error) => Some(error.code),
                _ => None,
            })
            .collect()
    }

    /// The ownership generation the client last heard of for the attachment.
    pub fn generation(&self, attachment: AttachmentId) -> Option<OwnershipGeneration> {
        self.views.get(&attachment)?.ownership.map(|(g, _)| g)
    }

    /// The acks the policy allows now: the last applied cursor of each attachment that has
    /// applied something since its last ack. `Never` keeps them until the policy changes.
    pub fn take_acks(&mut self) -> Vec<ClientMessage> {
        if self.ack == AckPolicy::Never {
            return Vec::new();
        }
        std::mem::take(&mut self.unacked)
            .into_iter()
            .filter_map(|attachment| self.views.get(&attachment)?.last_applied)
            .map(|cursor| ClientMessage::Ack(Ack { cursor }))
            .collect()
    }

    /// Applies one message from the daemon.
    pub fn apply(&mut self, message: &DaemonMessage) {
        match message {
            DaemonMessage::Welcome(_) => self.controls.push(message.clone()),
            DaemonMessage::Attached(attached) => {
                let terminal = self
                    .attaches
                    .remove(&attached.request)
                    .unwrap_or_else(|| TermId(String::new()));
                self.views.insert(
                    attached.attachment,
                    View::new(attached.attachment, terminal),
                );
                self.controls.push(message.clone());
            }
            DaemonMessage::Resumed(resumed) => {
                if let Some(view) = self.views.get_mut(&resumed.attachment) {
                    view.replay_through = resumed.replay_through;
                }
                self.controls.push(message.clone());
            }
            DaemonMessage::Gap(gap) if gap.request.is_some() => {
                if let Some(view) = self.views.get_mut(&gap.attachment) {
                    view.ending.get_or_insert(Ending::ResumeRefused);
                }
                self.controls.push(message.clone());
            }
            DaemonMessage::Ownership(ownership) => {
                if let Some(view) = self.views.get_mut(&ownership.attachment) {
                    view.ownership = Some((ownership.ownership_generation, ownership.owner));
                }
                self.controls.push(message.clone());
            }
            DaemonMessage::Error(error) => {
                if let Some(request) = error.request {
                    self.attaches.remove(&request);
                }
                self.controls.push(message.clone());
            }
            DaemonMessage::Gap(gap) => self.end_with_gap(gap.attachment),
            event => self.apply_event(event),
        }
    }

    fn end_with_gap(&mut self, attachment: AttachmentId) {
        let Some(view) = self.views.get_mut(&attachment) else {
            self.violations
                .push(format!("a gap for unknown attachment {}", attachment.0));
            return;
        };
        if let Some(ending) = &view.ending {
            self.violations
                .push(format!("a gap after the stream ended with {ending:?}"));
        }
        view.ending = Some(Ending::Gap);
    }

    fn apply_event(&mut self, message: &DaemonMessage) {
        let Some(cursor) = message.cursor() else {
            return;
        };
        let marker = self.measure_marker.clone();
        let keep_raw = self.keep_raw;
        let policy = self.replies;
        let Some(view) = self.views.get_mut(&cursor.attachment) else {
            self.violations.push(format!(
                "an event for unknown attachment {}",
                cursor.attachment.0
            ));
            return;
        };
        if let Some(ending) = &view.ending {
            self.violations.push(format!(
                "{} {cursor:?} after the stream ended with {ending:?}",
                message.type_name()
            ));
        }
        let violation = check_order(view, message, cursor);
        view.last_applied = Some(cursor);
        let mut replies = Vec::new();
        match message {
            DaemonMessage::Started(started) => view.reset_epoch(started),
            DaemonMessage::Data(data) => {
                replies = queries(view, policy, cursor, &data.bytes);
                view.feed(&data.bytes, marker.as_deref(), keep_raw);
            }
            DaemonMessage::Resized(resized) => {
                view.size = (resized.cols, resized.rows);
                view.note(Applied::Resized(resized.cols, resized.rows));
            }
            DaemonMessage::Facts(facts) => {
                view.last_facts = Some(facts.display.clone());
                view.note(Applied::Facts(facts.display.clone()));
            }
            DaemonMessage::Resynced(_) => view.note(Applied::Resynced),
            DaemonMessage::Closed(closed) => {
                view.ending = Some(Ending::Closed(closed.reason));
                view.note(Applied::Closed(closed.reason));
            }
            DaemonMessage::Lost(_) => {
                view.ending = Some(Ending::Lost);
                view.note(Applied::Lost);
            }
            _ => {}
        }
        self.violations.extend(violation);
        self.outgoing.extend(replies);
        self.unacked.insert(cursor.attachment);
    }
}

impl Default for ClientSim {
    fn default() -> Self {
        ClientSim::new()
    }
}

/// The stream invariants: seq is +1 within an epoch, an epoch opens with `Started` at seq 0,
/// and each epoch is the previous one plus 1.
fn check_order(view: &View, message: &DaemonMessage, cursor: Cursor) -> Option<String> {
    let name = message.type_name();
    if matches!(message, DaemonMessage::Started(_)) {
        let expected = view.epoch.map_or(1, |epoch| epoch.0 + 1);
        if cursor.epoch.0 != expected {
            return Some(format!(
                "started opens epoch {}, expected {expected}",
                cursor.epoch.0
            ));
        }
        if cursor.seq.0 != 0 {
            return Some(format!("started has seq {}, expected 0", cursor.seq.0));
        }
        return None;
    }
    if view.epoch != Some(cursor.epoch) {
        return Some(format!(
            "{name} in epoch {} before its started (epoch is {:?})",
            cursor.epoch.0,
            view.epoch.map(|epoch| epoch.0)
        ));
    }
    let last = view.last_applied?;
    if cursor.seq.0 != last.seq.0 + 1 {
        return Some(format!(
            "{name} has seq {} after {}",
            cursor.seq.0, last.seq.0
        ));
    }
    None
}

/// The replies the policy sends for the queries in one `Data` event. The k-th query in the
/// event (1-based) is answered with `ESC [ ? <seq> ; <k> R`.
fn queries(view: &View, policy: ReplyPolicy, cursor: Cursor, bytes: &[u8]) -> Vec<ClientMessage> {
    let live = view
        .replay_through
        .is_none_or(|through| (cursor.epoch.0, cursor.seq.0) > (through.epoch.0, through.seq.0));
    let allowed = match policy {
        ReplyPolicy::Never => false,
        ReplyPolicy::Always => true,
        ReplyPolicy::AsSpec => live && view.owns(),
    };
    if !allowed {
        return Vec::new();
    }
    let generation = view
        .ownership
        .map_or(OwnershipGeneration(0), |(generation, _)| generation);
    let mut found = 0;
    let mut replies = Vec::new();
    for at in bytes
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == 0x1b)
        .map(|(at, _)| at)
    {
        if !bytes[at..].starts_with(QUERY) {
            continue;
        }
        found += 1;
        replies.push(ClientMessage::AutomaticReply(AutomaticReply {
            attachment: cursor.attachment,
            epoch: cursor.epoch,
            ownership_generation: generation,
            bytes: format!("\x1b[?{};{found}R", cursor.seq.0).into_bytes(),
        }));
    }
    replies
}
