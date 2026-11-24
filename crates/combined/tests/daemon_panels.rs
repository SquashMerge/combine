#![cfg(feature = "test-support")]
//! The panels through the running server, with no client unless a test says so: in-memory
//! stores, the fake diagram check and a fake wall clock, called through the `PanelHandle`.
//! Waits are bounded polls; nothing asserts a duration except the 2 s a call may take.

mod common;
mod daemon_support;
mod panel_support;
mod spawn_support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use combine_protocol::{ClientMessage, ListTemplates, PanelState, RequestId, SkippedPanel};
use combined::core::{
    PanelApiError, PanelUpdate, PanelView, MAX_PANELS, SCRATCH_ID_OPEN, SCRATCH_TTL_MS,
};
use combined::model::Lifetime;
use combined::ports::{DiagramFailure, DiagramVerdict, FailureCode, ScratchDir, StoreError};
use daemon_support::wait_until;
use panel_support::{
    attach, markdown, mermaid, named, read_until_tail, scratch, settled_read_count, PanelRig, Race,
    TEST_TIMING, WAIT,
};
use spawn_support::{expect_templates, hash_hex, terminal_panel_json};

fn content_path(id: &str) -> String {
    format!("panels/{id}/content.md")
}

fn file_path(id: &str) -> String {
    format!("panels/{id}/panel.json")
}

fn update_content(content: &str) -> PanelUpdate {
    PanelUpdate {
        content: Some(content.to_string()),
        title: None,
    }
}

fn update_title(title: &str) -> PanelUpdate {
    PanelUpdate {
        content: None,
        title: Some(Some(title.to_string())),
    }
}

/// Waits for `get` to give a panel that `ready` accepts, and returns it with its source.
fn wait_for(
    rig: &PanelRig,
    id: &str,
    what: &str,
    ready: impl Fn(&PanelView, &str) -> bool,
) -> (PanelView, String) {
    let mut last = None;
    let held = wait_until(WAIT, || {
        last = Some(rig.get(id));
        matches!(&last, Some(Ok((view, source))) if ready(view, source))
    });
    assert!(held, "{id} never {what}; it is {last:?}");
    last.unwrap().unwrap()
}

/// A create sent right after a start, retried only while the space is still loading.
fn create_retrying_loading(
    rig: &PanelRig,
    create: combined::core::PanelCreate,
) -> Result<PanelView, PanelApiError> {
    let deadline = Instant::now() + WAIT;
    loop {
        match rig.create(create.clone()) {
            Err(PanelApiError::SpaceLoading) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            other => return other,
        }
    }
}

fn failure(code: FailureCode, line: u32, column: u32) -> DiagramVerdict {
    DiagramVerdict::Failed(DiagramFailure {
        code,
        message: "bad".to_string(),
        line: Some(line),
        column: Some(column),
        snippet: Some("A -->".to_string()),
    })
}

// AC1

#[test]
fn with_no_client_create_validates_and_writes_files() {
    let rig = PanelRig::new();
    let created = rig.create(named("p", markdown("one"))).unwrap();
    assert_eq!(created.id, "p");
    let (view, source) = rig.wait_state("p", PanelState::Validated);
    assert_eq!(
        (view.lifetime, source.as_str()),
        (Lifetime::Persistent, "one")
    );

    assert_eq!(rig.bytes(&content_path("p")).unwrap(), b"one");
    let file = String::from_utf8(rig.bytes(&file_path("p")).unwrap()).unwrap();
    assert!(file.contains("\"renderer\": \"markdown\""), "{file}");

    let diagram = rig.create(named("d", mermaid("graph TD; A-->B"))).unwrap();
    rig.wait_state(&diagram.id, PanelState::Validated);
    assert_eq!(
        rig.bytes("panels/d/diagram.mmd").unwrap(),
        b"graph TD; A-->B"
    );
    assert_eq!(rig.check.calls(), ["graph TD; A-->B"]);
    assert_eq!(rig.connections(), 0);
}

// AC2

