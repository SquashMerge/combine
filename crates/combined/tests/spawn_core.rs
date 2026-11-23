#![cfg(feature = "test-support")]
//! The spawn state machine: listing templates and starting terminals only on request.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use combine_protocol::{DaemonMessage, ErrorCode, RequestId, SkippedPanel, SpawnSource, TermId};
use combined::core::{
    ConnectionId, PanelRead, SpawnEffect, SpawnTicket, Spawns, MAX_SPAWN_IN_FLIGHT,
};
use combined::model::{ContentHash, SpaceId, SpawnSpec};
use combined::ports::{
    BackendError, DirListing, DirRefusal, EntryKind, ListedEntry, ProjectDirs, StoreError,
};
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(Clone, Default)]
struct Dirs(Arc<Mutex<BTreeMap<String, DirRefusal>>>);

impl Dirs {
    fn refuse(&self, relative: &str, refusal: DirRefusal) {
        self.0.lock().unwrap().insert(relative.to_owned(), refusal);
    }
}

impl ProjectDirs for Dirs {
    fn check_dir(&self, _root: &SpaceId, relative: &str) -> Result<(), DirRefusal> {
        match self.0.lock().unwrap().get(relative) {
            Some(refusal) => Err(refusal.clone()),
            None => Ok(()),
        }
    }
}

const CONN: ConnectionId = ConnectionId(1);

fn setup() -> (Spawns, Dirs) {
    let dirs = Dirs::default();
    (Spawns::new(Box::new(dirs.clone())), dirs)
}

fn space() -> SpaceId {
    SpaceId("/p".to_owned())
}

fn req(n: u64) -> RequestId {
    RequestId(n)
}

/// A template source carrying a hash that no test file has.
fn template(panel: &str) -> SpawnSource {
    SpawnSource::Template {
        panel: panel.to_owned(),
        hash: "0".repeat(64),
    }
}

fn hash_of(bytes: &[u8]) -> ContentHash {
    ContentHash(Sha256::digest(bytes).into())
}

/// A template source for the definition `bytes` hold, as a listing showed it.
fn template_for(panel: &str, bytes: &[u8]) -> SpawnSource {
    SpawnSource::Template {
        panel: panel.to_owned(),
        hash: hash_of(bytes).to_hex(),
    }
}

/// What the store gives for a file holding `bytes`.
fn found(bytes: Vec<u8>) -> PanelRead {
    PanelRead::Found {
        hash: hash_of(&bytes),
        bytes,
    }
}

fn panel_json(cwd: &str) -> Vec<u8> {
    json!({
        "version": 1,
        "content": {
            "type": "terminal",
            "slot": "main",
            "template": { "argv": ["claude"], "cwd": cwd }
        }
    })
    .to_string()
    .into_bytes()
}

fn entry(name: &str, kind: EntryKind) -> ListedEntry {
    ListedEntry {
        name: name.to_owned(),
        kind,
    }
}

fn error_of(message: &DaemonMessage) -> (ErrorCode, String, Option<RequestId>) {
    match message {
        DaemonMessage::Error(e) => (e.code, e.message.clone(), e.request),
        other => panic!("not an error: {other:?}"),
    }
}

fn only_reply(spawns: &mut Spawns) -> DaemonMessage {
    let mut replies = spawns.take_replies();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0].0, CONN);
    replies.remove(0).1
}

fn ticket_of(effects: &[SpawnEffect]) -> SpawnTicket {
    match effects {
        [SpawnEffect::ListPanels { ticket, .. }
        | SpawnEffect::ReadPanels { ticket, .. }
        | SpawnEffect::Spawn { ticket, .. }] => *ticket,
        other => panic!("expected one effect: {other:?}"),
    }
}

fn spawn_effect(effects: Vec<SpawnEffect>) -> (SpawnTicket, SpawnSpec) {
    match effects.as_slice() {
        [SpawnEffect::Spawn { ticket, spec }] => (*ticket, spec.clone()),
        other => panic!("expected a spawn: {other:?}"),
    }
}

fn templates_of(message: DaemonMessage) -> combine_protocol::Templates {
    match message {
        DaemonMessage::Templates(t) => t,
        other => panic!("not templates: {other:?}"),
    }
}

