#![cfg(feature = "test-support")]
//! The panel lane: the panel store's file work runs on a blocking thread, so a stalled store
//! doesn't hold terminal output, and a call, a check result or an outside edit waits for it.
//! The store is stalled by blocking its thread inside a read or a listing, and the tests
//! assert on the store's state and its record of calls, never on how long an answer took.

mod common;
mod daemon_support;
mod panel_support;

use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use combine_protocol::{
    ClientMessage, DaemonMessage, GetPanel, Panel, PanelState, RequestId, TermId,
};
use combined::core::{PanelApiError, PanelUpdate, PanelView};
use combined::model::SpaceId;
use daemon_support::{wait_until, WireClient};
use panel_support::{
    attach, markdown, mermaid, next_panels, read_until_tail, settled_read_count, watch_panels,
    PanelRig, StallingStore, StoreOp, WAIT,
};
use std::sync::Arc;

/// How long a test waits to be sure something doesn't happen.
const QUIET: Duration = Duration::from_millis(300);

const CONTENT: &str = "panels/p/content.md";

fn content_update(content: &str) -> PanelUpdate {
    PanelUpdate {
        content: Some(content.to_string()),
        title: None,
    }
}

/// A rig on a stalling store whose project is a space with its panels read and a validated
/// Markdown panel `p`, read as often as it will be.
fn rig_with_p() -> (PanelRig, Arc<StallingStore>, TermId) {
    let (rig, store) = PanelRig::stalling();
    let term = rig.spawn_terminal();
    assert!(
        wait_until(WAIT, || rig.panels.list(&rig.space).is_ok()),
        "the space's panels were never read"
    );
    rig.create(panel_support::named("p", markdown("base")))
        .unwrap();
    rig.wait_state("p", PanelState::Validated);
    settled_read_count(&rig.store, &rig.space, CONTENT, 1);
    (rig, store, term)
}

fn get_panel(client: &mut WireClient, rig: &PanelRig, id: &str, request: u64) -> Panel {
    client.send(&ClientMessage::GetPanel(GetPanel {
        request: RequestId(request),
        space: rig.space.0.clone(),
        panel: id.to_string(),
    }));
    loop {
        match client.recv() {
            DaemonMessage::Panel(panel) if panel.request == RequestId(request) => return panel,
            DaemonMessage::Panel(_) | DaemonMessage::Panels(_) => {}
            other => panic!("expected the panel, got {other:?}"),
        }
    }
}

/// Runs `update` of `id` on its own thread, which stalls while the lane does.
fn update_on_a_thread(
    rig: &PanelRig,
    id: &str,
    content: &str,
) -> (
    thread::JoinHandle<()>,
    mpsc::Receiver<Result<PanelView, PanelApiError>>,
) {
    let panels = rig.panels.clone();
    let space = rig.space.clone();
    let id = id.to_string();
    let content = content.to_string();
    let (done, answer) = mpsc::channel();
    let thread = thread::spawn(move || {
        let _ = done.send(panels.update(&space, &id, content_update(&content)));
    });
    (thread, answer)
}

#[test]
fn a_stalled_panel_read_leaves_terminal_frames_flowing() {
    let (rig, store, term) = rig_with_p();
    let mut client = rig.client();
    let attachment = attach(&mut client, &term);
    let (before, _) = rig.get("p").unwrap();
    let stall = store.stall(&rig.space, CONTENT);
    rig.store
        .write_outside(&rig.space, CONTENT, b"outside".to_vec());
    assert!(
        wait_until(WAIT, || store.entered()),
        "the stalled read never started"
    );

    for round in 1..=2 {
        let marker = format!("marker-{round}");
        rig.control.emit(&term, marker.as_bytes());
        assert!(
            read_until_tail(
                &mut client,
                attachment,
                marker.as_bytes(),
                Duration::from_secs(2)
            )
            .is_some(),
            "terminal output stopped flowing while a panel read was stalled ({round})"
        );
        thread::sleep(Duration::from_secs(1));
    }

    stall.release();
    assert!(
        wait_until(WAIT, || {
            matches!(rig.get("p"), Ok((view, source))
                if view.revision > before.revision && view.state == PanelState::Validated && source == "outside")
        }),
        "the outside edit was never applied after the stall ended"
    );
}

