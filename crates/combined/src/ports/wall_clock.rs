//! The time of day.

/// The time of day, for what must survive a restart; core never reads it, and the server passes
/// it in as a `u64`.
pub trait WallClock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_unix_ms(&self) -> u64;
}