#[test]
fn mermaid_failure_reaches_the_panel_with_line_and_column() {
    let rig = PanelRig::new();
    rig.check
        .answer("graph TD; A-->", Ok(failure(FailureCode::Syntax, 2, 5)));
    rig.create(named("d", mermaid("graph TD; A-->"))).unwrap();

    let (view, source) = rig.wait_state("d", PanelState::Failed);
    let error = view.error.unwrap();
    assert_eq!(error.code, "syntax");
    assert_eq!((error.line, error.column), (Some(2), Some(5)));
    assert_eq!(source, "graph TD; A-->");
    assert_eq!(rig.connections(), 0);
}

// AC3

#[test]
fn revisions_keep_growing_across_a_daemon_restart() {
    let mut rig = PanelRig::new();
    rig.create(named("p", markdown("one"))).unwrap();
    let updated = rig
        .panels
        .update(&rig.space, "p", update_content("two"))
        .unwrap();
    rig.wait_state("p", PanelState::Validated);

    rig.restart();
    let other = create_retrying_loading(&rig, named("q", markdown("q"))).unwrap();
    let (loaded, source) = rig.get("p").unwrap();
    assert_eq!(source, "two");
    assert!(loaded.revision > updated.revision, "{loaded:?} {updated:?}");
    assert!(other.revision > updated.revision);

    let again = rig
        .panels
        .update(&rig.space, "p", update_content("three"))
        .unwrap();
    assert!(again.revision > loaded.revision.max(other.revision));
    assert_eq!(rig.connections(), 0);
}

// AC4

#[test]
fn a_held_check_released_after_an_update_never_wins() {
    let rig = PanelRig::new();
    let (s1, s2, s3) = ("graph TD; A-->B", "graph TD; B-->C", "graph TD; C-->D");
    rig.check.answer(s1, Ok(failure(FailureCode::Syntax, 1, 1)));
    let hold = rig.check.hold(s1);
    rig.create(named("d", mermaid(s1))).unwrap();
    assert!(wait_until(WAIT, || rig.check.calls() == [s1]));

    rig.panels
        .update(&rig.space, "d", update_content(s2))
        .unwrap();
    let newest = rig
        .panels
        .update(&rig.space, "d", update_content(s3))
        .unwrap();
    hold.release();

    let (view, source) = rig.wait_state("d", PanelState::Validated);
    assert_eq!((view.revision, source.as_str()), (newest.revision, s3));
    assert!(view.error.is_none());
    assert_eq!(rig.check.calls(), [s1, s3]);
    assert_eq!(rig.connections(), 0);
}

// AC5

#[test]
fn outside_edit_with_no_client_revalidates() {
    let rig = PanelRig::new();
    rig.create(named("p", markdown("one"))).unwrap();
    let (before, _) = rig.wait_state("p", PanelState::Validated);

    rig.store
        .write_outside(&rig.space, &content_path("p"), b"two".to_vec());
    let (after, _) = wait_for(&rig, "p", "took the outside edit", |view, source| {
        source == "two" && view.state == PanelState::Validated
    });
    assert!(after.revision > before.revision);
    assert_eq!(rig.connections(), 0);
}

#[test]
fn own_writes_are_not_outside_edits() {
    let rig = PanelRig::new();
    rig.create(named("p", markdown("one"))).unwrap();
    rig.wait_state("p", PanelState::Validated);
    let updated = rig
        .panels
        .update(&rig.space, "p", update_content("two"))
        .unwrap();
    rig.wait_state("p", PanelState::Validated);
    settled_read_count(&rig.store, &rig.space, &content_path("p"), 2);

    let (view, source) = rig.get("p").unwrap();
    assert_eq!((view.revision, source.as_str()), (updated.revision, "two"));
    assert_eq!(rig.write_count(&content_path("p")), 2);
    assert_eq!(rig.connections(), 0);
}

