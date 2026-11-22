//! When the next flush pass may run: a leading-edge throttle in the daemon's own milliseconds.

use std::time::Duration;

use crate::model::Millis;

/// What a flush request should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushAt {
    /// Flush now.
    Now,
    /// Not before this time. Ask again then: the answer may still be `At`.
    At(Millis),
}

/// Flushes at once when the last pass was `interval` or more ago, otherwise at `last + interval`.
/// It reads no clock: the caller passes the time in, so no two passes are ever closer than
/// `interval` in the caller's clock.
#[derive(Debug, Clone)]
pub struct FlushSchedule {
    interval: u64,
    last: Option<Millis>,
}

impl FlushSchedule {
    pub fn new(interval: Duration) -> FlushSchedule {
        FlushSchedule {
            interval: u64::try_from(interval.as_millis()).unwrap_or(u64::MAX),
            last: None,
        }
    }

    pub fn request(&self, now: Millis) -> FlushAt {
        match self.last {
            None => FlushAt::Now,
            Some(last) => {
                let due = last.saturating_add(Millis(self.interval));
                if now >= due {
                    FlushAt::Now
                } else {
                    FlushAt::At(due)
                }
            }
        }
    }

    /// Records that a pass ran at `now`.
    pub fn flushed(&mut self, now: Millis) {
        self.last = Some(now);
    }

    /// When the last pass ran.
    pub fn last(&self) -> Option<Millis> {
        self.last
    }
}
