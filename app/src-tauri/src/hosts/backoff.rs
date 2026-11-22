//! Reconnect delays: doubling from one second to a thirty second cap, no jitter.

use std::time::Duration;

pub const RECONNECT_INITIAL: Duration = Duration::from_secs(1);
pub const RECONNECT_MAX: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct Backoff {
    next: Duration,
}

impl Backoff {
    pub fn new() -> Self {
        Self {
            next: RECONNECT_INITIAL,
        }
    }

    /// Returns the delay to wait now and doubles the one after it, up to the cap.
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = (self.next * 2).min(RECONNECT_MAX);
        delay
    }

    pub fn reset(&mut self) {
        self.next = RECONNECT_INITIAL;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_to_cap() {
        let mut backoff = Backoff::new();
        let secs: Vec<u64> = (0..7).map(|_| backoff.next_delay().as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 30, 30]);
    }

    #[test]
    fn reset_returns_to_initial() {
        let mut backoff = Backoff::new();
        backoff.next_delay();
        backoff.next_delay();
        backoff.reset();
        assert_eq!(backoff.next_delay(), RECONNECT_INITIAL);
        assert_eq!(backoff.next_delay(), RECONNECT_INITIAL * 2);
    }
}
