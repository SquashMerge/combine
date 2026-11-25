//! The files of a view panel: `panel.json`, the content file and `scratch.json`. Pure.

use combine_protocol::{
    CreatedBy, DaemonMessage, EncodeError, Panel, PanelError, PanelState, Renderer, RequestId,
    WireMessage, MAX_FRAME_LEN,
};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::core::panels::{PanelSkip, PANEL_FILE, PANEL_MAX_BYTES, SLOT_RULE};
use crate::model::{PanelId, SpaceId, TileId, ViewPanelFile};

pub const MERMAID_FILE: &str = "diagram.mmd";
pub const MARKDOWN_FILE: &str = "content.md";
pub const SCRATCH_FILE: &str = "scratch.json";
pub const DS_STORE: &str = ".DS_Store";
/// The largest source the daemon validates.
pub const SOURCE_MAX_BYTES: usize = 204_800;
/// The bound a content file is read with: a source this big is still hashed, so the echo of a
/// daemon write is ignored.
pub const CONTENT_READ_MAX_BYTES: usize = MAX_FRAME_LEN as usize;
pub const PANEL_ERROR_TEXT_MAX: usize = 1_024;
pub const TITLE_MAX_BYTES: usize = 1_024;
pub const IDEMPOTENCY_KEY_MAX_BYTES: usize = 256;
pub const SCRATCH_FILE_MAX_BYTES: usize = 8_192;

pub(crate) const TITLE_RULE: &str =
    "must be a string of 1 to 1024 bytes with no control characters";
pub(crate) const KEY_RULE: &str = "must be a string of 1 to 256 bytes with no control characters";
const RENDERER_RULE: &str = "must be \"mermaid\" or \"markdown\"";
const CREATED_BY_RULE: &str =
    "must be {\"type\":\"user\"} or {\"type\":\"agent\",\"slot\":<slot or null>}";

/// The content file of a panel drawn by `renderer`.
pub fn content_file(renderer: Renderer) -> &'static str {
    match renderer {
        Renderer::Mermaid => MERMAID_FILE,
        Renderer::Markdown => MARKDOWN_FILE,
    }
}

/// The bound a read of `file` is given.
pub fn panel_file_bound(file: &str) -> usize {
    if file == PANEL_FILE || file == SCRATCH_FILE {
        PANEL_MAX_BYTES as usize
    } else {
        CONTENT_READ_MAX_BYTES
    }
}

/// `text` cut to at most `max` bytes on a character boundary.
pub fn clip_text(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn text_ok(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
}

/// Whether `title` is 1 to 1,024 bytes with no control character.
pub fn valid_title(title: &str) -> bool {
    text_ok(title, TITLE_MAX_BYTES)
}

/// Whether `key` is 1 to 256 bytes with no control character.
pub fn valid_idempotency_key(key: &str) -> bool {
    text_ok(key, IDEMPOTENCY_KEY_MAX_BYTES)
}

/// Whether `slot` is a valid agent slot.
pub fn valid_slot(slot: &str) -> bool {
    PanelId::new(slot).is_ok()
}

/// The rule `created_by` breaks, as `(field, rule)`.
pub fn created_by_refusal(created_by: &CreatedBy) -> Option<(&'static str, &'static str)> {
    match created_by {
        CreatedBy::Agent { slot: Some(slot) } if !valid_slot(slot) => {
            Some(("created_by.slot", SLOT_RULE))
        }
        CreatedBy::Agent { .. } | CreatedBy::User => None,
    }
}

fn field(path: &'static str, rule: &'static str) -> PanelSkip {
    PanelSkip::Field { path, rule }
}

fn optional_text(
    root: &Map<String, Value>,
    key: &'static str,
    rule: &'static str,
    ok: fn(&str) -> bool,
) -> Result<Option<String>, PanelSkip> {
    match root.get(key) {
        None => Ok(None),
        Some(value) => match value.as_str() {
            Some(text) if ok(text) => Ok(Some(text.to_owned())),
            Some(_) | None => Err(field(key, rule)),
        },
    }
}

pub(crate) const PLACEMENT_RULE: &str =
    "must be a tile ID of 1 to 256 bytes with no control characters";

/// Whether `tile` can name a layout tile.
pub fn valid_placement(tile: &str) -> bool {
    TileId::new(tile).is_ok()
}

/// Reads one `panel.json` as a view panel.
pub fn parse_view_panel(folder: &str, bytes: &[u8]) -> Result<ViewPanelFile, PanelSkip> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| PanelSkip::BadJson(error.to_string()))?;
    let root = value.as_object().ok_or(PanelSkip::NotObject)?;
    let Some(renderer) = root.get("renderer") else {
        return Err(PanelSkip::NotView);
    };
    match root.get("version") {
        None => return Err(PanelSkip::NoVersion),
        Some(version) if version.as_u64() == Some(1) => {}
        Some(other) => return Err(PanelSkip::Version(other.to_string())),
    }
    if let Some(id) = root.get("id") {
        if id.as_str() != Some(folder) {
            return Err(PanelSkip::IdMismatch);
        }
    }
    let renderer = match renderer.as_str() {
        Some("mermaid") => Renderer::Mermaid,
        Some("markdown") => Renderer::Markdown,
        Some(_) | None => return Err(field("renderer", RENDERER_RULE)),
    };
    let title = optional_text(root, "title", TITLE_RULE, valid_title)?;
    let idempotency_key = optional_text(root, "idempotency_key", KEY_RULE, valid_idempotency_key)?;
    let created_by = match root.get("created_by") {
        None => None,
        Some(raw) => {
            let decoded: CreatedBy = serde_json::from_value(raw.clone())
                .map_err(|_| field("created_by", CREATED_BY_RULE))?;
            if let Some((path, rule)) = created_by_refusal(&decoded) {
                return Err(field(path, rule));
            }
            Some(decoded)
        }
    };
    let placement = optional_text(root, "placement", PLACEMENT_RULE, valid_placement)?;
    Ok(ViewPanelFile {
        renderer,
        title,
        idempotency_key,
        created_by,
        placement,
    })
}