#[test]
fn a_panel_folder_event_rereads_with_no_client() {
    let rig = PanelRig::new();
    rig.create(named("p", markdown("one"))).unwrap();
    let (first, _) = rig.wait_state("p", PanelState::Validated);
    let r = first.revision;

    // The create's own echoes settle: content.md has no compare at create, so its first read
    // is the echo's.
    let echo = settled_read_count(&rig.store, &rig.space, &content_path("p"), 1);
    let (view, source) = rig.get("p").unwrap();
    assert_eq!((view.revision, source.as_str()), (r, "one"));

    rig.store
        .write_quiet(&rig.space, &content_path("p"), b"two".to_vec());
    std::thread::sleep(Duration::from_millis(4 * TEST_TIMING.max_wait_ms));
    let (view, source) = rig.get("p").unwrap();
    assert_eq!((view.revision, source.as_str()), (r, "one"));
    assert_eq!(rig.store.read_count(&rig.space, &content_path("p")), echo);

    rig.store.notify_outside(&rig.space, "panels");
    let (view, _) = wait_for(&rig, "p", "reread the folder", |view, source| {
        view.revision > r && source == "two" && view.state == PanelState::Validated
    });
    assert!(view.revision > r);
    assert_eq!(rig.connections(), 0);
}

// AC6

#[test]
fn invalid_panel_json_is_never_rewritten_on_update_shutdown_or_restart() {
    let mut rig = PanelRig::new();
    let valid = br#"{"version":1,"renderer":"markdown"}"#.to_vec();
    rig.store.write_outside(&rig.space, &file_path("p"), valid);
    rig.store
        .write_outside(&rig.space, &content_path("p"), b"one".to_vec());
    rig.create(named("q", markdown("q"))).unwrap();
    wait_for(&rig, "p", "loaded", |view, _| {
        view.state == PanelState::Validated
    });

    let invalid = b"{not json".to_vec();
    rig.store
        .write_outside(&rig.space, &file_path("p"), invalid.clone());
    wait_for(&rig, "p", "saw the invalid file", |view, _| {
        view.file_error.is_some()
    });

    rig.panels
        .update(&rig.space, "p", update_content("two"))
        .unwrap();
    assert!(rig
        .panels
        .update(&rig.space, "p", update_title("t"))
        .is_err());
    assert_eq!(rig.bytes(&content_path("p")).unwrap(), b"two");
    assert_eq!(rig.bytes(&file_path("p")).unwrap(), invalid);
    assert_eq!(rig.write_count(&file_path("p")), 0);

    rig.restart();
    create_retrying_loading(&rig, named("r", markdown("r"))).unwrap();
    assert_eq!(rig.bytes(&file_path("p")).unwrap(), invalid);
    assert_eq!(rig.write_count(&file_path("p")), 0);
    let list = rig.panels.list(&rig.space).unwrap();
    let skipped: Vec<&SkippedPanel> = list.skipped.iter().filter(|s| s.panel == "p").collect();
    assert_eq!(skipped.len(), 1, "{list:?}");
    assert!(skipped[0].reason.contains("JSON"), "{}", skipped[0].reason);
    assert!(list.panels.iter().all(|view| view.id != "p"));
}

#[test]
fn a_pending_title_retry_never_rewrites_an_invalid_panel_json() {
    let mut timing = TEST_TIMING;
    timing.retry_ms = 1_000;
    let rig = PanelRig::with_timing(timing);
    rig.create(named("p", markdown("one"))).unwrap();
    rig.wait_state("p", PanelState::Validated);
    let writes = rig.write_count(&file_path("p"));

    rig.store
        .fail_next_write(&rig.space, StoreError::Unavailable("busy".to_string()));
    rig.panels
        .update(&rig.space, "p", update_title("t"))
        .unwrap();
    let invalid = b"{not json".to_vec();
    rig.store
        .write_outside(&rig.space, &file_path("p"), invalid.clone());

    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(rig.bytes(&file_path("p")).unwrap(), invalid);
    assert_eq!(rig.write_count(&file_path("p")), writes);
    assert_eq!(rig.connections(), 0);
}

// AC7

