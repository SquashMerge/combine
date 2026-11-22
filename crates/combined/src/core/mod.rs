//! Daemon core: spaces, attachment logs, ownership and the driver; no I/O.

mod config;
mod driver;
mod effects;
mod layout;
mod log;
mod machine;
mod ownership;
mod spaces;

pub use config::{ConfigError, CoreConfig, EVENT_OVERHEAD};
pub use driver::Driver;
pub use effects::{BackendCommand, ConnectionId, Effect, RefusedCommand};
pub use layout::{
    apply_ops, default_layout, diff_fields, encode_layout, identity_origins, parse_layout,
    touched_with_origins, validate_layout, AppliedOps, FieldPath, LayoutError, LayoutOp, Origins,
    SplitKey, LAYOUT_MAX_BYTES, LAYOUT_MAX_DEPTH,
};
pub use log::{AckError, Appended, EventLog, LogEnd, LogLimits, LogStats, ResumeError};
pub use machine::{Core, OwnershipState};
pub use ownership::{OwnershipChange, TerminalOwnership};
pub use spaces::Spaces;
