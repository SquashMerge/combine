//! Daemon core: spaces, attachment logs, ownership and the driver; no I/O.

mod config;
mod driver;
mod effects;
mod log;
mod machine;
mod ownership;
mod spaces;

pub use config::{ConfigError, CoreConfig, EVENT_OVERHEAD};
pub use driver::Driver;
pub use effects::{BackendCommand, ConnectionId, Effect};
pub use log::{AckError, Appended, EventLog, LogEnd, LogLimits, LogStats, ResumeError};
pub use machine::{Core, OwnershipState};
pub use ownership::{OwnershipChange, TerminalOwnership};
pub use spaces::Spaces;
