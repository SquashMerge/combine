#![cfg(feature = "test-support")]
//! The cases whose `!signals` branches only a fake without a resync signal reaches, and the
//! `contract!` lane arm's skip path.

mod common;
#[macro_use]
mod contract;
#[path = "contract/cases/payload.rs"]
mod payload;
#[path = "contract/cases/remote.rs"]
mod remote;
#[path = "contract/cases/slow_client.rs"]
mod slow_client;

use contract::fake_target::FakeTarget;

#[test]
fn resynced_begins_a_fresh_paint_in_order_without_a_signal() {
    contract::run_named::<FakeTarget>(
        FakeTarget::silent,
        &payload::cases(),
        "resynced_begins_a_fresh_paint_in_order",
    );
}

#[test]
fn slow_client_output_stays_bounded_without_a_signal() {
    contract::run_named::<FakeTarget>(
        FakeTarget::silent,
        &slow_client::cases(),
        "slow_client_output_stays_bounded",
    );
}

/// A lane that says no records a skip and never builds the target: `make` panics.
mod skipped_lane {
    use super::remote;

    contract!(
        target: crate::contract::fake_target::FakeTarget,
        make: || -> crate::contract::fake_target::FakeTarget {
            panic!("the lane arm built a target after the lane said no")
        },
        lane: || Err::<(), String>("the lane is off".to_string()),
        lane_target: "skipped-lane",
        areas: [remote],
        cases: [remote_terminals_never_appear_and_are_rejected],
    );
}