#[test]
fn idempotency_key_survives_a_restart() {
    let mut rig = PanelRig::new();
    let mut request = markdown("one");
    request.idempotency_key = Some("k-1".to_string());
    let first = rig.create(request.clone()).unwrap();
    rig.wait_state(&first.id, PanelState::Validated);

    rig.restart();
    let again = create_retrying_loading(&rig, request).unwrap();
    assert_eq!(again.id, first.id);
    assert_eq!(rig.list_ids(), [first.id]);
    assert_eq!(rig.connections(), 0);
}

// AC8

#[test]
fn the_101st_create_is_refused_and_nothing_is_written() {
    let rig = PanelRig::new();
    for n in 0..MAX_PANELS {
        rig.create(named(&format!("p{n}"), markdown("x"))).unwrap();
    }
    let refused = rig.create(named("extra", markdown("x"))).unwrap_err();
    assert_eq!(
        refused,
        PanelApiError::PanelLimit {
            count: MAX_PANELS,
            limit: MAX_PANELS
        }
    );
    assert_eq!(rig.bytes(&file_path("extra")), None);
    assert_eq!(rig.write_count(&file_path("extra")), 0);
    assert_eq!(rig.write_count(&content_path("extra")), 0);
    assert_eq!(rig.connections(), 0);
}

#[test]
fn remove_deletes_the_panel_files_and_ds_store_and_frees_the_id() {
    let rig = PanelRig::new();
    rig.create(named("p", markdown("one"))).unwrap();
    rig.wait_state("p", PanelState::Validated);
    rig.store
        .write_outside(&rig.space, "panels/p/.DS_Store", b"finder".to_vec());
    rig.store
        .write_outside(&rig.space, "panels/p/notes.txt", b"mine".to_vec());

    let removed = rig.panels.remove(&rig.space, "p").unwrap();
    assert_eq!(removed.state, PanelState::Removed);
    assert_eq!(rig.bytes(&file_path("p")), None);
    assert_eq!(rig.bytes(&content_path("p")), None);
    assert_eq!(rig.bytes("panels/p/.DS_Store"), None);
    assert_eq!(rig.bytes("panels/p/notes.txt").unwrap(), b"mine");

    let again = rig.create(named("p", markdown("two"))).unwrap();
    assert_eq!(again.id, "p");
    rig.wait_state("p", PanelState::Validated);
    assert_eq!(rig.connections(), 0);
}

// AC9

#[test]
fn scratch_panel_survives_a_restart_and_expires_after_7_days() {
    let mut rig = PanelRig::new();
    rig.create(named("s", scratch(markdown("one")))).unwrap();
    rig.wait_state("s", PanelState::Validated);
    let dir = ScratchDir {
        key: rig.space.sha256_hex(),
        panel: "s".to_string(),
    };
    assert_eq!(rig.scratch.dirs(), std::slice::from_ref(&dir));
    assert_eq!(rig.store.write_count(&rig.space, &file_path("s")), 0);

    rig.restart();
    create_retrying_loading(&rig, named("q", markdown("q"))).unwrap();
    let (view, source) = rig.get("s").unwrap();
    assert_eq!((view.lifetime, source.as_str()), (Lifetime::Scratch, "one"));

    rig.wall.advance(SCRATCH_TTL_MS + 1);
    assert!(
        wait_until(WAIT, || rig.scratch.dirs().is_empty()),
        "the expired scratch panel was never swept"
    );
    let (gone, source) = rig.get("s").unwrap();
    assert_eq!((gone.state, source.as_str()), (PanelState::Removed, ""));
    assert_eq!(rig.get("q").unwrap().0.lifetime, Lifetime::Persistent);
    assert_eq!(rig.connections(), 0);
}

