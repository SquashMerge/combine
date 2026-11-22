//! The daemon's clock: monotonic milliseconds since the daemon started.

use std::time::{Duration, Instant};

use crate::model::Millis;

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
