#![cfg(feature = "test-support")]
//! The contract suite against the herdr-core adapter and an isolated core, plus a real-core
//! watch test and the event capture. Every test here is a recorded skip unless
//! `COMBINE_HC_LANE=1`, which only `sh tools/verify-herdr-core` sets, so `./tools/verify`
//! never needs `hc`.

mod common;
mod daemon_support;
#[macro_use]
mod contract;
#[path = "contract/cases/basics.rs"]
mod basics;
#[path = "contract/daemon_lane.rs"]
mod daemon_lane;
#[path = "contract/herdr_core_target.rs"]
mod herdr_core_target;
#[path = "contract/lane_safety.rs"]
mod lane_safety;
#[path = "contract/cases/ownership.rs"]
mod ownership;
#[path = "contract/cases/pair.rs"]
mod pair;
#[path = "contract/cases/payload.rs"]
mod payload;
#[path = "contract/cases/queries.rs"]
mod queries;
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

use std::time::Duration;

use combine_protocol::{OwnershipGeneration, TermId};
use combined::model::{TermEvent, TermState};
use combined::ports::{AttachOptions, TerminalBackend};
use contract::report::{record, Verdict};
use contract::target::Target;
use futures::stream::{BoxStream, StreamExt};
use herdr_core_target::HerdrCoreTarget;
use serde_json::json;

contract!(
    target: herdr_core_target::HerdrCoreTarget,
    make: herdr_core_target::HerdrCoreTarget::from_lane,
    lane: herdr_core_target::lane,
    lane_target: "herdr-core",
    areas: [
        basics, payload, remote, restart, resize, two_attachments, slow_client, pair, ownership,
        queries
    ],
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
        exit_with_output_waiting_for_credit_still_finishes,
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

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/adapters/herdr_core/fixtures"
);

async fn next_event(events: &mut BoxStream<'static, TermEvent>, seconds: u64) -> Option<TermEvent> {
    tokio::time::timeout(Duration::from_secs(seconds), events.next())
        .await
        .ok()
        .flatten()
}

/// Waits for an event the predicate picks, skipping others.
async fn wait_for(
    events: &mut BoxStream<'static, TermEvent>,
    seconds: u64,
    what: &str,
    pick: impl Fn(&TermEvent) -> bool,
) -> Result<TermEvent, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, events.next()).await {
            Ok(Some(event)) if pick(&event) => return Ok(event),
            Ok(Some(_)) => {}
            Ok(None) => return Err(format!("the watch ended before {what}")),
            Err(_) => return Err(format!("no {what} within {seconds} s")),
        }
    }
}

fn attach_options() -> AttachOptions {
    AttachOptions {
        cols: 80,
        rows: 24,
        display: common::display(),
        observe: false,
        resumed: false,
        history: 0,
    }
}

async fn spawn_one(
    target: &mut HerdrCoreTarget,
    backend: &dyn TerminalBackend,
    dir: &str,
) -> TermId {
    let cwd = target.scratch_dir(dir);
    let spec = target.spawn_spec(&cwd, 80, 24);
    backend.spawn(spec).await.expect("spawn")
}

