//! The daemon's open-file limit, raised at start-up.
//!
//! macOS watches a path with one file descriptor's worth of budget: `notify` refuses to watch more
//! than a twelfth of the soft `RLIMIT_NOFILE` paths in the process, and launchd starts agents with
//! a soft limit of 256. The daemon watches two paths per space, so it raises the soft limit
//! towards the hard one before it watches anything. Reading and setting the limit are the binary's
//! job (this crate forbids `unsafe`), so they come in through [`FdLimits`].

use std::io;

/// The most the soft limit is raised to. macOS refuses a `RLIMIT_NOFILE` above `OPEN_MAX`
/// (10,240) when the hard limit is unlimited.
pub const SOFT_LIMIT_CAP: u64 = 10_240;

/// A process's open-file limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FdLimit {
    pub soft: u64,
    pub hard: u64,
}

/// Reads and sets the process's open-file limits.
pub trait FdLimits {
    fn get(&self) -> io::Result<FdLimit>;
    /// Sets the soft limit, leaving the hard one alone.
    fn set_soft(&self, soft: u64) -> io::Result<()>;
}

/// The soft limit to ask for given the current limits: the lesser of the hard limit and `cap`,
/// and only when that is above the current soft limit, so a higher soft limit is never lowered.
pub fn raised_soft(current: FdLimit, cap: u64) -> Option<u64> {
    let target = current.hard.min(cap);
    (target > current.soft).then_some(target)
}

/// Raises the soft limit as far as [`raised_soft`] allows and says what came of it, for the
/// start-up log. A failure leaves the limit as it was and is reported, not returned: the daemon
/// runs without the raise.
pub fn raise(limits: &dyn FdLimits) -> String {
    let before = match limits.get() {
        Ok(limit) => limit,
        Err(error) => return format!("open-file limit unknown: cannot read it: {error}"),
    };
    let Some(target) = raised_soft(before, SOFT_LIMIT_CAP) else {
        return format!(
            "open-file limit {} (hard {}), already enough",
            before.soft, before.hard
        );
    };
    if let Err(error) = limits.set_soft(target) {
        return format!(
            "open-file limit {} (hard {}), cannot raise it to {target}: {error}",
            before.soft, before.hard
        );
    }
    match limits.get() {
        Ok(after) => format!(
            "open-file limit {} (hard {}), raised from {}",
            after.soft, after.hard, before.soft
        ),
        Err(_) => format!("open-file limit raised from {} to {target}", before.soft),
    }
}
