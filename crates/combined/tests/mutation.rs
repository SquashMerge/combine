#![cfg(feature = "test-support")]
//! The suite notices a faulty backend: each mutation of the fake must fail the cases built
//! to catch it, on the check that names the fault.

mod common;
#[macro_use]
mod contract;
#[path = "contract/cases/basics.rs"]
mod basics;
#[path = "contract/cases/payload.rs"]
mod payload;
#[path = "contract/cases/remote.rs"]
mod remote;
#[path = "contract/cases/resize.rs"]
mod resize;
#[path = "contract/cases/restart.rs"]
mod restart;
#[path = "contract/cases/slow_client.rs"]
mod slow_client;
#[path = "contract/cases/two_attachments.rs"]
mod two_attachments;

use combined::testing::Mutation;
use contract::fake_target::FakeTarget;
use contract::report::{record, Verdict};
use contract::{run_quietly, Case};

fn all_cases() -> Vec<Case<FakeTarget>> {
    let mut all = Vec::new();
    all.extend(basics::cases::<FakeTarget>());
    all.extend(payload::cases::<FakeTarget>());
    all.extend(remote::cases::<FakeTarget>());
    all.extend(restart::cases::<FakeTarget>());
    all.extend(resize::cases::<FakeTarget>());
    all.extend(two_attachments::cases::<FakeTarget>());
    all.extend(slow_client::cases::<FakeTarget>());
    all
}

/// Runs every case that isn't heavy against the mutated fake, and returns each verdict.
fn run_suite(mutation: Mutation) -> Vec<(&'static str, Verdict)> {
    all_cases()
        .iter()
        .filter(|case| !case.heavy)
        .map(|case| {
            let verdict = run_quietly(|| FakeTarget::mutated(mutation), case);
            (case.name, verdict)
        })
        .collect()
}

/// The suite fails somewhere, and `detector` fails on one of `checks`.
fn assert_caught(mutation: Mutation, name: &str, detector: &str, checks: &[&str]) {
    let verdicts = run_suite(mutation);
    assert!(
        verdicts
            .iter()
            .any(|(_, verdict)| matches!(verdict, Verdict::Fail(_))),
        "no case failed under {mutation:?}"
    );
    let caught = verdicts.iter().find(|(case, _)| *case == detector);
    assert!(
        matches!(caught, Some((_, Verdict::Fail(failure))) if checks.contains(&failure.check)),
        "{detector} should fail on one of {checks:?} under {mutation:?}; it gave {caught:?}"
    );
    record(
        &format!("mutation:{name}"),
        &format!("fake (mutation {mutation:?})"),
        &Verdict::Pass,
    );
}

#[test]
fn dropped_resized_fails_the_suite() {
    assert_caught(
        Mutation::DropResized,
        "dropped_resized",
        "replay_from_before_a_resize_boundary",
        &["resized-count"],
    );
}

#[test]
fn moved_resized_fails_the_suite() {
    assert_caught(
        Mutation::MoveResized,
        "moved_resized",
        "old_size_output_in_flight_lands_before_resized",
        &["resize-order"],
    );
}

#[test]
fn dropped_byte_fails_the_suite() {
    assert_caught(
        Mutation::DropByte,
        "dropped_byte",
        "reconnect_during_output_is_byte_exact",
        &["payload-length", "payload-sha256"],
    );
}