async fn watch_scenario(target: &mut HerdrCoreTarget) -> Result<(), String> {
    let backend = target.backend();
    let mut events = backend.watch().await.map_err(|e| e.to_string())?;
    let quiet = tokio::time::timeout(Duration::from_millis(300), events.next()).await;
    if let Ok(event) = quiet {
        return Err(format!("an idle watch produced {event:?}"));
    }

    let a = spawn_one(target, backend.as_ref(), "a").await;
    let created = wait_for(
        &mut events,
        10,
        "Created(A)",
        |e| matches!(e, TermEvent::Created(i) if i.id == a),
    )
    .await?;
    let TermEvent::Created(info) = created else {
        unreachable!()
    };
    if info.state != TermState::Running {
        return Err(format!("A was created as {:?}", info.state));
    }

    let mut attachment = backend
        .attach(&a, attach_options())
        .await
        .map_err(|e| e.to_string())?;
    attachment
        .focus(OwnershipGeneration(0))
        .await
        .map_err(|e| e.to_string())?;
    attachment
        .resize(OwnershipGeneration(0), 100, 30)
        .await
        .map_err(|e| e.to_string())?;
    wait_for(
        &mut events,
        10,
        "Changed(A) at 100x30",
        |e| matches!(e, TermEvent::Changed(i) if i.id == a && (i.cols, i.rows) == (100, 30)),
    )
    .await?;

    backend.kill(&a).await.map_err(|e| e.to_string())?;
    wait_for(
        &mut events,
        10,
        "Exited(A)",
        |e| matches!(e, TermEvent::Exited { id, .. } if *id == a),
    )
    .await?;
    attachment.detach().await.map_err(|e| e.to_string())?;

    // An exited terminal the core removes leaves the list: the watch reports it `Lost`.
    target.checked_status()?;
    target
        .probe()
        .call(
            "terminal.rm",
            json!({"terminal": a.0.trim_start_matches("hc:")}),
        )
        .map_err(|e| e.to_string())?;
    wait_for(
        &mut events,
        10,
        "Lost(A) after it was removed",
        |e| matches!(e, TermEvent::Lost { id } if *id == a),
    )
    .await?;

    let b = spawn_one(target, backend.as_ref(), "b").await;
    wait_for(
        &mut events,
        10,
        "Created(B)",
        |e| matches!(e, TermEvent::Created(i) if i.id == b),
    )
    .await?;
    let instance = backend.instance();
    if instance.is_none() {
        return Err("the core named no backend instance".to_string());
    }
    target
        .restart_backend()
        .await
        .map_err(|e| e.0.to_string())?;
    wait_for(
        &mut events,
        30,
        "Restarted(B)",
        |e| matches!(e, TermEvent::Restarted { id } if *id == b),
    )
    .await?;
    backend
        .list()
        .await
        .map_err(|e| format!("the same core refused a call after the restart: {e}"))?;
    if backend.instance() != instance {
        return Err("the backend instance changed across the restart".to_string());
    }

    let meta = std::fs::read_to_string(format!("{FIXTURES}/meta.json"))
        .map_err(|_| "no meta.json: run `sh tools/verify-herdr-core --capture`".to_string())?;
    let meta: serde_json::Value = serde_json::from_str(&meta).map_err(|e| e.to_string())?;
    let captured = meta
        .get("herdr_core_version")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if captured != target.version() {
        return Err(format!(
            "the fixtures are from herdr-core {captured}, the lane runs {}: run `sh tools/verify-herdr-core --capture`",
            target.version()
        ));
    }
    Ok(())
}

#[test]
fn combined_connect_attach_replay_resume_on_herdr_core() {
    if herdr_core_target::lane().is_err() {
        return;
    }
    daemon_lane::run(
        "combined_connect_attach_replay_resume_on_herdr_core",
        daemon_lane::connect_attach_replay_resume,
    );
}

#[test]
fn two_panels_recover_across_a_herdr_core_restart() {
    if herdr_core_target::lane().is_err() {
        return;
    }
    daemon_lane::run(
        "two_panels_recover_across_a_herdr_core_restart",
        daemon_lane::two_panels_across_a_restart,
    );
}

#[test]
fn watch_reports_real_core_changes() {
    if herdr_core_target::lane().is_err() {
        return;
    }
    let mut target = HerdrCoreTarget::from_lane();
    let outcome = HerdrCoreTarget::block_on(watch_scenario(&mut target));
    let verdict = match &outcome {
        Ok(()) => Verdict::Pass,
        Err(reason) => Verdict::Fail(contract::Failure::new("watch", reason.clone())),
    };
    let line = record(
        "watch_reports_real_core_changes",
        &target.describe(),
        &verdict,
    );
    if let Err(reason) = outcome {
        panic!("{line}\n{reason}");
    }
}

