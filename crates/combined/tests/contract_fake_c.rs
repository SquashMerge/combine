#![cfg(feature = "test-support")]
//! The contract suite, areas pair, ownership and queries, run against the fake.

mod common;
#[macro_use]
mod contract;
#[path = "contract/cases/ownership.rs"]
mod ownership;
#[path = "contract/cases/pair.rs"]
mod pair;
#[path = "contract/cases/queries.rs"]
mod queries;

contract!(
    target: contract::fake_target::FakeTarget,
    make: contract::fake_target::FakeTarget::new,
    areas: [pair, ownership, queries],
    cases: [
        input_half_failure_starts_one_new_epoch,
        display_half_failure_starts_one_new_epoch,
        reopen_failing_halfway_starts_one_new_epoch,
        first_attach_failing_halfway_is_attach_failed,
        requests_while_reopening_apply_in_order,
        opening_a_second_panel_keeps_the_pty_size,
        watch_only_never_takes_size_or_ownership,
        stale_resize_is_rejected,
        non_owner_input_takes_ownership_before_it_is_forwarded,
        owner_leaving_hands_off_to_most_recently_focused,
        ownership_changes_while_a_pair_is_reopening,
        queued_requests_of_a_superseded_owner_do_not_take_over_on_reopen,
        two_pairs_reopening_keep_the_new_owners_latest_size_and_display,
        only_the_owners_reply_is_forwarded_once,
        replayed_then_live_query_gets_one_reply_for_the_live_one,
        stale_replies_are_dropped_silently,
    ]
);
