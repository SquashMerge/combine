//! Terminal panels: reading `panel.json`, normalizing a template's `cwd`, and building the spawn
//! specs. Pure; resolving symlinks is the `ProjectDirs` port's job.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use combine_protocol as wire;
use serde_json::{Map, Value};

use crate::model::{ContentHash, PanelId, SpaceId, SpawnSpec};
use crate::ports::DirRefusal;

pub const PANELS_DIR: &str = "panels";
pub const PANEL_FILE: &str = "panel.json";
pub const PANEL_MAX_BYTES: u64 = 8_192;
pub const MAX_PANELS: usize = 100;
pub const PANEL_LIST_MAX: usize = 256;
pub const DEFAULT_COLS: u16 = 80;
pub const DEFAULT_ROWS: u16 = 24;
pub const REASON_MAX_BYTES: usize = 256;

const FOLDER_RULE: &str = "it can't contain / or start with .";
const SLOT_RULE: &str = "must be 1 to 256 bytes with no control characters";
const ARGV_RULE: &str = "must be a non-empty list of strings with no NUL and a non-empty first";
const ENV_RULE: &str =
    "must be an object of strings with non-empty keys without = or NUL and values without NUL";
const SIZE_RULE: &str = "must be an integer from 1 to 65535";

/// Why a panel folder name or spawn-request panel ID is refused, or `None` when it is valid.
pub fn panel_folder_refusal(name: &str) -> Option<String> {
    if let Err(invalid) = PanelId::new(name) {
        return Some(invalid.to_string());
    }
    if name.contains('/') || name.starts_with('.') {
        return Some(FOLDER_RULE.to_owned());
    }
    None
}

pub fn valid_panel_folder(name: &str) -> bool {
    panel_folder_refusal(name).is_none()
}

/// The terminal template of one panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalTemplate {
    pub panel: String,
    pub title: Option<String>,
    pub slot: String,
    pub argv: Vec<String>,
    /// Relative to the project root, normalized; `""` is the root.
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub labels: BTreeMap<String, String>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CwdRefusal {
    Absolute,
    ParentSegment,
    GitSegment,
    Nul,
    Dir(DirRefusal),
}

impl fmt::Display for CwdRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CwdRefusal::Absolute => {
                f.write_str("is absolute; it must be relative to the project root")
            }
            CwdRefusal::ParentSegment => f.write_str("has a .. segment"),
            CwdRefusal::GitSegment => f.write_str("is inside .git/"),
            CwdRefusal::Nul => f.write_str("contains a NUL byte"),
            CwdRefusal::Dir(DirRefusal::Missing) => f.write_str("doesn't exist"),
            CwdRefusal::Dir(DirRefusal::NotADirectory) => f.write_str("isn't a directory"),
            CwdRefusal::Dir(DirRefusal::Outside) => f.write_str("resolves outside the project"),
            CwdRefusal::Dir(DirRefusal::InsideGit) => f.write_str("resolves inside .git/"),
            CwdRefusal::Dir(DirRefusal::Unavailable(detail)) => {
                write!(f, "couldn't be checked ({detail})")
            }
        }
    }
}

/// The lexical half of the `cwd` rules: relative only, no `..`, no `.git` segment. `""` and `"."`
/// both give `""`.
pub fn normalize_cwd(raw: &str) -> Result<String, CwdRefusal> {
    if raw.contains('\0') {
        return Err(CwdRefusal::Nul);
    }
    if raw.starts_with('/') {
        return Err(CwdRefusal::Absolute);
    }
    let segments: Vec<&str> = raw
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect();
    if segments.contains(&"..") {
        return Err(CwdRefusal::ParentSegment);
    }
    if segments
        .iter()
        .any(|segment| segment.eq_ignore_ascii_case(".git"))
    {
        return Err(CwdRefusal::GitSegment);
    }
    Ok(segments.join("/"))
}

/// Why a panel folder isn't offered as a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelSkip {
    NoFile,
    Symlink,
    TooLarge {
        size: u64,
    },
    ReadFailed(String),
    BadJson(String),
    NotObject,
    NoVersion,
    Version(String),
    IdMismatch,
    NotTerminal,
    Field {
        path: &'static str,
        rule: &'static str,
    },
    Cwd {
        cwd: String,
        refusal: CwdRefusal,
    },
}

impl fmt::Display for PanelSkip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PanelSkip::NoFile => write!(f, "no {PANEL_FILE}"),
            PanelSkip::Symlink => {
                f.write_str("the panel folder is a symlink; Combine doesn't follow it")
            }
            PanelSkip::TooLarge { size } => {
                write!(
                    f,
                    "{PANEL_FILE} is {size} bytes; the limit is {PANEL_MAX_BYTES}"
                )
            }
            PanelSkip::ReadFailed(detail) => write!(f, "couldn't read {PANEL_FILE} ({detail})"),
            PanelSkip::BadJson(detail) => write!(f, "{PANEL_FILE} isn't valid JSON: {detail}"),
            PanelSkip::NotObject => write!(f, "{PANEL_FILE} isn't a JSON object"),
            PanelSkip::NoVersion => write!(f, "{PANEL_FILE} has no version"),
            PanelSkip::Version(version) => {
                write!(
                    f,
                    "unsupported version {version}; this daemon reads version 1"
                )
            }
            PanelSkip::IdMismatch => f.write_str("its id doesn't match the folder name"),
            PanelSkip::NotTerminal => f.write_str("not a terminal panel"),
            PanelSkip::Field { path, rule } => write!(f, "{path} {rule}"),
            PanelSkip::Cwd { cwd, refusal } => write!(f, "cwd \"{cwd}\" {refusal}"),
        }
    }
}