#[test]
fn pin_moves_a_scratch_panel_into_workspace() {
    let rig = PanelRig::new();
    rig.create(named("s", scratch(markdown("one")))).unwrap();
    rig.wait_state("s", PanelState::Validated);

    rig.panels.pin(&rig.space, "s").unwrap();
    assert_eq!(rig.bytes(&content_path("s")).unwrap(), b"one");
    assert!(rig.bytes(&file_path("s")).is_some());
    assert!(rig.scratch.dirs().is_empty());
    let (view, source) = wait_for(&rig, "s", "became persistent", |view, _| {
        view.lifetime == Lifetime::Persistent && view.state == PanelState::Validated
    });
    assert_eq!(source, "one");
    assert!(view.file_error.is_none());
    assert_eq!(rig.connections(), 0);
}

#[test]
fn a_pinned_panel_is_not_swept_after_7_days() {
    let rig = PanelRig::new();
    rig.create(named("s", scratch(markdown("one")))).unwrap();
    rig.panels.pin(&rig.space, "s").unwrap();
    wait_for(&rig, "s", "became persistent", |view, _| {
        view.lifetime == Lifetime::Persistent && view.state == PanelState::Validated
    });

    rig.wall.advance(8 * 24 * 3_600_000);
    std::thread::sleep(Duration::from_millis(400));
    let (view, source) = rig.get("s").unwrap();
    assert_eq!(view.lifetime, Lifetime::Persistent);
    assert_eq!(
        (view.state, source.as_str()),
        (PanelState::Validated, "one")
    );
    assert_eq!(rig.bytes(&content_path("s")).unwrap(), b"one");
    assert_eq!(rig.remove_count(&content_path("s")), 0);
    assert_eq!(rig.remove_count(&file_path("s")), 0);
    assert_eq!(rig.connections(), 0);
}

// AC10

#[test]
fn spaces_from_the_terminal_list_are_watched_without_a_client() {
    let rig = PanelRig::new();
    assert_eq!(
        rig.panels.list(&rig.space).unwrap_err(),
        PanelApiError::UnknownSpace
    );

    rig.spawn_terminal();
    assert!(
        wait_until(WAIT, || rig.store.list_count(&rig.space, "panels") >= 1),
        "the terminal's space was never listed"
    );
    rig.store.write_outside(
        &rig.space,
        &file_path("x"),
        br#"{"version":1,"renderer":"markdown"}"#.to_vec(),
    );
    rig.store
        .write_outside(&rig.space, &content_path("x"), b"hello".to_vec());

    let (view, _) = wait_for(&rig, "x", "was adopted", |view, source| {
        source == "hello" && view.state == PanelState::Validated
    });
    assert_eq!(view.lifetime, Lifetime::Persistent);
    assert_eq!(rig.connections(), 0);
}

// AC11

#[test]
fn a_held_check_does_not_stall_terminal_output() {
    let rig = PanelRig::new();
    let term = rig.spawn_terminal();
    let source = "graph TD; A-->B";
    let hold = rig.check.hold(source);
    rig.create(named("d", mermaid(source))).unwrap();
    assert!(wait_until(WAIT, || rig.check.in_flight() == 1));

    let mut client = rig.client();
    let attachment = attach(&mut client, &term);
    rig.control.emit(&term, b"HELLO");
    let seen = read_until_tail(&mut client, attachment, b"HELLO", WAIT);
    assert!(seen.is_some(), "no output while the check was held");
    assert_eq!(rig.check.in_flight(), 1);

    hold.release();
    rig.wait_state("d", PanelState::Validated);
}

#[test]
fn no_more_than_two_checks_run_at_once() {
    let rig = PanelRig::new();
    let sources: Vec<String> = (1..=5).map(|n| format!("graph TD; A-->N{n}")).collect();
    let holds: Vec<_> = sources.iter().map(|s| rig.check.hold(s)).collect();
    for (n, source) in sources.iter().enumerate() {
        rig.create(named(&format!("d{n}"), mermaid(source)))
            .unwrap();
    }
    assert!(wait_until(WAIT, || rig.check.in_flight() == 2));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(rig.check.in_flight(), 2);
    assert_eq!(rig.check.peak_in_flight(), 2);

    for hold in holds {
        hold.release();
    }
    for n in 0..sources.len() {
        rig.wait_state(&format!("d{n}"), PanelState::Validated);
    }
    assert_eq!(rig.check.peak_in_flight(), 2);
}

