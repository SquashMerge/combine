//! A space's panels on the wire: the entries of `panels`, a panel's state,
//! renderer and error, and the render status a client reports.
//!
//! Panel and space IDs are opaque strings the daemon validates, not the codec.

use serde::{Deserialize, Serialize};

/// How a panel's source is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Renderer {
    Mermaid,
    Markdown,
}

/// Where a panel's revision is in the daemon's checks. `validated_parser_only`
/// means only a parser checked the source; `untrusted` waits for the user's
/// trust approval; `removed` is the state of a closed panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelState {
    Validating,
    Fetching,
    Validated,
    ValidatedParserOnly,
    Failed,
    Untrusted,
    Removed,
}

/// Whether a client drew a panel's revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderStatus {
    Rendered,
    RenderFailed,
}

/// Why a panel's revision failed a check or failed to draw. `code` is short
/// and machine-readable; `message` is for people. `line` and `column` locate
/// the error in the source, counted from 1, and `snippet` is the source text
/// there; each is `None` when the check doesn't know it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelError {
    pub code: String,
    pub message: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub snippet: Option<String>,
}

/// Who made a panel: the user, or an agent in a terminal slot (`slot` is
/// `None` when it isn't known). Repo files name slots, never terminals.
///
/// It decodes through a flat struct, like [`SpawnSource`](crate::SpawnSource).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", try_from = "RawCreatedBy")]
pub enum CreatedBy {
    User,
    Agent { slot: Option<String> },
}

#[derive(Deserialize)]
struct RawCreatedBy {
    #[serde(rename = "type")]
    kind: String,
    slot: Option<String>,
}

impl TryFrom<RawCreatedBy> for CreatedBy {
    type Error = String;

    fn try_from(raw: RawCreatedBy) -> Result<CreatedBy, String> {
        match raw.kind.as_str() {
            "user" => Ok(CreatedBy::User),
            "agent" => Ok(CreatedBy::Agent { slot: raw.slot }),
            other => Err(format!("unknown creator `{}`", clip(other))),
        }
    }
}

fn clip(text: &str) -> &str {
    &text[..crate::codec::floor_char_boundary(text, text.len().min(64))]
}

/// One panel of a space, as `panels` lists it: its current revision, and that
/// revision's state. `error` is set while `state` is `failed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelSummary {
    pub id: String,
    pub title: Option<String>,
    pub renderer: Renderer,
    pub revision: u64,
    pub state: PanelState,
    pub error: Option<PanelError>,
    pub created_by: Option<CreatedBy>,
}
