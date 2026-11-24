//! Daemon-to-client messages.

use serde::{Deserialize, Serialize};

use crate::codec::floor_char_boundary;
use crate::display::Display;
use crate::error::EncodeError;
use crate::frame::{MAX_DATA_CHUNK, MAX_ERROR_DETAIL};
use crate::ids::{
    AttachmentId, BackendGeneration, Cursor, HostId, OwnershipGeneration, RequestId, Seq, TermId,
};
use crate::layout::LayoutSnapshot;
use crate::mcp::Mcp;
use crate::panels::{PanelError, PanelState, PanelSummary, Renderer};
use crate::spawn::{SkippedPanel, TemplateInfo};
use crate::terminals::{SpaceInfo, TerminalInfo};

/// The daemon's answer to `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    pub protocol_version: u32,
    pub host_id: HostId,
}

/// Answers an `attach`. Sent before the attachment's first `started`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attached {
    pub request: RequestId,
    pub attachment: AttachmentId,
}

/// Answers a `resume`, before the first replayed event. `replay_through` is
/// `null` when nothing is replayed, and everything after it is live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resumed {
    pub request: RequestId,
    pub attachment: AttachmentId,
    pub replay_through: Option<Cursor>,
}

/// The attachment can no longer be served exactly.
///
/// With `request: Some(..)` it answers a resume the daemon can't serve (the
/// log hit its bound, the grace period ended, or the attachment is unknown,
/// for example after a daemon restart) and is control traffic. With
/// `request: None` it is the in-order gap that ends a live stream, after the
/// attachment's last event. After either, the attachment ID is finished and
/// the client attaches fresh.
///
/// It has no cursor: it is never acked or resumed from, and when it answers a
/// resume the daemon may no longer have a log to take a position from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
    pub attachment: AttachmentId,
    pub request: Option<RequestId>,
}

/// Input ownership state. Sent to an attachment after `attached` and after
/// `resumed`, and to every attachment of the terminal on each change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ownership {
    pub attachment: AttachmentId,
    pub ownership_generation: OwnershipGeneration,
    /// `None` means no owner.
    pub owner: Option<AttachmentId>,
}

/// What an [`ErrorMessage`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The hello named another version. The daemon closes.
    VersionMismatch,
    /// The daemon couldn't decode a client frame. The daemon closes.
    Malformed,
    /// Sent in place of `attached`, answering that attach's `request`.
    UnknownTerminal,
    /// The backend couldn't attach. Sent in place of `attached`.
    AttachFailed,
    /// The connection doesn't hold that attachment.
    UnknownAttachment,
    /// A `resize` with an older ownership generation.
    StaleGeneration,
    /// The attachment is `observe`, and the message needs an input half.
    WatchOnly,
    /// A well-formed message the daemon refuses, for example 0 columns.
    InvalidRequest,
    /// A layout request named a space the host doesn't list.
    UnknownSpace,
    /// A `mutate_layout` made against a layout revision that is no longer
    /// current.
    StaleLayout,
    /// The named panel has no usable terminal template.
    UnknownTemplate,
    /// The panel's file changed since the client listed it: its hash no
    /// longer matches the one the request carries. Nothing was started.
    TemplateChanged,
    /// The directory a terminal would start in is refused.
    CwdRefused,
    /// The backend couldn't start the terminal.
    SpawnFailed,
    /// A panel request named a panel the space doesn't have.
    UnknownPanel,
    /// `mcp_open` named a caller that no terminal or space matches.
    UnknownCaller,
    /// Creating a panel would take its space past the panel limit.
    PanelLimit,
    /// Anything else.
    Internal,
}

/// A protocol error, daemon to client only. `request` or `attachment` names
/// what it answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorMessage {
    pub code: ErrorCode,
    pub message: String,
    pub request: Option<RequestId>,
    pub attachment: Option<AttachmentId>,
}

impl ErrorMessage {
    /// The message cut to at most [`MAX_ERROR_DETAIL`] bytes on a char
    /// boundary. A receiver shows or logs this, never the raw text: the text
    /// is a peer's.
    pub fn trimmed_message(&self) -> &str {
        &self.message
            [..floor_char_boundary(&self.message, self.message.len().min(MAX_ERROR_DETAIL))]
    }
}