#[test]
fn panel_calls_are_answered_while_output_waits_to_flush() {
    let rig = PanelRig::new();
    let term = rig.spawn_terminal();
    let mut client = rig.client();
    let attachment = attach(&mut client, &term);
    let control = rig.control.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let emitting = Arc::clone(&stop);
    let emitter = std::thread::spawn(move || {
        while !emitting.load(Ordering::SeqCst) {
            control.emit(&term, &[b'x'; 512]);
            std::thread::sleep(Duration::from_millis(1));
        }
    });

    let mut slowest = Duration::ZERO;
    for n in 0..20 {
        let started = Instant::now();
        match n % 3 {
            0 => drop(rig.create(named(&format!("p{n}"), markdown("x"))).unwrap()),
            1 => drop(rig.panels.list(&rig.space).unwrap()),
            _ => drop(rig.panels.get(&rig.space, &format!("p{}", n - 2)).unwrap()),
        }
        slowest = slowest.max(started.elapsed());
        assert!(
            slowest < Duration::from_secs(2),
            "call {n} took {slowest:?}"
        );
    }
    stop.store(true, Ordering::SeqCst);
    emitter.join().unwrap();
    assert!(read_until_tail(
        &mut client,
        attachment,
        b"never",
        Duration::from_millis(100)
    )
    .is_none());
    assert_eq!(rig.connections(), 1);
}

// AC13

#[test]
fn terminal_templates_are_not_written_or_cached_by_the_panel_store() {
    let rig = PanelRig::new();
    let first = terminal_panel_json(&["sh"], None);
    rig.store
        .write_outside(&rig.space, &file_path("agent"), first.clone());
    rig.spawn_terminal();
    assert!(wait_until(WAIT, || rig
        .store
        .read_count(&rig.space, &file_path("agent"))
        >= 1));

    let mut client = rig.client();
    let hash_of = |client: &mut daemon_support::WireClient, request: u64| {
        client.send(&ClientMessage::ListTemplates(ListTemplates {
            request: RequestId(request),
            space: rig.space.0.clone(),
        }));
        let templates = expect_templates(client, request);
        assert!(templates.skipped.is_empty(), "{:?}", templates.skipped);
        templates.templates[0].hash.clone()
    };
    assert_eq!(hash_of(&mut client, 1), hash_hex(&first));

    let second = terminal_panel_json(&["zsh"], None);
    rig.store
        .write_outside(&rig.space, &file_path("agent"), second.clone());
    settled_read_count(&rig.store, &rig.space, &file_path("agent"), 2);
    assert_eq!(hash_of(&mut client, 2), hash_hex(&second));

    assert_eq!(rig.write_count(&file_path("agent")), 0);
    assert_eq!(rig.remove_count(&file_path("agent")), 0);
    assert!(rig.panels.list(&rig.space).unwrap().panels.is_empty());
}

// A2-5

#[test]
fn a_hand_placed_fallback_copy_is_never_written_and_never_bumps_revisions() {
    let rig = PanelRig::new();
    rig.create(named("seed", scratch(markdown("s")))).unwrap();

    let file = br#"{"version":1,"renderer":"markdown"}"#.to_vec();
    rig.store
        .write_quiet(&rig.space, &file_path("f"), b"project copy".to_vec());
    rig.store.put_fallback(&rig.space, &file_path("f"), file);
    rig.store
        .put_fallback(&rig.space, &content_path("f"), b"fallback".to_vec());
    rig.store.notify_outside(&rig.space, "panels");

    let (view, source) = wait_for(&rig, "f", "was adopted", |view, _| {
        view.state == PanelState::Validated
    });
    assert_eq!(source, "fallback");
    assert!(view.read_only);
    assert_eq!(
        rig.panels
            .update(&rig.space, "f", update_content("mine"))
            .unwrap_err(),
        PanelApiError::ReadOnly
    );

    rig.store
        .write_outside(&rig.space, &content_path("f"), b"project".to_vec());
    rig.store
        .write_outside(&rig.space, &file_path("f"), b"project again".to_vec());
    rig.store.notify_outside(&rig.space, "panels");
    settled_read_count(&rig.store, &rig.space, &file_path("f"), 2);

    let (after, source) = rig.get("f").unwrap();
    assert_eq!(
        (after.revision, source.as_str()),
        (view.revision, "fallback")
    );
    assert!(after.read_only);
    assert_eq!(rig.write_count(&file_path("f")), 0);
    assert_eq!(rig.write_count(&content_path("f")), 0);
}