async fn capture_scenario(target: &mut HerdrCoreTarget) -> Result<serde_json::Value, String> {
    let dir = std::path::Path::new(FIXTURES);
    let backend = target.backend();
    let control = target.control().clone();
    control.record_raw_events(dir).map_err(|e| e.to_string())?;
    let probe = target.probe().clone();
    let mut steps = Vec::new();

    let z = spawn_one(target, backend.as_ref(), "z").await;
    steps.push("spawned Z, which runs through the capture");
    let mut events = backend.watch().await.map_err(|e| e.to_string())?;
    steps.push("subscribed: subscribe-1.json");

    let a = spawn_one(target, backend.as_ref(), "a").await;
    wait_for(
        &mut events,
        10,
        "Created(A)",
        |e| matches!(e, TermEvent::Created(i) if i.id == a),
    )
    .await?;
    let address = a.0.trim_start_matches("hc:").to_string();
    probe
        .call(
            "terminal.label",
            json!({"terminal": address, "labels": {"captured": "yes"}}),
        )
        .map_err(|e| e.to_string())?;
    steps.push("labelled A");
    let mut attachment = backend
        .attach(&a, attach_options())
        .await
        .map_err(|e| e.to_string())?;
    attachment
        .focus(OwnershipGeneration(0))
        .await
        .map_err(|e| e.to_string())?;
    attachment
        .resize(OwnershipGeneration(0), 100, 30)
        .await
        .map_err(|e| e.to_string())?;
    wait_for(
        &mut events,
        10,
        "a size event",
        |e| matches!(e, TermEvent::Changed(i) if i.id == a && i.cols == 100),
    )
    .await?;
    steps.push("attached A, focused it and resized it to 100x30");
    attachment.detach().await.map_err(|e| e.to_string())?;
    backend.kill(&a).await.map_err(|e| e.to_string())?;
    wait_for(
        &mut events,
        10,
        "Exited(A)",
        |e| matches!(e, TermEvent::Exited { id, .. } if *id == a),
    )
    .await?;
    // A destructive call follows a fresh status whose core id passed the pin, as the crash
    // path does.
    target.checked_status()?;
    probe
        .call("terminal.rm", json!({"terminal": address}))
        .map_err(|e| e.to_string())?;
    steps.push("killed and removed A");

    let b = spawn_one(target, backend.as_ref(), "b").await;
    wait_for(
        &mut events,
        10,
        "Created(B)",
        |e| matches!(e, TermEvent::Created(i) if i.id == b),
    )
    .await?;
    target
        .restart_backend()
        .await
        .map_err(|e| e.0.to_string())?;
    wait_for(
        &mut events,
        30,
        "Restarted(B)",
        |e| matches!(e, TermEvent::Restarted { id } if *id == b),
    )
    .await?;
    steps.push("spawned B and restarted the core: subscribe-2.json");

    tokio::time::sleep(Duration::from_secs(16)).await;
    steps.push("waited 16 s for a heartbeat");

    control.stall_watch_credit(true);
    let mut attachment = backend
        .attach(&b, attach_options())
        .await
        .map_err(|e| e.to_string())?;
    attachment
        .focus(OwnershipGeneration(0))
        .await
        .map_err(|e| e.to_string())?;
    let started = std::time::Instant::now();
    let events_path = dir.join("events.jsonl");
    let missed_seen = || {
        std::fs::read_to_string(&events_path)
            .map(|text| text.lines().any(|line| line.contains("\"missed\"")))
            .unwrap_or(false)
    };
    let mut sent = 0u32;
    while sent < 100_000 && started.elapsed() < Duration::from_secs(120) {
        let (cols, rows) = if sent.is_multiple_of(2) {
            (81, 24)
        } else {
            (80, 24)
        };
        if attachment
            .resize(OwnershipGeneration(0), cols, rows)
            .await
            .is_err()
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        sent += 1;
        if sent.is_multiple_of(512) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
    control.stall_watch_credit(false);
    let wait_until = std::time::Instant::now() + Duration::from_secs(10);
    while !missed_seen() && std::time::Instant::now() < wait_until {
        let _ = next_event(&mut events, 1).await;
    }
    steps.push("stalled the watcher's credit, flooded size events, released");
    attachment.detach().await.map_err(|e| e.to_string())?;
    let _ = z;

    let lines = std::fs::read_to_string(&events_path).map_err(|e| e.to_string())?;
    let has = |needle: &str| lines.lines().any(|line| line.contains(needle));
    if !has("\"type\":\"size\"") {
        return Err("the capture saw no size event: stop and report".to_string());
    }
    if !has("heartbeat") {
        return Err("the capture saw no heartbeat: stop and report".to_string());
    }
    Ok(json!({
        "herdr_core_version": target.version(),
        "steps": steps,
        "heartbeat_observed": true,
        "missed_observed": missed_seen(),
    }))
}

/// Rewrites the committed event fixtures from the lane's core. Run with
/// `sh tools/verify-herdr-core --capture`.
#[test]
fn capture_event_fixtures() {
    if herdr_core_target::lane().is_err()
        || std::env::var("COMBINE_HC_CAPTURE").as_deref() != Ok("1")
    {
        return;
    }
    let mut target = HerdrCoreTarget::from_lane();
    let meta = HerdrCoreTarget::block_on(capture_scenario(&mut target))
        .unwrap_or_else(|reason| panic!("capture failed: {reason}"));
    std::fs::write(
        format!("{FIXTURES}/meta.json"),
        serde_json::to_string_pretty(&meta).expect("meta") + "\n",
    )
    .expect("meta.json");
}