#[test]
fn unknown_space_refused() {
    let (mut spawns, _) = setup();
    spawns.list_templates(CONN, req(1), space(), false);
    spawns.spawn_terminal(CONN, req(2), space(), SpawnSource::Shell, false);
    assert!(spawns.take_effects().is_empty());
    let replies = spawns.take_replies();
    assert_eq!(replies.len(), 2);
    for (reply, id) in replies.iter().zip([1, 2]) {
        let (code, message, request) = error_of(&reply.1);
        assert_eq!(code, ErrorCode::UnknownSpace);
        assert_eq!(message, "the host lists no such space");
        assert_eq!(request, Some(req(id)));
    }
}

#[test]
fn invalid_panel_id_refused() {
    let (mut spawns, _) = setup();
    for (n, id) in ["", "a/b", "..", ".x", "a\u{7}b"].into_iter().enumerate() {
        spawns.spawn_terminal(CONN, req(n as u64), space(), template(id), true);
        assert!(spawns.take_effects().is_empty(), "{id:?}");
        let (code, message, request) = error_of(&only_reply(&mut spawns));
        assert_eq!(code, ErrorCode::InvalidRequest, "{id:?}");
        assert!(
            message.starts_with("the panel ID isn't valid: "),
            "{message}"
        );
        assert_eq!(request, Some(req(n as u64)));
    }
}

#[test]
fn fifth_in_flight_refused() {
    let (mut spawns, _) = setup();
    for n in 0..MAX_SPAWN_IN_FLIGHT as u64 {
        spawns.list_templates(CONN, req(n), space(), true);
    }
    assert_eq!(spawns.take_effects().len(), MAX_SPAWN_IN_FLIGHT);
    spawns.list_templates(CONN, req(9), space(), true);
    spawns.spawn_terminal(CONN, req(10), space(), SpawnSource::Shell, true);
    assert!(spawns.take_effects().is_empty());
    let replies = spawns.take_replies();
    assert_eq!(replies.len(), 2);
    let (code, message, _) = error_of(&replies[0].1);
    assert_eq!(code, ErrorCode::InvalidRequest);
    assert_eq!(
        message,
        "too many template and spawn requests in flight; the limit is 4"
    );

    // Another connection has its own count; an answer frees a slot.
    spawns.list_templates(ConnectionId(2), req(11), space(), true);
    assert_eq!(spawns.take_effects().len(), 1);
    spawns.apply_listing(SpawnTicket(0), Ok(None));
    assert_eq!(spawns.take_replies().len(), 1);
    spawns.list_templates(CONN, req(12), space(), true);
    assert_eq!(spawns.take_effects().len(), 1);
}

#[test]
fn shell_with_missing_root_is_cwd_refused() {
    let (mut spawns, dirs) = setup();
    dirs.refuse("", DirRefusal::Missing);
    spawns.spawn_terminal(CONN, req(1), space(), SpawnSource::Shell, true);
    assert!(spawns.take_effects().is_empty());
    let (code, message, request) = error_of(&only_reply(&mut spawns));
    assert_eq!(code, ErrorCode::CwdRefused);
    assert_eq!(message, "the project folder /p doesn't exist");
    assert_eq!(request, Some(req(1)));
    // The slot is free again.
    for n in 0..MAX_SPAWN_IN_FLIGHT as u64 {
        spawns.list_templates(CONN, req(n), space(), true);
    }
    assert!(spawns.take_replies().is_empty());
}

#[test]
fn template_cwd_refused_by_dirs_emits_no_spawn() {
    for (refusal, why) in [
        (DirRefusal::Outside, "resolves outside the project"),
        (DirRefusal::InsideGit, "resolves inside .git/"),
        (DirRefusal::Missing, "doesn't exist"),
    ] {
        let (mut spawns, dirs) = setup();
        dirs.refuse("sub", refusal);
        let source = template_for("dev", &panel_json("sub"));
        spawns.spawn_terminal(CONN, req(1), space(), source, true);
        let ticket = ticket_of(&spawns.take_effects());
        spawns.apply_panels(ticket, vec![("dev".to_owned(), found(panel_json("sub")))]);
        assert!(spawns.take_effects().is_empty());
        let (code, message, _) = error_of(&only_reply(&mut spawns));
        assert_eq!(code, ErrorCode::CwdRefused);
        assert_eq!(message, format!("the template's cwd \"sub\" {why}"));
    }
    let (mut spawns, _) = setup();
    let source = template_for("dev", &panel_json("../x"));
    spawns.spawn_terminal(CONN, req(1), space(), source, true);
    let ticket = ticket_of(&spawns.take_effects());
    spawns.apply_panels(ticket, vec![("dev".to_owned(), found(panel_json("../x")))]);
    assert!(spawns.take_effects().is_empty());
    let (code, message, _) = error_of(&only_reply(&mut spawns));
    assert_eq!(code, ErrorCode::CwdRefused);
    assert_eq!(message, "the template's cwd \"../x\" has a .. segment");
}

