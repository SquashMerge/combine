//! Daemon core: spaces, attachment logs, ownership and the driver; no I/O.

mod caller;
mod config;
mod driver;
mod effects;
mod layout;
mod layout_sync;
mod log;
mod machine;
mod ownership;
mod panel_file;
mod panel_store;
mod panel_wire;
mod panels;
mod render_status;
mod spaces;
mod spawn;
mod terminal_list;

pub use caller::{resolve_caller, CallerMatch, ResolvedCaller};
pub use config::{ConfigError, CoreConfig, EVENT_OVERHEAD};
pub use driver::{Driver, MAX_QUEUED_COMMANDS};
pub use effects::{BackendCommand, ConnectionId, Effect, RefusedCommand};
pub use layout::{
    apply_ops, default_layout, diff_fields, encode_layout, identity_origins, parse_layout,
    touched_with_origins, validate_layout, AppliedOps, FieldPath, LayoutError, LayoutOp, Origins,
    SplitKey, LAYOUT_MAX_BYTES, LAYOUT_MAX_DEPTH,
};
pub use layout_sync::{
    layout_snapshot, ops_from_wire, DroppedBatch, LayoutEffect, LayoutRead, LayoutReply,
    LayoutTiming, LayoutWritten, Layouts, MutateError, SpaceLayoutStatus, WatchError, LAYOUT_FILE,
    MAX_WIRE_OPS,
};
pub use log::{AckError, Appended, EventLog, LogEnd, LogLimits, LogStats, ResumeError};
pub use machine::{Core, OwnershipState};
pub use ownership::{OwnershipChange, TerminalOwnership};
pub use panel_file::{
    clip_text, content_file, created_by_refusal, encode_scratch_meta, encode_view_panel,
    panel_file_bound, panel_reply_len, parse_scratch_meta, parse_view_panel, valid_idempotency_key,
    valid_placement, valid_slot, valid_title, ScratchMeta, CONTENT_READ_MAX_BYTES, DS_STORE,
    IDEMPOTENCY_KEY_MAX_BYTES, MARKDOWN_FILE, MERMAID_FILE, PANEL_ERROR_TEXT_MAX, SCRATCH_FILE,
    SCRATCH_FILE_MAX_BYTES, SOURCE_MAX_BYTES, TITLE_MAX_BYTES,
};
pub use panel_store::{
    FileWrite, IdSource, PanelApiError, PanelCreate, PanelEffect, PanelEvent, PanelFolder,
    PanelList, PanelStore, PanelTiming, PanelUpdate, PanelView, SaveEnd, SaveReport, SaveTicket,
    MAX_CHECKS_IN_FLIGHT, REMOVED_IDS_MAX, SCRATCH_ID_OPEN, SCRATCH_LIST_MAX, SCRATCH_TTL_MS,
};
pub use panel_wire::{
    panel_reply, panel_summary, panels_loading, panels_message, PANELS_LOADING, SKIPPED_TRIMMED,
    TRIMMED_REASON,
};
pub use panels::{
    normalize_cwd, panel_folder_refusal, parse_terminal_panel, shell_spec, template_info,
    template_spec, valid_panel_folder, CwdRefusal, PanelSkip, TerminalTemplate, DEFAULT_COLS,
    DEFAULT_ROWS, MAX_PANELS, PANELS_DIR, PANEL_FILE, PANEL_ID_MAX_BYTES, PANEL_LIST_MAX,
    PANEL_MAX_BYTES, REASON_MAX_BYTES,
};
pub use render_status::{
    ClientRender, RenderStatuses, RENDER_ERROR_CODE_MAX, RENDER_STATUS_MAX_PER_CONNECTION,
};
pub use spaces::Spaces;
pub use spawn::{PanelRead, SpawnEffect, SpawnTicket, Spawns, MAX_SPAWN_IN_FLIGHT};
pub use terminal_list::TerminalList;
