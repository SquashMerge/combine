//! Daemon core: spaces, attachment logs, ownership and the driver; no I/O.

mod config;
mod driver;
mod effects;
mod layout;
mod layout_sync;
mod log;
mod machine;
mod ownership;
mod panels;
mod spaces;
mod spawn;
mod terminal_list;

pub use config::{ConfigError, CoreConfig, EVENT_OVERHEAD};
pub use driver::Driver;
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
pub use panels::{
    normalize_cwd, panel_folder_refusal, parse_terminal_panel, shell_spec, template_info,
    template_spec, valid_panel_folder, CwdRefusal, PanelSkip, TerminalTemplate, DEFAULT_COLS,
    DEFAULT_ROWS, MAX_PANELS, PANELS_DIR, PANEL_FILE, PANEL_LIST_MAX, PANEL_MAX_BYTES,
    REASON_MAX_BYTES,
};
pub use spaces::Spaces;
pub use spawn::{PanelRead, SpawnEffect, SpawnTicket, Spawns, MAX_SPAWN_IN_FLIGHT};
pub use terminal_list::TerminalList;
