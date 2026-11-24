#![cfg(feature = "test-support")]
//! A [`WallClock`] a test sets.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::ports::WallClock;

/// Tells the time a test sets. It never moves on its own.
pub struct FakeWallClock {
    unix_ms: AtomicU64,
}

impl FakeWallClock {
    pub fn new(unix_ms: u64) -> FakeWallClock {
        FakeWallClock {
            unix_ms: AtomicU64::new(unix_ms),
        }
    }

    pub fn set(&self, ms: u64) {
        self.unix_ms.store(ms, Ordering::SeqCst);
    }

    /// Moves the time on by `ms`, stopping at `u64::MAX`.
    pub fn advance(&self, ms: u64) {
        let _ = self
            .unix_ms
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |now| {
                Some(now.saturating_add(ms))
            });
    }
}

impl WallClock for FakeWallClock {
    fn now_unix_ms(&self) -> u64 {
        self.unix_ms.load(Ordering::SeqCst)
    }
}
