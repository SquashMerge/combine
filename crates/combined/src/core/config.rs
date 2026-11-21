//! Core's tunable bounds and the checks that make them safe to run with.

use std::fmt;

use combine_protocol::MAX_DATA_CHUNK;

use crate::model::Millis;
use crate::ports::Capabilities;

/// Bytes charged per log entry on top of its payload; also the cost of the final gap.
pub const EVENT_OVERHEAD: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreConfig {
    /// The most bytes one attachment's log accounts for: retained entries, outstanding credit
    /// and the gap reservation.
    pub log_max_bytes: usize,
    /// The most entries one log holds, counting the gap's slot.
    pub log_max_events: usize,
    /// Bytes of the log that credit is never granted from.
    pub credit_reserve: usize,
    /// The smallest credit grant; below this much room, nothing is granted.
    pub credit_min_grant: u32,
    /// How long an unbound attachment is kept for a resume.
    pub grace: Millis,
    /// How long a bound client may leave the final gap unsent before it is disconnected.
    pub gap_timeout: Millis,
    /// Control messages queued per connection before it is disconnected.
    pub max_control_messages: usize,
    /// The first value of the counter that issues attachment IDs and ownership generations.
    pub id_base: u64,
}

impl Default for CoreConfig {
    fn default() -> Self {
        CoreConfig {
            log_max_bytes: 4_194_304,
            log_max_events: 16_384,
            credit_reserve: 65_536,
            credit_min_grant: 65_536,
            grace: Millis(30_000),
            gap_timeout: Millis(10_000),
            max_control_messages: 1_024,
            id_base: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// With byte credit, the log can't hold the initial credit, the reserve and two entries.
    LogTooSmall { needed: usize, max_bytes: usize },
    /// The reserve is under twice the bytes of the data margin.
    ReserveTooSmall { needed: usize },
    /// The log holds fewer than twice the events of the data margin.
    TooFewEvents { needed: usize },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::LogTooSmall { needed, max_bytes } => {
                write!(f, "log of {max_bytes} bytes is under the {needed} needed")
            }
            ConfigError::ReserveTooSmall { needed } => {
                write!(f, "credit reserve is under the {needed} bytes needed")
            }
            ConfigError::TooFewEvents { needed } => {
                write!(f, "event limit is under the {needed} needed")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// The `(bytes, events)` a log keeps free for the entries that credit already granted may still
/// create: two more than the full-size chunks that fit in the log, at the overhead each.
pub(crate) fn data_margin(log_max_bytes: usize) -> (usize, usize) {
    let chunks = log_max_bytes.div_ceil(MAX_DATA_CHUNK) + 2;
    (EVENT_OVERHEAD * chunks, chunks)
}

/// Checks `config` against what the backend's credit scheme needs.
pub(crate) fn validate(
    config: &CoreConfig,
    capabilities: &Capabilities,
) -> Result<(), ConfigError> {
    let (margin_bytes, margin_events) = data_margin(config.log_max_bytes);
    if config.credit_reserve < 2 * margin_bytes {
        return Err(ConfigError::ReserveTooSmall {
            needed: 2 * margin_bytes,
        });
    }
    if config.log_max_events / 2 < margin_events {
        return Err(ConfigError::TooFewEvents {
            needed: 2 * margin_events,
        });
    }
    if capabilities.byte_credit {
        let needed =
            capabilities.initial_credit as usize + config.credit_reserve + 2 * EVENT_OVERHEAD;
        if config.log_max_bytes < needed {
            return Err(ConfigError::LogTooSmall {
                needed,
                max_bytes: config.log_max_bytes,
            });
        }
    }
    Ok(())
}
