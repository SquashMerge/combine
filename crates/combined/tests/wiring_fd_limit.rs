//! The start-up open-file limit raise never lowers a higher soft limit.

use std::cell::Cell;
use std::io;

use combined::wiring::fd_limit::{raise, raised_soft, FdLimit, FdLimits, SOFT_LIMIT_CAP};

fn limit(soft: u64, hard: u64) -> FdLimit {
    FdLimit { soft, hard }
}

#[test]
fn raises_to_the_cap_when_the_hard_limit_allows() {
    assert_eq!(
        raised_soft(limit(256, u64::MAX), SOFT_LIMIT_CAP),
        Some(10_240)
    );
    assert_eq!(
        raised_soft(limit(256, 1 << 20), SOFT_LIMIT_CAP),
        Some(10_240)
    );
}

#[test]
fn raises_to_the_hard_limit_when_it_is_below_the_cap() {
    assert_eq!(raised_soft(limit(256, 4096), SOFT_LIMIT_CAP), Some(4096));
}

#[test]
fn never_lowers_a_higher_soft_limit() {
    for current in [
        limit(10_240, u64::MAX),
        limit(65_536, u64::MAX),
        limit(20_000, 20_000),
        limit(4096, 4096),
    ] {
        assert_eq!(raised_soft(current, SOFT_LIMIT_CAP), None, "{current:?}");
    }
}

struct Fake {
    now: Cell<FdLimit>,
    refuse: bool,
    set_calls: Cell<u32>,
}

impl Fake {
    fn new(now: FdLimit, refuse: bool) -> Fake {
        Fake {
            now: Cell::new(now),
            refuse,
            set_calls: Cell::new(0),
        }
    }
}

impl FdLimits for Fake {
    fn get(&self) -> io::Result<FdLimit> {
        Ok(self.now.get())
    }

    fn set_soft(&self, soft: u64) -> io::Result<()> {
        self.set_calls.set(self.set_calls.get() + 1);
        if self.refuse {
            return Err(io::Error::from_raw_os_error(22));
        }
        self.now.set(limit(soft, self.now.get().hard));
        Ok(())
    }
}

#[test]
fn raise_sets_the_limit_and_reports_it() {
    let fake = Fake::new(limit(256, u64::MAX), false);
    let text = raise(&fake);
    assert_eq!(fake.now.get().soft, 10_240);
    assert!(
        text.contains("10240") && text.contains("raised from 256"),
        "{text}"
    );
}

#[test]
fn raise_leaves_a_higher_limit_alone() {
    let fake = Fake::new(limit(50_000, u64::MAX), false);
    raise(&fake);
    assert_eq!(fake.set_calls.get(), 0);
    assert_eq!(fake.now.get().soft, 50_000);
}

#[test]
fn raise_carries_on_when_the_system_refuses() {
    let fake = Fake::new(limit(256, u64::MAX), true);
    let text = raise(&fake);
    assert_eq!(fake.now.get().soft, 256);
    assert!(text.contains("cannot raise"), "{text}");
}
