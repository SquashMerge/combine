#![cfg(feature = "test-support")]
//! The contract suite, areas resize, two_attachments and slow_client, run against the fake.

mod common;
#[macro_use]
mod contract;
#[path = "contract/cases/resize.rs"]
mod resize;
#[path = "contract/cases/slow_client.rs"]
mod slow_client;
#[path = "contract/cases/two_attachments.rs"]
mod two_attachments;

contract!(
    target: contract::fake_target::FakeTarget,
    make: contract::fake_target::FakeTarget::new,
    areas: [resize, two_attachments, slow_client],
    cases: [
        replay_from_before_a_resize_boundary,
        replay_from_after_a_resize_boundary,
        old_size_output_in_flight_lands_before_resized,
        same_size_and_non_owner_resizes_change_nothing,
        two_attachments_route_operations_and_show_the_pty_size,
        rebuilt_attachment_reapplies_its_latest_requested_size,
        slow_client_output_stays_bounded,
        many_small_frames_to_a_paused_client_never_gap,
        client_reading_without_acks_is_held_back_not_gapped,
        credit_is_granted_only_while_the_log_has_room,
        event_flood_ends_with_a_final_gap_once_drained,
        blocked_client_is_disconnected_and_resume_gets_gap,
        backend_without_byte_credit_is_still_bounded,
    ]
);