#[derive(Serialize)]
struct ViewPanelJson<'a> {
    version: u8,
    renderer: Renderer,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    idempotency_key: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_by: Option<&'a CreatedBy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    placement: Option<&'a str>,
}

fn pretty(value: &impl Serialize) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

/// The bytes of `panel.json`: pretty JSON with a trailing newline. It writes only the fields of
/// a [`ViewPanelFile`]. The worst case is 3,747 bytes, within [`PANEL_MAX_BYTES`].
pub fn encode_view_panel(file: &ViewPanelFile) -> Vec<u8> {
    pretty(&ViewPanelJson {
        version: 1,
        renderer: file.renderer,
        title: file.title.as_deref(),
        idempotency_key: file.idempotency_key.as_deref(),
        created_by: file.created_by.as_ref(),
        placement: file.placement.as_deref(),
    })
}

/// What `scratch.json` holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchMeta {
    pub space: SpaceId,
    pub changed_at_ms: u64,
}

/// Reads the `scratch.json` of the folder under space key `key`.
pub fn parse_scratch_meta(key: &str, bytes: &[u8]) -> Result<ScratchMeta, String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("{SCRATCH_FILE} isn't valid JSON: {error}"))?;
    let root = value
        .as_object()
        .ok_or_else(|| format!("{SCRATCH_FILE} isn't a JSON object"))?;
    match root.get("version") {
        None => return Err(format!("{SCRATCH_FILE} has no version")),
        Some(version) if version.as_u64() == Some(1) => {}
        Some(other) => {
            return Err(format!(
                "unsupported version {other}; this daemon reads version 1"
            ))
        }
    }
    let space = root
        .get("space")
        .and_then(Value::as_str)
        .filter(|space| space.starts_with('/'))
        .ok_or_else(|| "space must be an absolute folder path".to_owned())?;
    let changed_at_ms = root
        .get("changed_at_ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| "changed_at_ms must be an integer".to_owned())?;
    let space = SpaceId(space.to_owned());
    if space.sha256_hex() != key {
        return Err(format!("{SCRATCH_FILE} names another space"));
    }
    Ok(ScratchMeta {
        space,
        changed_at_ms,
    })
}

#[derive(Serialize)]
struct ScratchMetaJson<'a> {
    version: u8,
    space: &'a str,
    changed_at_ms: u64,
}

/// The bytes of `scratch.json`: pretty JSON with a trailing newline.
pub fn encode_scratch_meta(space: &SpaceId, changed_at_ms: u64) -> Vec<u8> {
    pretty(&ScratchMetaJson {
        version: 1,
        space: &space.0,
        changed_at_ms,
    })
}

/// The length of the `panel` frame (kind byte and payload) that would carry `source` for this
/// panel at its worst: the longest code, the largest numbers and a full-size, fully escaped
/// error. A source fits one frame when this is at most `MAX_FRAME_LEN`.
pub fn panel_reply_len(space: &SpaceId, id: &str, renderer: Renderer, source: &str) -> usize {
    let reply = DaemonMessage::Panel(Panel {
        request: RequestId(u64::MAX),
        space: space.0.clone(),
        id: id.to_owned(),
        revision: u64::MAX,
        renderer,
        source: source.to_owned(),
        state: PanelState::ValidatedParserOnly,
        error: Some(PanelError {
            code: "unsupported_diagram".to_owned(),
            message: "\u{1}".repeat(PANEL_ERROR_TEXT_MAX),
            line: Some(u32::MAX),
            column: Some(u32::MAX),
            snippet: Some("\u{1}".repeat(PANEL_ERROR_TEXT_MAX)),
        }),
    });
    match reply.encode() {
        Ok(frame) => frame.len().saturating_sub(4),
        Err(EncodeError::FrameTooLarge { length, .. }) => length,
        Err(EncodeError::SeqOverflow | EncodeError::Json { .. }) => usize::MAX,
    }
}
