#![cfg(feature = "test-support")]
//! Listing templates and spawning terminals through the running server: a real Unix socket, the
//! fake backend and an in-memory or real file store.

mod daemon_support;
mod spawn_support;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::sync::Arc;

use std::time::Duration;

use combine_protocol::{
    ClientMessage, DaemonMessage, ErrorCode, ListTemplates, RequestId, SpawnSource, WatchTerminals,
};
use combined::adapters::fs::file_store::FsFileStore;
use combined::core::{PANELS_DIR, PANEL_MAX_BYTES};
use combined::model::SpawnSpec;
use combined::testing::{FakeCall, FakeControl};
use daemon_support::{make_fifo, release_fifo, work_dir};
use serde_json::json;
use spawn_support::*;

/// Long enough for any reply from a daemon that isn't blocked.
const PROMPTLY: Duration = Duration::from_secs(3);

fn spawn_calls(control: &FakeControl) -> usize {
    control
        .calls()
        .iter()
        .filter(|call| matches!(call, FakeCall::Spawn { .. }))
        .count()
}

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// Spawning `panel`, with the hash of its file as the store has it now, is refused with `code`,
/// and the backend is not asked to start anything.
fn assert_refused(rig: &Rig, panel: &str, code: ErrorCode) -> String {
    let mut client = rig.client();
    let before = (spawn_calls(&rig.control), spawn_attempts(&rig.control));
    rig.spawn_template(&mut client, 1, panel, &rig.current_hash(panel));
    let error = expect_error(&client, 1, code);
    assert_eq!(
        (spawn_calls(&rig.control), spawn_attempts(&rig.control)),
        before
    );
    error.message
}

