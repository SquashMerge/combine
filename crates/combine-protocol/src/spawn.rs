//! What a client asks to start in a space, and what the daemon offers: the
//! nested types of the `spawn_terminal` and `templates` messages.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What `spawn_terminal` starts: the user's shell in the project folder, or
/// the terminal template of one panel. A template source carries the `hash`
/// of the definition the client was shown, from its [`TemplateInfo`].
///
/// It decodes through a flat struct, like [`LayoutNode`](crate::LayoutNode).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", try_from = "RawSource")]
pub enum SpawnSource {
    Shell,
    Template { panel: String, hash: String },
}

#[derive(Deserialize)]
struct RawSource {
    #[serde(rename = "type")]
    kind: String,
    panel: Option<String>,
    hash: Option<String>,
}

impl TryFrom<RawSource> for SpawnSource {
    type Error = String;

    fn try_from(raw: RawSource) -> Result<SpawnSource, String> {
        match raw.kind.as_str() {
            "shell" => Ok(SpawnSource::Shell),
            "template" => Ok(SpawnSource::Template {
                panel: raw
                    .panel
                    .ok_or_else(|| "`template` is missing `panel`".to_owned())?,
                hash: raw
                    .hash
                    .ok_or_else(|| "`template` is missing `hash`".to_owned())?,
            }),
            other => Err(format!("unknown spawn source `{}`", clip(other))),
        }
    }
}

fn clip(text: &str) -> &str {
    &text[..crate::codec::floor_char_boundary(text, text.len().min(64))]
}

/// One panel's terminal template, as the daemon read it from `panel.json`.
/// `cwd` is relative to the project root, with `""` for the root itself.
/// `hash` is the lowercase hex SHA-256 of the exact `panel.json` bytes the
/// daemon read; a `spawn_terminal` for this template sends it back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateInfo {
    pub panel: String,
    pub title: Option<String>,
    pub slot: String,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: String,
    pub hash: String,
}

/// A panel folder the daemon couldn't offer, and why: as a terminal template in
/// `templates`, or as a panel in `panels`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedPanel {
    pub panel: String,
    pub reason: String,
}