/// A file that is gone or unreadable, or whose bytes (with the hash the client sent) aren't a
/// terminal template.
#[test]
fn template_file_now_invalid_is_unknown_template() {
    let reads = [
        PanelRead::Missing,
        found(b"{".to_vec()),
        found(
            json!({"version": 1, "content": {"type": "web"}})
                .to_string()
                .into_bytes(),
        ),
        PanelRead::TooLarge { size: 9_000 },
        PanelRead::Failed("denied".to_owned()),
    ];
    for read in reads {
        let (mut spawns, _) = setup();
        let source = match &read {
            PanelRead::Found { bytes, .. } => template_for("dev", bytes),
            _ => template("dev"),
        };
        spawns.spawn_terminal(CONN, req(1), space(), source, true);
        let ticket = ticket_of(&spawns.take_effects());
        spawns.apply_panels(ticket, vec![("dev".to_owned(), read.clone())]);
        assert!(spawns.take_effects().is_empty(), "{read:?}");
        let (code, message, _) = error_of(&only_reply(&mut spawns));
        assert_eq!(code, ErrorCode::UnknownTemplate, "{read:?}");
        assert!(
            message.starts_with("no usable terminal template in panels/dev: "),
            "{message}"
        );
    }
}

#[test]
fn spawned_and_spawn_failed_replies() {
    let (mut spawns, _) = setup();
    spawns.spawn_terminal(CONN, req(1), space(), SpawnSource::Shell, true);
    let (ticket, spec) = spawn_effect(spawns.take_effects());
    assert!(spec.argv.is_empty());
    assert_eq!(spec.cwd, "/p");
    spawns.apply_spawned(ticket, Ok(TermId("t12".to_owned())));
    match only_reply(&mut spawns) {
        DaemonMessage::Spawned(s) => {
            assert_eq!(
                (s.request, s.space.as_str(), s.terminal),
                (req(1), "/p", TermId("t12".into()))
            );
        }
        other => panic!("{other:?}"),
    }

    let source = template_for("dev", &panel_json("sub"));
    spawns.spawn_terminal(CONN, req(2), space(), source, true);
    let ticket = ticket_of(&spawns.take_effects());
    spawns.apply_panels(ticket, vec![("dev".to_owned(), found(panel_json("sub")))]);
    let (ticket, spec) = spawn_effect(spawns.take_effects());
    assert_eq!(spec.argv, ["claude"]);
    assert_eq!(spec.cwd, "/p/sub");
    spawns.apply_spawned(ticket, Err(BackendError::Failed("no pty".to_owned())));
    let (code, message, request) = error_of(&only_reply(&mut spawns));
    assert_eq!(code, ErrorCode::SpawnFailed);
    assert_eq!(
        message,
        "couldn't start the terminal: backend failure: no pty"
    );
    assert_eq!(request, Some(req(2)));
}

#[test]
fn disconnect_orphans_ticket_and_never_spawns() {
    let (mut spawns, _) = setup();
    spawns.spawn_terminal(CONN, req(1), space(), template("dev"), true);
    let read = ticket_of(&spawns.take_effects());
    spawns.spawn_terminal(CONN, req(2), space(), SpawnSource::Shell, true);
    let (running, _) = spawn_effect(spawns.take_effects());
    spawns.list_templates(CONN, req(3), space(), true);
    let listing = ticket_of(&spawns.take_effects());
    spawns.disconnect(CONN);

    spawns.apply_panels(read, vec![("dev".to_owned(), found(panel_json("")))]);
    spawns.apply_spawned(running, Ok(TermId("t1".to_owned())));
    spawns.apply_listing(
        listing,
        Ok(Some(DirListing {
            entries: vec![entry("a", EntryKind::Directory)],
            truncated: false,
        })),
    );
    assert!(spawns.take_effects().is_empty());
    assert!(spawns.take_replies().is_empty());

    // A queued effect of an orphaned ticket is dropped too.
    spawns.spawn_terminal(CONN, req(4), space(), SpawnSource::Shell, true);
    spawns.disconnect(CONN);
    assert!(spawns.take_effects().is_empty());

    // The connection's count is cleared.
    for n in 0..MAX_SPAWN_IN_FLIGHT as u64 {
        spawns.list_templates(CONN, req(n), space(), true);
    }
    assert!(spawns.take_replies().is_empty());
}

