#![cfg(feature = "test-support")]
//! The contract suite, areas basics, payload, remote and restart, run against the fake.

mod common;
#[macro_use]
mod contract;
#[path = "contract/cases/basics.rs"]
mod basics;
#[path = "contract/cases/payload.rs"]
mod payload;
#[path = "contract/cases/remote.rs"]
mod remote;
#[path = "contract/cases/restart.rs"]
mod restart;

contract!(
    target: contract::fake_target::FakeTarget,
    make: contract::fake_target::FakeTarget::new,
    areas: [basics, payload, remote, restart],
    cases: [
        spawn_attach_write_read_back_resize_kill,
        term_ids_are_prefixed_and_opaque,
        display_facts_follow_capabilities,
        detach_ends_with_a_final_closed,
        program_exit_is_final,
        adapter_reports_lost_only_after_giving_up,
        reconnect_during_output_is_byte_exact,
        hide_and_show_during_output_is_byte_exact,
        burst_of_50_mb_is_byte_exact,
        resynced_begins_a_fresh_paint_in_order,
        remote_terminals_never_appear_and_are_rejected,
        daemon_restart_answers_resume_with_gap_then_attaches_fresh,
        backend_restart_starts_one_new_exact_epoch,
        backend_crash_starts_one_new_inexact_epoch,
        inexact_terminal_stays_inexact,
    ]
);