impl PanelSkip {
    /// The Display text cut to [`REASON_MAX_BYTES`] on a character boundary.
    pub fn reason(&self) -> String {
        let text = self.to_string();
        let mut end = text.len().min(REASON_MAX_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text[..end].to_owned()
    }
}

fn field(path: &'static str, rule: &'static str) -> PanelSkip {
    PanelSkip::Field { path, rule }
}

fn string_map(
    value: Option<&Value>,
    path: &'static str,
    rule: &'static str,
    entry_ok: impl Fn(&str, &str) -> bool,
) -> Result<BTreeMap<String, String>, PanelSkip> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value.as_object().ok_or_else(|| field(path, rule))?;
    let mut map = BTreeMap::new();
    for (key, entry) in object {
        match entry.as_str() {
            Some(text) if entry_ok(key, text) => {
                map.insert(key.clone(), text.to_owned());
            }
            _ => return Err(field(path, rule)),
        }
    }
    Ok(map)
}

fn size(
    object: &Map<String, Value>,
    key: &str,
    path: &'static str,
    default: u16,
) -> Result<u16, PanelSkip> {
    match object.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|n| (1..=u64::from(u16::MAX)).contains(n))
            .and_then(|n| u16::try_from(n).ok())
            .ok_or_else(|| field(path, SIZE_RULE)),
    }
}

/// Reads one `panel.json` as a terminal template.
pub fn parse_terminal_panel(panel: &str, bytes: &[u8]) -> Result<TerminalTemplate, PanelSkip> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| PanelSkip::BadJson(error.to_string()))?;
    let root = value.as_object().ok_or(PanelSkip::NotObject)?;

    match root.get("version") {
        None => return Err(PanelSkip::NoVersion),
        Some(version) if version.as_u64() == Some(1) => {}
        Some(other) => return Err(PanelSkip::Version(other.to_string())),
    }
    if let Some(id) = root.get("id") {
        if id.as_str() != Some(panel) {
            return Err(PanelSkip::IdMismatch);
        }
    }
    let title = match root.get("title") {
        None => None,
        Some(title) => Some(
            title
                .as_str()
                .ok_or_else(|| field("title", "must be a string"))?
                .to_owned(),
        ),
    };

    let content = root
        .get("content")
        .and_then(Value::as_object)
        .ok_or_else(|| field("content", "must be an object"))?;
    if content.get("type").and_then(Value::as_str) != Some("terminal") {
        return Err(PanelSkip::NotTerminal);
    }
    let slot = content
        .get("slot")
        .and_then(Value::as_str)
        .filter(|slot| PanelId::new(*slot).is_ok())
        .ok_or_else(|| field("content.slot", SLOT_RULE))?
        .to_owned();
    let template = content
        .get("template")
        .and_then(Value::as_object)
        .ok_or_else(|| field("content.template", "must be an object"))?;

    let argv = template
        .get("argv")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .filter(|s| !s.contains('\0'))
                        .map(str::to_owned)
                })
                .collect::<Option<Vec<String>>>()
        })
        .filter(|argv| argv.first().is_some_and(|first| !first.is_empty()))
        .ok_or_else(|| field("content.template.argv", ARGV_RULE))?;

    let cwd = match template.get("cwd") {
        None => String::new(),
        Some(raw) => {
            let raw = raw
                .as_str()
                .ok_or_else(|| field("content.template.cwd", "must be a string"))?;
            normalize_cwd(raw).map_err(|refusal| PanelSkip::Cwd {
                cwd: raw.to_owned(),
                refusal,
            })?
        }
    };

    let env = string_map(
        template.get("env"),
        "content.template.env",
        ENV_RULE,
        |key, value| !key.is_empty() && !key.contains(['=', '\0']) && !value.contains('\0'),
    )?;
    let labels = string_map(
        template.get("labels"),
        "content.template.labels",
        "must be an object of strings",
        |_, _| true,
    )?;
    let cols = size(template, "cols", "content.template.cols", DEFAULT_COLS)?;
    let rows = size(template, "rows", "content.template.rows", DEFAULT_ROWS)?;

    Ok(TerminalTemplate {
        panel: panel.to_owned(),
        title,
        slot,
        argv,
        cwd,
        env,
        labels,
        cols,
        rows,
    })
}

/// What starts the template's terminal. The cwd is the lexical path under the root, never the
/// canonical one.
pub fn template_spec(root: &SpaceId, template: &TerminalTemplate) -> SpawnSpec {
    let cwd = if template.cwd.is_empty() {
        root.0.clone()
    } else {
        Path::new(&root.0)
            .join(&template.cwd)
            .to_string_lossy()
            .into_owned()
    };
    SpawnSpec {
        argv: template.argv.clone(),
        cwd,
        env: template.env.clone(),
        labels: template.labels.clone(),
        cols: template.cols,
        rows: template.rows,
    }
}

/// The user's shell in the project root.
pub fn shell_spec(root: &SpaceId) -> SpawnSpec {
    SpawnSpec {
        argv: Vec::new(),
        cwd: root.0.clone(),
        env: BTreeMap::new(),
        labels: BTreeMap::new(),
        cols: DEFAULT_COLS,
        rows: DEFAULT_ROWS,
    }
}

/// What a listing offers for `template`; `hash` is the hash of the bytes it was parsed from.
pub fn template_info(template: &TerminalTemplate, hash: &ContentHash) -> wire::TemplateInfo {
    wire::TemplateInfo {
        panel: template.panel.clone(),
        title: template.title.clone(),
        slot: template.slot.clone(),
        argv: template.argv.clone(),
        env: template.env.clone(),
        cwd: template.cwd.clone(),
        hash: hash.to_hex(),
    }
}