// A2-6

#[test]
fn create_in_a_space_with_no_terminal_succeeds_on_the_first_call() {
    let rig = PanelRig::new();
    let created = rig.create(named("p", markdown("one"))).unwrap();
    assert_eq!(created.id, "p");
    rig.wait_state("p", PanelState::Validated);
}

// #34: the hash check before every write

#[test]
fn an_outside_edit_before_the_comparing_read_is_not_overwritten() {
    let (rig, racing) = PanelRig::racing();
    rig.create(named("p", markdown("one"))).unwrap();
    rig.wait_state("p", PanelState::Validated);
    settled_read_count(&rig.store, &rig.space, &content_path("p"), 1);
    let writes = rig.write_count(&content_path("p"));

    racing.arm(
        &rig.space,
        &content_path("p"),
        Race::BeforeCompare,
        b"theirs".to_vec(),
    );
    rig.panels
        .update(&rig.space, "p", update_content("ours"))
        .unwrap();

    assert!(!racing.armed(), "the update never read the file");
    let (view, _) = wait_for(&rig, "p", "took the outside edit", |view, source| {
        source == "theirs" && view.state == PanelState::Validated
    });
    assert!(view.dropped.is_some());
    assert_eq!(rig.bytes(&content_path("p")).unwrap(), b"theirs");
    assert_eq!(rig.write_count(&content_path("p")), writes);
    assert_eq!(rig.connections(), 0);
}

#[test]
fn the_window_after_the_comparing_read_overwrites() {
    let (rig, racing) = PanelRig::racing();
    rig.create(named("p", markdown("one"))).unwrap();
    rig.wait_state("p", PanelState::Validated);
    settled_read_count(&rig.store, &rig.space, &content_path("p"), 1);
    let writes = rig.write_count(&content_path("p"));

    racing.arm(
        &rig.space,
        &content_path("p"),
        Race::AfterCompare,
        b"theirs".to_vec(),
    );
    rig.panels
        .update(&rig.space, "p", update_content("ours"))
        .unwrap();

    assert!(!racing.armed(), "the update never read the file");
    assert_eq!(rig.bytes(&content_path("p")).unwrap(), b"ours");
    assert_eq!(rig.write_count(&content_path("p")), writes + 1);
    std::thread::sleep(Duration::from_millis(4 * TEST_TIMING.max_wait_ms));
    let (view, source) = rig.get("p").unwrap();
    assert_eq!(
        (view.state, source.as_str()),
        (PanelState::Validated, "ours")
    );
    assert_eq!(rig.connections(), 0);
}

const THEIRS_JSON: &[u8] = br#"{"version":1,"renderer":"markdown","title":"theirs"}"#;

fn skipped_x() -> Vec<SkippedPanel> {
    vec![SkippedPanel {
        panel: "x".to_string(),
        reason: SCRATCH_ID_OPEN.to_string(),
    }]
}

/// A space that is listed, with `x` made by hand in the debounce window: the folder's change
/// events are not applied yet.
fn rig_with_unseen_folder() -> PanelRig {
    let rig = PanelRig::new();
    rig.create(named("seed", scratch(markdown("s")))).unwrap();
    rig.store
        .write_quiet(&rig.space, &file_path("x"), THEIRS_JSON.to_vec());
    rig.store
        .write_quiet(&rig.space, &content_path("x"), b"theirs".to_vec());
    rig
}