#[test]
fn calls_wait_while_panel_file_work_runs() {
    let (rig, store, _term) = rig_with_p();
    let mut client = rig.client();
    watch_panels(&mut client, &rig.space, 1);

    let stall = store.stall(&rig.space, CONTENT);
    let (a, a_answer) = update_on_a_thread(&rig, "p", "u1");
    assert!(
        wait_until(WAIT, || store.entered()),
        "the first update never reached its comparing read"
    );
    let before_b = store.log().len();
    // The first update is applied to the store and is saving: this is r1.
    let r1 = get_panel(&mut client, &rig, "p", 10).revision;
    while next_panels(&client, Duration::from_millis(50)).is_some() {}

    let (b, b_answer) = update_on_a_thread(&rig, "p", "u2");
    thread::sleep(QUIET);

    // (a) the second update is not applied: the client still sees r1.
    assert_eq!(get_panel(&mut client, &rig, "p", 11).revision, r1);
    // (b) no list pushed since lists the panel above r1.
    while let Some(panels) = next_panels(&client, Duration::from_millis(50)) {
        let above = panels.panels.iter().any(|panel| panel.revision > r1);
        assert!(
            !above,
            "a push listed a newer revision during the stall: {panels:?}"
        );
    }
    // (c) the store saw no write of u2 and no further read of the panel's files.
    let seen = store.log()[before_b..].to_vec();
    assert!(
        !seen
            .iter()
            .any(|op| matches!(op, StoreOp::Write(_, bytes) if bytes == b"u2")),
        "u2 was written during the stall: {seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|op| matches!(op, StoreOp::Read(path) if path.starts_with("panels/p/"))),
        "the panel was read again during the stall: {seen:?}"
    );
    // (d) the second call hasn't returned.
    assert!(
        b_answer.try_recv().is_err(),
        "the second update returned during the stall"
    );

    stall.release();
    let first = a_answer
        .recv_timeout(WAIT)
        .expect("no answer to the first update");
    let second = b_answer
        .recv_timeout(WAIT)
        .expect("no answer to the second update");
    a.join().unwrap();
    b.join().unwrap();
    // (e) the first answers r1, the second a later revision.
    assert_eq!(first.unwrap().revision, r1);
    let r2 = second.unwrap().revision;
    assert!(r2 > r1, "{r2} is not above {r1}");
    // (f) u1 was written before u2.
    let writes: Vec<Vec<u8>> = store
        .log()
        .into_iter()
        .filter_map(|op| match op {
            StoreOp::Write(path, bytes) if path == CONTENT => Some(bytes),
            _ => None,
        })
        .collect();
    let u1 = writes
        .iter()
        .position(|bytes| bytes == b"u1")
        .expect("u1 was never written");
    let u2 = writes
        .iter()
        .position(|bytes| bytes == b"u2")
        .expect("u2 was never written");
    assert!(u1 < u2, "{writes:?}");
    // (g) the content ends as u2, validated at r2, and nothing newer follows.
    let (view, source) = rig.wait_state("p", PanelState::Validated);
    assert_eq!(source, "u2");
    assert_eq!(view.revision, r2);
    thread::sleep(QUIET);
    assert_eq!(rig.get("p").unwrap().0.revision, r2);
}

