//! Injected time.

/// Monotonic milliseconds since an origin the caller picks. Core never reads a clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Millis(pub u64);

impl Millis {
    /// The sum, stopping at `u64::MAX` instead of overflowing.
    pub fn saturating_add(self, other: Millis) -> Millis {
        Millis(self.0.saturating_add(other.0))
    }
}