/// A full snapshot of the host's terminals and spaces. It answers a
/// `watch_terminals` with that `request`, and is pushed with `request: None`
/// after a change, at most once per flush. It is control traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Terminals {
    pub request: Option<RequestId>,
    pub terminals: Vec<TerminalInfo>,
    pub spaces: Vec<SpaceInfo>,
}

/// A space's layout. With `request: Some(..)` it answers a `watch_layout` or
/// an accepted `mutate_layout`; with `request: None` it is a push to the
/// space's watchers, at most once per flush. It is control traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub request: Option<RequestId>,
    pub layout: LayoutSnapshot,
}

/// The terminal templates of a space's panels, answering a `list_templates`
/// with that `request`. `skipped` names the panel folders that offer none.
/// `error` is set when the panels couldn't be listed at all. It is control
/// traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Templates {
    pub request: RequestId,
    pub space: String,
    pub templates: Vec<TemplateInfo>,
    pub skipped: Vec<SkippedPanel>,
    pub error: Option<String>,
}

/// A terminal the daemon started, answering a `spawn_terminal` with that
/// `request`. `terminal` is its ID, such as `t12`. It is control traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spawned {
    pub request: RequestId,
    pub space: String,
    pub terminal: TermId,
}

/// A space's panels, for a connection watching its layout. With
/// `request: Some(..)` it follows the `layout` that answers a `watch_layout`
/// with that request; with `request: None` it is a push after a change, at
/// most once per flush. `panels` is sorted by `id`; `skipped` names the panel
/// folders that offer no panel, sorted by `panel`. `error` is set when the
/// panels couldn't be listed. It is control traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Panels {
    pub request: Option<RequestId>,
    pub space: String,
    pub panels: Vec<PanelSummary>,
    pub skipped: Vec<SkippedPanel>,
    pub error: Option<String>,
}

/// One panel's current revision, answering a `get_panel` with that `request`.
/// `source` is the revision's full text; `error` is set while `state` is
/// `failed`. It is control traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Panel {
    pub request: RequestId,
    pub space: String,
    pub id: String,
    pub revision: u64,
    pub renderer: Renderer,
    pub source: String,
    pub state: PanelState,
    pub error: Option<PanelError>,
}

/// The panel is closed, answering a `close_panel` with that `request`. It is
/// control traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelClosed {
    pub request: RequestId,
    pub space: String,
    pub panel: String,
}

/// The panel is kept in the project, answering a `pin_panel` with that
/// `request`. It is control traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelPinned {
    pub request: RequestId,
    pub space: String,
    pub panel: String,
}

/// The MCP channel is open, answering an `mcp_open` with that `request`.
/// `space` is the space the caller's panels go to. It is control traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpOpened {
    pub request: RequestId,
    pub space: String,
}

/// Whether the backend reproduced the screen exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fidelity {
    Exact,
    Inexact,
}

/// Why an attachment closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    Detached,
    Exited,
}

/// Opens every epoch; its cursor carries the new epoch. `ordered_resizes`
/// sits here because it can change when a backend pair is rebuilt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Started {
    pub cursor: Cursor,
    pub backend_generation: BackendGeneration,
    pub cols: u16,
    pub rows: u16,
    pub fidelity: Fidelity,
    pub ordered_resizes: bool,
}

/// Terminal output: frame kind 2, never JSON. Each frame is its own event with
/// its own `seq`; nothing reassembles split output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Data {
    pub cursor: Cursor,
    pub bytes: Vec<u8>,
}

impl Data {
    /// Cuts `bytes` into events of at most [`MAX_DATA_CHUNK`] bytes, keeping
    /// `first`'s attachment and epoch. Seqs run `first.seq`, `+1`, `+2`, and
    /// so on. Empty input gives no events and uses no seq.
    pub fn split(first: Cursor, bytes: &[u8]) -> Result<Vec<Data>, EncodeError> {
        let mut events = Vec::with_capacity(bytes.len().div_ceil(MAX_DATA_CHUNK));
        let mut seq = first.seq.0;
        for (index, chunk) in bytes.chunks(MAX_DATA_CHUNK).enumerate() {
            if index > 0 {
                seq = seq.checked_add(1).ok_or(EncodeError::SeqOverflow)?;
            }
            events.push(Data {
                cursor: Cursor {
                    seq: Seq(seq),
                    ..first
                },
                bytes: chunk.to_vec(),
            });
        }
        Ok(events)
    }
}