fn assert_theirs_untouched(rig: &PanelRig) {
    assert_eq!(rig.bytes(&file_path("x")).unwrap(), THEIRS_JSON);
    assert_eq!(rig.bytes(&content_path("x")).unwrap(), b"theirs");
    for path in [file_path("x"), content_path("x")] {
        assert_eq!(rig.write_count(&path), 0, "{path}");
        assert_eq!(rig.remove_count(&path), 0, "{path}");
    }
}

#[test]
fn a_create_never_writes_into_a_folder_whose_panel_json_it_has_not_seen() {
    let rig = rig_with_unseen_folder();
    let created = rig.create(named("x", markdown("ours"))).unwrap();

    let (view, _) = wait_for(&rig, "x", "adopted the folder", |view, source| {
        view.state == PanelState::Validated && source == "theirs"
    });
    assert_eq!(view.lifetime, Lifetime::Persistent);
    assert_eq!(view.file.title.as_deref(), Some("theirs"));
    assert!(view.revision > created.revision);
    assert_theirs_untouched(&rig);
    let list = rig.panels.list(&rig.space).unwrap();
    assert_eq!(list.panels.iter().filter(|view| view.id == "x").count(), 1);

    let again = rig.create(named("x", markdown("more"))).unwrap_err();
    assert_eq!(
        again,
        PanelApiError::IdTaken {
            id: "x".to_string()
        }
    );
    assert_eq!(again.to_string(), "the space already has a panel folder x");
    assert_eq!(rig.connections(), 0);
}

#[test]
fn a_pin_never_writes_into_a_folder_whose_panel_json_it_has_not_seen() {
    let rig = PanelRig::new();
    rig.create(named("x", scratch(markdown("s1")))).unwrap();
    rig.store
        .write_quiet(&rig.space, &file_path("x"), THEIRS_JSON.to_vec());
    rig.store
        .write_quiet(&rig.space, &content_path("x"), b"theirs".to_vec());

    rig.panels.pin(&rig.space, "x").unwrap();

    assert_theirs_untouched(&rig);
    let (view, source) = rig.get("x").unwrap();
    assert_eq!((view.lifetime, source.as_str()), (Lifetime::Scratch, "s1"));
    assert_eq!(
        view.file_error.as_deref(),
        Some("Couldn't pin: .workspace/panels/x/panel.json changed while it was written")
    );
    let list = rig.panels.list(&rig.space).unwrap();
    assert_eq!(list.panels.iter().filter(|view| view.id == "x").count(), 1);
    assert_eq!(list.skipped, skipped_x());
    let dir = ScratchDir {
        key: rig.space.sha256_hex(),
        panel: "x".to_string(),
    };
    assert!(rig.scratch.dirs().contains(&dir));

    let closed = rig.panels.close(&rig.space, "x").unwrap();
    assert_eq!(closed.state, PanelState::Removed);
    let (adopted, _) = wait_for(&rig, "x", "adopted the folder", |view, source| {
        view.lifetime == Lifetime::Persistent
            && view.state == PanelState::Validated
            && source == "theirs"
    });
    assert_eq!(adopted.file.title.as_deref(), Some("theirs"));
    assert_theirs_untouched(&rig);
    assert!(rig.scratch.dirs().is_empty());
    assert_eq!(rig.connections(), 0);
}

#[test]
fn create_overwrites_a_leftover_content_file_in_a_folder_with_no_panel_json() {
    let rig = PanelRig::new();
    rig.create(named("seed", scratch(markdown("s")))).unwrap();
    rig.store
        .write_quiet(&rig.space, &content_path("x"), b"old".to_vec());

    rig.create(named("x", markdown("new"))).unwrap();

    assert_eq!(rig.bytes(&content_path("x")).unwrap(), b"new");
    assert_eq!(rig.write_count(&content_path("x")), 1);
    assert_eq!(rig.write_count(&file_path("x")), 1);
    let (view, source) = rig.wait_state("x", PanelState::Validated);
    assert_eq!(
        (view.lifetime, source.as_str()),
        (Lifetime::Persistent, "new")
    );
    assert_eq!(rig.connections(), 0);
}