fn listing(entries: Vec<ListedEntry>, truncated: bool) -> Result<Option<DirListing>, StoreError> {
    Ok(Some(DirListing { entries, truncated }))
}

#[test]
fn symlinked_panel_folders_count_toward_one_hundred() {
    let (mut spawns, _) = setup();
    let mut entries: Vec<ListedEntry> = (0..60)
        .map(|n| entry(&format!("d{n:03}"), EntryKind::Directory))
        .collect();
    entries.extend((60..110).map(|n| entry(&format!("d{n:03}"), EntryKind::Symlink)));
    entries.push(entry(".hidden", EntryKind::Directory));
    entries.push(entry("file", EntryKind::File));
    entries.push(entry("odd", EntryKind::Other));
    spawns.list_templates(CONN, req(1), space(), true);
    let ticket = ticket_of(&spawns.take_effects());
    spawns.apply_listing(ticket, listing(entries, true));
    let panels = match spawns.take_effects().as_slice() {
        [SpawnEffect::ReadPanels { panels, .. }] => panels.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(panels.len(), 60);
    assert_eq!(panels[0], "d000");
    spawns.apply_panels(
        ticket,
        panels
            .iter()
            .map(|p| (p.clone(), found(panel_json(""))))
            .collect(),
    );
    let templates = templates_of(only_reply(&mut spawns));
    assert_eq!(templates.templates.len(), 60);
    // 100 folders considered: 60 directories and 40 symlinks.
    assert_eq!(templates.skipped.len(), 40);
    assert_eq!(
        templates.skipped[0],
        SkippedPanel {
            panel: "d060".to_owned(),
            reason: "the panel folder is a symlink; Combine doesn't follow it".to_owned()
        }
    );
    assert_eq!(templates.skipped[39].panel, "d099");
    assert_eq!(
        templates.error.as_deref(),
        Some(
            ".workspace/panels has more than 256 entries; only some were read; \
             .workspace/panels has more than 100 panel folders; only the first 100 by name were read"
        )
    );
}

#[test]
fn listing_never_spawns() {
    let (mut spawns, dirs) = setup();
    dirs.refuse("bad", DirRefusal::Outside);
    spawns.list_templates(CONN, req(1), space(), true);
    let ticket = ticket_of(&spawns.take_effects());
    spawns.apply_listing(
        ticket,
        listing(
            vec![
                entry("a", EntryKind::Directory),
                entry("b", EntryKind::Directory),
                entry("c", EntryKind::Directory),
                entry("d", EntryKind::Directory),
            ],
            false,
        ),
    );
    let effects = spawns.take_effects();
    assert!(matches!(
        effects.as_slice(),
        [SpawnEffect::ReadPanels { .. }]
    ));
    spawns.apply_panels(
        ticket,
        vec![
            ("d".to_owned(), found(panel_json("bad"))),
            ("c".to_owned(), PanelRead::Failed("denied".to_owned())),
            ("b".to_owned(), PanelRead::TooLarge { size: 10_000 }),
            ("a".to_owned(), found(panel_json("sub"))),
        ],
    );
    assert!(spawns.take_effects().is_empty());
    let templates = templates_of(only_reply(&mut spawns));
    assert_eq!(templates.error, None);
    assert_eq!(templates.templates.len(), 1);
    assert_eq!(templates.templates[0].panel, "a");
    assert_eq!(templates.templates[0].cwd, "sub");
    let skipped: Vec<(&str, &str)> = templates
        .skipped
        .iter()
        .map(|s| (s.panel.as_str(), s.reason.as_str()))
        .collect();
    assert_eq!(
        skipped,
        [
            ("b", "panel.json is 10000 bytes; the limit is 8192"),
            ("c", "couldn't read panel.json (denied)"),
            ("d", "cwd \"bad\" resolves outside the project"),
        ]
    );

    for (listed, text) in [
        (Ok(None), None),
        (
            Err(StoreError::Unavailable("gone".to_owned())),
            Some("Couldn't list .workspace/panels (gone)"),
        ),
    ] {
        spawns.list_templates(CONN, req(2), space(), true);
        let ticket = ticket_of(&spawns.take_effects());
        spawns.apply_listing(ticket, listed);
        assert!(spawns.take_effects().is_empty());
        let t = templates_of(only_reply(&mut spawns));
        assert!(t.templates.is_empty() && t.skipped.is_empty());
        assert_eq!(t.error.as_deref(), text);
    }
}

#[test]
fn only_spawn_requests_emit_spawn_effects() {
    let (mut spawns, _) = setup();
    spawns.list_templates(CONN, req(1), space(), true);
    spawns.list_templates(CONN, req(2), space(), false);
    spawns.spawn_terminal(CONN, req(3), space(), template(""), true);
    spawns.spawn_terminal(CONN, req(4), space(), template("dev"), false);
    let effects = spawns.take_effects();
    assert!(effects
        .iter()
        .all(|e| !matches!(e, SpawnEffect::Spawn { .. })));
    assert_eq!(effects.len(), 1);
    assert!(spawns.take_effects().is_empty());

    spawns.spawn_terminal(CONN, req(5), space(), SpawnSource::Shell, true);
    assert!(matches!(
        spawns.take_effects().as_slice(),
        [SpawnEffect::Spawn { .. }]
    ));
}

/// A listing offers each template with the hash of the bytes it was parsed from.
#[test]
fn listed_templates_carry_the_hash_of_their_bytes() {
    let (mut spawns, _) = setup();
    spawns.list_templates(CONN, req(1), space(), true);
    let ticket = ticket_of(&spawns.take_effects());
    spawns.apply_listing(
        ticket,
        listing(vec![entry("a", EntryKind::Directory)], false),
    );
    spawns.take_effects();
    spawns.apply_panels(ticket, vec![("a".to_owned(), found(panel_json("sub")))]);
    let templates = templates_of(only_reply(&mut spawns));
    let expected = hash_of(&panel_json("sub")).to_hex();
    assert_eq!(templates.templates[0].hash, expected);
    assert_eq!(expected.len(), 64);
    assert!(expected
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
}

/// The click approves the definition the menu showed: a file whose bytes no longer have that
/// hash is refused, even when it is another valid template, and nothing is spawned.
#[test]
fn changed_template_is_refused_and_never_spawned() {
    let shown = panel_json("");
    let now = json!({
        "version": 1,
        "content": {
            "type": "terminal",
            "slot": "main",
            "template": { "argv": ["sh", "-c", "echo unreviewed"] }
        }
    })
    .to_string()
    .into_bytes();
    let (mut spawns, _) = setup();
    spawns.spawn_terminal(CONN, req(1), space(), template_for("dev", &shown), true);
    let ticket = ticket_of(&spawns.take_effects());
    spawns.apply_panels(ticket, vec![("dev".to_owned(), found(now.clone()))]);
    assert!(spawns.take_effects().is_empty());
    let (code, message, request) = error_of(&only_reply(&mut spawns));
    assert_eq!(code, ErrorCode::TemplateChanged);
    assert_eq!(
        message,
        "the panel dev changed since it was listed; review it and choose again"
    );
    assert_eq!(request, Some(req(1)));

    // The store's hash is what counts: bytes that match the request under a different
    // reported hash are refused too.
    spawns.spawn_terminal(CONN, req(2), space(), template_for("dev", &shown), true);
    let ticket = ticket_of(&spawns.take_effects());
    let read = PanelRead::Found {
        bytes: shown.clone(),
        hash: hash_of(&now),
    };
    spawns.apply_panels(ticket, vec![("dev".to_owned(), read)]);
    assert!(spawns.take_effects().is_empty());
    assert_eq!(
        error_of(&only_reply(&mut spawns)).0,
        ErrorCode::TemplateChanged
    );

    // Choosing again with the new listing's hash runs the new definition.
    spawns.spawn_terminal(CONN, req(3), space(), template_for("dev", &now), true);
    let ticket = ticket_of(&spawns.take_effects());
    spawns.apply_panels(ticket, vec![("dev".to_owned(), found(now))]);
    let (_, spec) = spawn_effect(spawns.take_effects());
    assert_eq!(spec.argv, ["sh", "-c", "echo unreviewed"]);
}