/// The terminal's size changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resized {
    pub cursor: Cursor,
    pub cols: u16,
    pub rows: u16,
}

/// Display facts changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    pub cursor: Cursor,
    pub display: Display,
}

/// The terminal's state was resynchronised.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resynced {
    pub cursor: Cursor,
}

/// The attachment ended: the final event of its stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Closed {
    pub cursor: Cursor,
    pub reason: CloseReason,
}

/// The backend lost the terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lost {
    pub cursor: Cursor,
}

/// Every message a daemon sends. `Data` is a binary frame; the rest are JSON
/// control frames tagged by `type`.
///
/// There is no `Deserialize`: [`WireMessage::decode`](crate::WireMessage::decode)
/// decodes each struct directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMessage {
    Welcome(Welcome),
    Attached(Attached),
    Resumed(Resumed),
    Gap(Gap),
    Ownership(Ownership),
    Error(ErrorMessage),
    Terminals(Terminals),
    Layout(Layout),
    Templates(Templates),
    Spawned(Spawned),
    Panels(Panels),
    Panel(Panel),
    PanelClosed(PanelClosed),
    PanelPinned(PanelPinned),
    McpOpened(McpOpened),
    /// One JSON-RPC message from the daemon's MCP server.
    Mcp(Mcp),
    Started(Started),
    #[serde(skip)]
    Data(Data),
    Resized(Resized),
    Facts(Facts),
    Resynced(Resynced),
    Closed(Closed),
    Lost(Lost),
}

impl DaemonMessage {
    /// The JSON `type` tag, or `"data"` for the binary message.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Welcome(_) => "welcome",
            Self::Attached(_) => "attached",
            Self::Resumed(_) => "resumed",
            Self::Gap(_) => "gap",
            Self::Ownership(_) => "ownership",
            Self::Error(_) => "error",
            Self::Terminals(_) => "terminals",
            Self::Layout(_) => "layout",
            Self::Templates(_) => "templates",
            Self::Spawned(_) => "spawned",
            Self::Panels(_) => "panels",
            Self::Panel(_) => "panel",
            Self::PanelClosed(_) => "panel_closed",
            Self::PanelPinned(_) => "panel_pinned",
            Self::McpOpened(_) => "mcp_opened",
            Self::Mcp(_) => "mcp",
            Self::Started(_) => "started",
            Self::Data(_) => "data",
            Self::Resized(_) => "resized",
            Self::Facts(_) => "facts",
            Self::Resynced(_) => "resynced",
            Self::Closed(_) => "closed",
            Self::Lost(_) => "lost",
        }
    }

    /// The cursor of an attachment event: `Some` for `started`, data,
    /// `resized`, `facts`, `resynced`, `closed` and `lost`, and `None` for
    /// every other message.
    ///
    /// A `gap` has no cursor, including the in-order one that ends a stream.
    /// To tell stream messages from control traffic, use
    /// [`stream_attachment`](Self::stream_attachment).
    pub fn cursor(&self) -> Option<Cursor> {
        match self {
            Self::Started(m) => Some(m.cursor),
            Self::Data(m) => Some(m.cursor),
            Self::Resized(m) => Some(m.cursor),
            Self::Facts(m) => Some(m.cursor),
            Self::Resynced(m) => Some(m.cursor),
            Self::Closed(m) => Some(m.cursor),
            Self::Lost(m) => Some(m.cursor),
            _ => None,
        }
    }

    /// The attachment whose ordered stream carries this message, or `None` for
    /// control traffic.
    ///
    /// The stream is an attachment's seven events, in cursor order, plus the
    /// in-order `gap { request: None }` after the last of them. Control
    /// traffic (`welcome`, `attached`, `resumed`, a `gap` answering a resume,
    /// `ownership`, `error`, `terminals`, `layout`, `templates`, `spawned`,
    /// `panels`, `panel`, `panel_closed`, `panel_pinned`, `mcp_opened` and
    /// `mcp`) is queued ahead of output.
    pub fn stream_attachment(&self) -> Option<AttachmentId> {
        match self {
            Self::Gap(gap) if gap.request.is_none() => Some(gap.attachment),
            _ => self.cursor().map(|cursor| cursor.attachment),
        }
    }
}
