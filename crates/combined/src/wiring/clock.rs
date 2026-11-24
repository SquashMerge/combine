//! The daemon's clocks: monotonic milliseconds since the daemon started, and the time of day.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::model::Millis;
use crate::ports::WallClock;

#[derive(Debug, Clone, Copy)]
pub struct MonotonicClock {
    origin: Instant,
}

impl MonotonicClock {
    pub fn new() -> MonotonicClock {
        MonotonicClock {
            origin: Instant::now(),
        }
    }

    /// Whole milliseconds since the clock started.
    pub fn now(&self) -> Millis {
        Millis(u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX))
    }

    /// The instant at which `now()` reaches `at`, for timer deadlines.
    pub fn instant_at(&self, at: Millis) -> Instant {
        self.origin + Duration::from_millis(at.0)
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        MonotonicClock::new()
    }
}

/// The system's time of day, for what must survive a restart.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    /// Milliseconds since the Unix epoch, 0 for a time before it.
    fn now_unix_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            })
    }
}