#[test]
fn check_results_wait_while_panel_file_work_runs() {
    let (rig, store, _term) = rig_with_p();
    let mut client = rig.client();
    let source = "graph TD; A-->B";
    let hold = rig.check.hold(source);
    let q = rig
        .create(panel_support::named("q", mermaid(source)))
        .unwrap();
    assert_eq!(q.state, PanelState::Validating);

    let stall = store.stall(&rig.space, CONTENT);
    let (a, a_answer) = update_on_a_thread(&rig, "p", "u1");
    assert!(
        wait_until(WAIT, || store.entered()),
        "the update never stalled"
    );
    hold.release();
    thread::sleep(QUIET);

    assert_eq!(
        get_panel(&mut client, &rig, "q", 20).state,
        PanelState::Validating,
        "a check result was applied while the lane was busy"
    );

    stall.release();
    a_answer
        .recv_timeout(WAIT)
        .expect("no answer to the update")
        .unwrap();
    a.join().unwrap();
    rig.wait_state("q", PanelState::Validated);
}

#[test]
fn an_answer_is_not_held_behind_deferred_work() {
    let (rig, store, _term) = rig_with_p();
    let first = store.stall(&rig.space, CONTENT);
    let (a, a_answer) = update_on_a_thread(&rig, "p", "u1");
    assert!(
        wait_until(WAIT, || store.entered()),
        "the update never stalled"
    );

    let w2 = rig.dir.path().join("w2");
    fs::create_dir_all(w2.join(".git")).unwrap();
    let w2_space = SpaceId(w2.to_str().unwrap().to_string());
    let second = store.stall_list(&w2_space, "panels");
    rig.spawn_terminal_in(&w2);
    thread::sleep(QUIET);
    first.release();

    let answer = a_answer
        .recv_timeout(Duration::from_secs(2))
        .expect("the update's answer waited behind the new space's listing");
    answer.unwrap();
    a.join().unwrap();
    assert!(
        wait_until(WAIT, || store.entered()),
        "the new space's listing never started"
    );
    second.release();
    assert!(wait_until(WAIT, || rig.panels.list(&w2_space).is_ok()));
}

#[test]
fn a_create_in_a_new_space_waits_for_its_listing() {
    let (rig, store, _term) = rig_with_p();
    let w2 = rig.dir.path().join("w2");
    fs::create_dir_all(w2.join(".git")).unwrap();
    let w2_space = SpaceId(w2.to_str().unwrap().to_string());
    let held = store.stall_list(&w2_space, "panels");
    rig.spawn_terminal_in(&w2);
    assert!(
        wait_until(WAIT, || store.entered()),
        "the listing never started"
    );

    let panels = rig.panels.clone();
    let space = w2_space.clone();
    let (done, created) = mpsc::channel();
    let thread = thread::spawn(move || {
        let _ = done.send(panels.create(&space, markdown("x")));
    });
    thread::sleep(QUIET);
    assert!(
        created.try_recv().is_err(),
        "the create ran before its space was listed"
    );

    held.release();
    let made = created
        .recv_timeout(WAIT)
        .expect("the create never ran")
        .expect("the create found its space loading");
    thread.join().unwrap();
    assert_eq!(made.space, w2_space);
}

#[test]
fn async_handle_methods_work_inside_a_runtime() {
    let rig = PanelRig::new();
    rig.spawn_terminal();
    assert!(
        wait_until(WAIT, || rig.panels.list(&rig.space).is_ok()),
        "the space's panels were never read"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let panels = rig.panels.clone();
    let space = rig.space.clone();

    runtime.block_on(async {
        let made = panels.create_async(&space, markdown("one")).await.unwrap();
        let (view, source) = panels.get_async(&space, &made.id).await.unwrap();
        assert_eq!(
            (view.id.as_str(), source.as_str()),
            (made.id.as_str(), "one")
        );
        assert_eq!(
            panels.render_status_async(&space, &made.id).await.unwrap(),
            Vec::new()
        );
        assert!(panels.status_async(&space, &made.id).await.is_ok());
        assert_eq!(panels.list_async(&space).await.unwrap().panels.len(), 1);

        let blocked = catch_unwind(AssertUnwindSafe(|| panels.create(&space, markdown("two"))));
        assert!(blocked.is_err(), "the blocking create ran inside a runtime");
    });
}