#[test]
fn spawn_shell_starts_in_project_root() {
    let (rig, _store) = Rig::in_memory();
    let mut client = rig.client();
    let before = watch_terminals(&mut client, 1);
    assert_eq!(before.terminals.len(), 1);

    rig.spawn(&mut client, 2, SpawnSource::Shell);
    let mut spawned: Option<combine_protocol::Spawned> = None;
    let mut listed: Vec<combine_protocol::TermId> = Vec::new();
    while spawned.is_none()
        || !spawned
            .as_ref()
            .is_some_and(|s| listed.contains(&s.terminal))
    {
        match client.recv() {
            DaemonMessage::Spawned(reply) => {
                assert_eq!(reply.request, RequestId(2));
                spawned = Some(reply);
            }
            DaemonMessage::Terminals(push) => {
                let space = push.spaces.iter().find(|space| space.id == rig.name());
                listed = space
                    .map(|space| space.terminals.clone())
                    .unwrap_or_default();
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(spawned.unwrap().space, rig.name());

    let specs = rig.control.spawn_specs();
    assert_eq!(specs.len(), 2);
    assert_eq!(
        specs[1],
        SpawnSpec {
            argv: Vec::new(),
            cwd: rig.name(),
            env: BTreeMap::new(),
            labels: BTreeMap::new(),
            cols: 80,
            rows: 24,
        }
    );
}

#[test]
fn spawn_template_runs_file_argv_env_labels_and_size() {
    let (rig, store) = Rig::in_memory();
    fs::create_dir_all(work_dir(&rig.scratch).join("sub")).unwrap();
    let bytes = panel_json_of(json!({
        "version": 1,
        "content": {
            "type": "terminal",
            "slot": "main",
            "template": {
                "argv": ["claude", "--resume"],
                "cwd": "sub",
                "env": { "MODE": "plan" },
                "labels": { "role": "agent" },
                "cols": 100,
                "rows": 30
            }
        }
    }));
    write_panel_memory(&store, &rig.space, "agent", &bytes);

    let mut client = rig.client();
    rig.spawn_template(&mut client, 1, "agent", &hash_hex(&bytes));
    let spawned = expect_spawned(&client, 1);

    let specs = rig.control.spawn_specs();
    assert_eq!(specs.len(), 2);
    assert_eq!(
        specs[1],
        SpawnSpec {
            argv: vec!["claude".to_string(), "--resume".to_string()],
            cwd: format!("{}/sub", rig.name()),
            env: map(&[("MODE", "plan")]),
            labels: map(&[("role", "agent")]),
            cols: 100,
            rows: 30,
        }
    );
    assert!(!spawned.terminal.0.is_empty());
}

#[test]
fn list_templates_returns_templates_in_name_order() {
    let (rig, store) = Rig::in_memory();
    for panel in ["b", "c", "a"] {
        write_panel_memory(
            &store,
            &rig.space,
            panel,
            &terminal_panel_json(&["sh"], None),
        );
    }
    let mut client = rig.client();
    let templates = rig.list(&mut client, 1);
    let names: Vec<&str> = templates
        .templates
        .iter()
        .map(|t| t.panel.as_str())
        .collect();
    assert_eq!(names, ["a", "b", "c"]);
    assert_eq!(templates.space, rig.name());
    assert_eq!(templates.error, None);
    assert_eq!(spawn_attempts(&rig.control), 1);
}

#[test]
fn templates_carry_argv_env_cwd_exactly() {
    let (rig, store) = Rig::in_memory();
    fs::create_dir_all(work_dir(&rig.scratch).join("sub")).unwrap();
    let odd = "line\nbreak \u{202e}flipped";
    let bytes = panel_json_of(json!({
        "version": 1,
        "title": "Odd",
        "content": {
            "type": "terminal",
            "slot": "main",
            "template": {
                "argv": ["sh", odd, ""],
                "cwd": "sub",
                "env": { "NOTE": odd }
            }
        }
    }));
    write_panel_memory(&store, &rig.space, "odd", &bytes);

    let mut client = rig.client();
    let templates = rig.list(&mut client, 1);
    assert_eq!(templates.templates.len(), 1);
    let template = &templates.templates[0];
    assert_eq!(template.panel, "odd");
    assert_eq!(template.title.as_deref(), Some("Odd"));
    assert_eq!(template.slot, "main");
    assert_eq!(template.argv, ["sh", odd, ""]);
    assert_eq!(template.env, map(&[("NOTE", odd)]));
    assert_eq!(template.cwd, "sub");
    assert_eq!(template.hash, hash_hex(&bytes));
}

#[test]
fn template_cwd_escaping_with_dotdot_refused() {
    let (rig, store) = Rig::in_memory();
    write_panel_memory(
        &store,
        &rig.space,
        "up",
        &terminal_panel_json(&["sh"], Some("../elsewhere")),
    );
    assert_refused(&rig, "up", ErrorCode::CwdRefused);
}

#[test]
fn absolute_cwd_refused() {
    let (rig, store) = Rig::in_memory();
    write_panel_memory(
        &store,
        &rig.space,
        "abs",
        &terminal_panel_json(&["sh"], Some("/tmp")),
    );
    assert_refused(&rig, "abs", ErrorCode::CwdRefused);
}

#[test]
fn symlinked_cwd_outside_root_refused() {
    let (rig, store) = Rig::in_memory();
    let outside = rig.scratch.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    symlink(&outside, work_dir(&rig.scratch).join("escape")).unwrap();
    write_panel_memory(
        &store,
        &rig.space,
        "link",
        &terminal_panel_json(&["sh"], Some("escape")),
    );
    assert_refused(&rig, "link", ErrorCode::CwdRefused);
}

#[test]
fn git_cwd_refused_direct_and_via_symlink() {
    let (rig, store) = Rig::in_memory();
    symlink(
        work_dir(&rig.scratch).join(".git"),
        work_dir(&rig.scratch).join("gitlink"),
    )
    .unwrap();
    for (panel, cwd) in [("direct", ".git"), ("via", "gitlink")] {
        write_panel_memory(
            &store,
            &rig.space,
            panel,
            &terminal_panel_json(&["sh"], Some(cwd)),
        );
        assert_refused(&rig, panel, ErrorCode::CwdRefused);
    }
}

#[test]
fn unknown_template_refused() {
    let (rig, _store) = Rig::in_memory();
    assert_refused(&rig, "nothing", ErrorCode::UnknownTemplate);
}

#[test]
fn invalid_panel_id_refused() {
    let (rig, _store) = Rig::in_memory();
    assert_refused(&rig, "../escape", ErrorCode::InvalidRequest);
}

#[test]
fn backend_failure_is_spawn_failed() {
    let (rig, _store) = Rig::in_memory();
    rig.control
        .fail_next_spawn(combined::ports::BackendError::Failed("no pty".to_string()));
    let mut client = rig.client();
    let calls = spawn_calls(&rig.control);
    let attempts = spawn_attempts(&rig.control);

    rig.spawn(&mut client, 1, SpawnSource::Shell);
    expect_error(&client, 1, ErrorCode::SpawnFailed);

    assert_eq!(spawn_attempts(&rig.control), attempts + 1);
    assert_eq!(spawn_calls(&rig.control), calls);
    let snapshot = watch_terminals(&mut client, 2);
    assert_eq!(snapshot.terminals.len(), 1);
}

#[test]
fn list_skips_invalid_oversized_and_view_panels_others_load() {
    let (rig, store) = Rig::in_memory();
    write_panel_memory(
        &store,
        &rig.space,
        "good",
        &terminal_panel_json(&["sh"], None),
    );
    write_panel_memory(&store, &rig.space, "broken", b"{ not json");
    let size = usize::try_from(PANEL_MAX_BYTES).unwrap();
    let mut huge = terminal_panel_json(&["sh"], None);
    huge.resize(size + 1, b' ');
    write_panel_memory(&store, &rig.space, "huge", &huge);
    write_panel_memory(
        &store,
        &rig.space,
        "viewer",
        &panel_json_of(json!({
            "version": 1,
            "content": { "type": "view", "slot": "main" }
        })),
    );

    let mut client = rig.client();
    let templates = rig.list(&mut client, 1);
    let names: Vec<&str> = templates
        .templates
        .iter()
        .map(|t| t.panel.as_str())
        .collect();
    assert_eq!(names, ["good"]);
    let skipped: Vec<&str> = templates.skipped.iter().map(|s| s.panel.as_str()).collect();
    assert_eq!(skipped, ["broken", "huge", "viewer"]);
    assert!(templates.skipped.iter().all(|s| !s.reason.is_empty()));
}

#[test]
fn symlinked_panel_folder_and_panel_json_skipped() {
    let store = Arc::new(FsFileStore::new());
    let rig = Rig::new(store);
    let work = work_dir(&rig.scratch);
    write_panel_fs(&work, "ok", &terminal_panel_json(&["sh"], None));
    let panels = work.join(".workspace").join(PANELS_DIR);

    symlink(panels.join("ok"), panels.join("folderlink")).unwrap();

    let target = rig.scratch.path().join("elsewhere.json");
    fs::write(&target, terminal_panel_json(&["sh"], None)).unwrap();
    fs::create_dir_all(panels.join("filelink")).unwrap();
    symlink(&target, panels.join("filelink/panel.json")).unwrap();

    let mut client = rig.client();
    let templates = rig.list(&mut client, 1);
    let names: Vec<&str> = templates
        .templates
        .iter()
        .map(|t| t.panel.as_str())
        .collect();
    assert_eq!(names, ["ok"]);
    let skipped: Vec<&str> = templates.skipped.iter().map(|s| s.panel.as_str()).collect();
    assert_eq!(skipped, ["filelink", "folderlink"]);
}

#[test]
fn hundred_max_size_templates_fit_one_frame() {
    let (rig, store) = Rig::in_memory();
    let size = usize::try_from(PANEL_MAX_BYTES).unwrap();
    let base = terminal_panel_json(&["sh", ""], None).len();
    let pad = "x".repeat(size - base);
    let bytes = terminal_panel_json(&["sh", &pad], None);
    assert_eq!(bytes.len(), size);
    for n in 0..100 {
        write_panel_memory(&store, &rig.space, &format!("p{n:03}"), &bytes);
    }

    let mut client = rig.client();
    let templates = rig.list(&mut client, 1);
    assert_eq!(templates.templates.len(), 100);
    assert_eq!(templates.error, None);
    assert!(templates.templates.iter().all(|t| t.argv[1] == pad));
}

#[test]
fn over_one_hundred_folders_sets_error() {
    let (rig, store) = Rig::in_memory();
    for n in 0..101 {
        write_panel_memory(
            &store,
            &rig.space,
            &format!("p{n:03}"),
            &terminal_panel_json(&["sh"], None),
        );
    }
    let mut client = rig.client();
    let templates = rig.list(&mut client, 1);
    assert_eq!(templates.templates.len(), 100);
    assert!(templates.error.is_some());
}

/// The review's reproduction: the menu shows one valid definition, the file is replaced by
/// another valid one, and the click that names the shown definition must not run the new one.
#[test]
fn replaced_template_is_refused_and_choosing_again_runs_the_new_one() {
    let rig = Rig::new(Arc::new(FsFileStore::new()));
    let work = work_dir(&rig.scratch);
    let reviewed = terminal_panel_json(&["echo", "reviewed"], None);
    write_panel_fs(&work, "agent", &reviewed);
    let mut client = rig.client();
    let shown = rig.list(&mut client, 1);
    assert_eq!(shown.templates[0].argv, ["echo", "reviewed"]);
    assert_eq!(shown.templates[0].hash, hash_hex(&reviewed));
    let specs_before = rig.control.spawn_specs().len();

    let unreviewed = terminal_panel_json(&["sh", "-c", "echo unreviewed"], None);
    write_panel_fs(&work, "agent", &unreviewed);
    rig.spawn_template(&mut client, 2, "agent", &shown.templates[0].hash);
    let error = expect_error(&client, 2, ErrorCode::TemplateChanged);
    assert_eq!(
        error.message,
        "the panel agent changed since it was listed; review it and choose again"
    );
    assert_eq!(rig.control.spawn_specs().len(), specs_before);
    assert_eq!(spawn_calls(&rig.control), specs_before);

    // The refreshed menu shows the new definition; choosing it runs exactly that.
    let again = rig.list(&mut client, 3);
    assert_eq!(again.templates[0].argv, ["sh", "-c", "echo unreviewed"]);
    assert_eq!(again.templates[0].hash, hash_hex(&unreviewed));
    rig.spawn_template(&mut client, 4, "agent", &again.templates[0].hash);
    expect_spawned(&client, 4);
    let specs = rig.control.spawn_specs();
    assert_eq!(specs.len(), specs_before + 1);
    assert_eq!(specs.last().unwrap().argv, ["sh", "-c", "echo unreviewed"]);
}

/// A template source without `hash` is a malformed frame: the daemon answers `malformed`,
/// closes the connection and starts nothing.
#[test]
fn template_source_without_hash_is_malformed() {
    let (rig, store) = Rig::in_memory();
    write_panel_memory(
        &store,
        &rig.space,
        "agent",
        &terminal_panel_json(&["sh"], None),
    );
    let mut client = rig.client();
    let before = spawn_attempts(&rig.control);
    let json = json!({
        "type": "spawn_terminal",
        "request": 1,
        "space": rig.name(),
        "source": { "type": "template", "panel": "agent" }
    })
    .to_string();
    let mut frame = u32::try_from(json.len() + 1)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    frame.push(1);
    frame.extend_from_slice(json.as_bytes());
    client.send_bytes(&frame);
    match client.recv() {
        DaemonMessage::Error(error) => {
            assert_eq!(error.code, ErrorCode::Malformed);
            assert!(
                error.message.contains("`template` is missing `hash`"),
                "{}",
                error.message
            );
        }
        other => panic!("expected the malformed error, got {other:?}"),
    }
    client.expect_closed();
    assert_eq!(spawn_attempts(&rig.control), before);
}

/// A FIFO at a candidate's `panel.json` is skipped with a reason, never opened for a blocking
/// read: the listing answers at once, and another client is served meanwhile.
#[test]
fn fifo_panel_file_is_skipped_without_blocking_the_daemon() {
    let rig = Rig::new(Arc::new(FsFileStore::new()));
    let work = work_dir(&rig.scratch);
    write_panel_fs(&work, "good", &terminal_panel_json(&["sh"], None));
    let fifo = work.join(".workspace/panels/agent/panel.json");
    make_fifo(&fifo);

    let mut lister = rig.client();
    let mut other = rig.client();
    lister.send(&ClientMessage::ListTemplates(ListTemplates {
        request: RequestId(1),
        space: rig.name(),
    }));
    let listed = lister.recv_within(PROMPTLY);
    other.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(2),
    }));
    let watched = other.recv_within(PROMPTLY);
    // Frees a daemon stuck on the FIFO, so a regression fails below instead of hanging.
    release_fifo(&fifo, &terminal_panel_json(&["echo", "late"], None));

    let Some(DaemonMessage::Templates(templates)) = listed else {
        panic!("no templates reply while a FIFO sat at panel.json: {listed:?}");
    };
    assert_eq!(templates.request, RequestId(1));
    let names: Vec<&str> = templates
        .templates
        .iter()
        .map(|t| t.panel.as_str())
        .collect();
    assert_eq!(names, ["good"]);
    assert_eq!(templates.skipped.len(), 1);
    assert_eq!(templates.skipped[0].panel, "agent");
    assert_eq!(
        templates.skipped[0].reason,
        "couldn't read panel.json (.workspace/panels/agent/panel.json is not a regular file)"
    );
    assert!(
        matches!(watched, Some(DaemonMessage::Terminals(_))),
        "the other client got no terminal list: {watched:?}"
    );

    // A spawn naming it is refused the same way, and starts nothing.
    let before = spawn_attempts(&rig.control);
    rig.spawn_template(&mut lister, 3, "agent", NO_HASH);
    let error = lister.recv_within(PROMPTLY);
    release_fifo(&fifo, b"");
    let Some(DaemonMessage::Error(error)) = error else {
        panic!("no reply to a spawn of the FIFO panel: {error:?}");
    };
    assert_eq!(error.code, ErrorCode::UnknownTemplate);
    assert!(
        error.message.ends_with("is not a regular file)"),
        "{}",
        error.message
    );
    assert_eq!(spawn_attempts(&rig.control), before);
}
