//! The panel store's Workspace (Persistent) paths. Machine tests: an explicit clock, an explicit
//! ID counter, and no sleeping.

use combine_protocol::{PanelState, Renderer};
use combined::core::PanelRead;
use combined::core::{
    encode_scratch_meta, panel_reply_len, Core, CoreConfig, FileWrite, IdSource, PanelApiError,
    PanelCreate, PanelEffect, PanelEvent, PanelFolder, PanelStore, PanelTiming, PanelUpdate,
    PanelView, SaveEnd, SaveReport, SaveTicket, SCRATCH_ID_OPEN, SCRATCH_TTL_MS,
};
use combined::model::{ContentHash, Lifetime, Millis, SpaceId};
use combined::ports::{
    Capabilities, DiagramCheckError, DiagramFailure, DiagramVerdict, DirListing, EntryKind,
    FailureCode, FileMark, ListedEntry, ScratchDir, ScratchListing, StoreError,
};
use sha2::{Digest, Sha256};

struct Counter(u64);

impl IdSource for Counter {
    fn take_id(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }
}

/// A store that has asked for its scratch listing and been told it is empty, as the server
/// starts it. No space is added.
fn started_store(timing: PanelTiming) -> PanelStore {
    let mut store = PanelStore::new(timing);
    store.start(Millis(0));
    store.apply_scratch_listing(
        Ok(ScratchListing {
            dirs: Vec::new(),
            truncated: false,
        }),
        Millis(0),
    );
    store.take_effects();
    store
}

fn space() -> SpaceId {
    SpaceId("/work/app".to_owned())
}

fn hash_of(bytes: &[u8]) -> ContentHash {
    ContentHash(Sha256::digest(bytes).into())
}

fn found(bytes: &[u8]) -> PanelRead {
    PanelRead::Found {
        bytes: bytes.to_vec(),
        hash: hash_of(bytes),
    }
}

fn fallback(bytes: &[u8]) -> PanelRead {
    PanelRead::FallbackFound {
        bytes: bytes.to_vec(),
        hash: hash_of(bytes),
    }
}

fn md_json() -> &'static [u8] {
    br#"{"version":1,"renderer":"markdown"}"#
}

fn mm_json() -> &'static [u8] {
    br#"{"version":1,"renderer":"mermaid"}"#
}

fn entry(name: &str, kind: EntryKind) -> ListedEntry {
    ListedEntry {
        name: name.to_owned(),
        kind,
    }
}

fn listing(names: &[&str]) -> Result<Option<DirListing>, StoreError> {
    Ok(Some(DirListing {
        entries: names
            .iter()
            .map(|name| entry(name, EntryKind::Directory))
            .collect(),
        truncated: false,
    }))
}

fn at(id: &str) -> PanelFolder {
    PanelFolder::Workspace {
        space: space(),
        panel: id.to_owned(),
    }
}

fn saves(effects: &[PanelEffect]) -> Vec<(SaveTicket, Vec<FileWrite>)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            PanelEffect::Save { ticket, files, .. } => Some((*ticket, files.clone())),
            _ => None,
        })
        .collect()
}

fn checks(effects: &[PanelEffect]) -> Vec<(String, u64, String)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            PanelEffect::Check {
                panel,
                revision,
                source,
                ..
            } => Some((panel.clone(), *revision, source.clone())),
            _ => None,
        })
        .collect()
}

fn reads(effects: &[PanelEffect]) -> Vec<(String, &'static str)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            PanelEffect::Read {
                at: PanelFolder::Workspace { panel, .. },
                file,
            } => Some((panel.clone(), *file)),
            _ => None,
        })
        .collect()
}

fn deletes(effects: &[PanelEffect]) -> Vec<(SaveTicket, Vec<(&'static str, FileMark)>)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            PanelEffect::Delete { ticket, files, .. } => Some((*ticket, files.clone())),
            _ => None,
        })
        .collect()
}

fn lists(effects: &[PanelEffect]) -> usize {
    effects
        .iter()
        .filter(|effect| matches!(effect, PanelEffect::ListWorkspace { .. }))
        .count()
}

fn logs(effects: &[PanelEffect]) -> Vec<String> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            PanelEffect::Log { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn changed(events: &[PanelEvent]) -> Vec<(String, u64, PanelState)> {
    events
        .iter()
        .filter_map(|event| match event {
            PanelEvent::Changed {
                panel,
                revision,
                state,
                ..
            } => Some((panel.clone(), *revision, *state)),
            PanelEvent::Listing { .. } => None,
        })
        .collect()
}

fn done(files: &[FileWrite]) -> SaveReport {
    SaveReport {
        written: files
            .iter()
            .map(|file| (file.file, hash_of(&file.bytes)))
            .collect(),
        end: SaveEnd::Done,
    }
}

fn create_of(id: &str, renderer: Renderer, content: &str) -> PanelCreate {
    PanelCreate {
        id: Some(id.to_owned()),
        lifetime: Lifetime::Persistent,
        renderer,
        title: None,
        idempotency_key: None,
        created_by: None,
        placement: None,
        content: content.to_owned(),
    }
}

fn content_update(content: &str) -> PanelUpdate {
    PanelUpdate {
        content: Some(content.to_owned()),
        title: None,
    }
}

fn title_update(title: Option<&str>) -> PanelUpdate {
    PanelUpdate {
        content: None,
        title: Some(title.map(str::to_owned)),
    }
}

fn failure(code: FailureCode) -> DiagramFailure {
    DiagramFailure {
        code,
        message: "bad".to_owned(),
        line: Some(3),
        column: Some(4),
        snippet: Some("  a -> b".to_owned()),
    }
}

struct Rig {
    store: PanelStore,
    ids: Counter,
    now: u64,
    timing: PanelTiming,
}

impl Rig {
    /// A loaded space with no panel folders.
    fn new() -> Rig {
        Rig::with_folders(&[])
    }

    /// A loaded space holding Markdown panels `(name, content)` on disk.
    fn with_folders(folders: &[(&str, &str)]) -> Rig {
        let timing = PanelTiming::default();
        let mut rig = Rig {
            store: PanelStore::new(timing),
            ids: Counter(1_000),
            now: 10_000,
            timing,
        };
        rig.store.start(rig.t());
        rig.store.add_space(&space(), rig.t());
        rig.store.apply_scratch_listing(
            Ok(ScratchListing {
                dirs: Vec::new(),
                truncated: false,
            }),
            rig.t(),
        );
        if folders.is_empty() {
            rig.store.apply_listing(&space(), Ok(None), rig.t());
        } else {
            let names: Vec<&str> = folders.iter().map(|(name, _)| *name).collect();
            rig.store.apply_listing(&space(), listing(&names), rig.t());
            for (name, content) in folders {
                rig.pair(name, found(md_json()), found(content.as_bytes()));
            }
        }
        rig.store.take_effects();
        rig.store.take_events();
        rig
    }

    fn t(&self) -> Millis {
        Millis(self.now)
    }

    fn set(&mut self, now: u64) {
        self.now = now;
    }

    fn tick(&mut self, after_ms: u64) {
        self.now += after_ms;
        self.store.tick(self.t(), 0);
    }

    fn fx(&mut self) -> Vec<PanelEffect> {
        self.store.take_effects()
    }

    fn ev(&mut self) -> Vec<PanelEvent> {
        self.store.take_events()
    }

    fn create(
        &mut self,
        id: &str,
        renderer: Renderer,
        content: &str,
    ) -> Result<PanelView, PanelApiError> {
        self.store.create(
            &space(),
            create_of(id, renderer, content),
            self.t(),
            0,
            &mut self.ids,
        )
    }

    fn md(&mut self, id: &str, content: &str) -> PanelView {
        self.create(id, Renderer::Markdown, content).unwrap()
    }

    fn mm(&mut self, id: &str, content: &str) -> PanelView {
        self.create(id, Renderer::Mermaid, content).unwrap()
    }

    fn update(&mut self, id: &str, update: PanelUpdate) -> Result<PanelView, PanelApiError> {
        self.store
            .update(&space(), id, update, self.t(), 0, &mut self.ids)
    }

    fn get(&self, id: &str) -> (PanelView, String) {
        let (view, source) = self.store.get(&space(), id).unwrap();
        (view, source.to_owned())
    }

    /// Applies a read of `panel.json` and then of the content file.
    fn pair(&mut self, id: &str, json: PanelRead, content: PanelRead) {
        self.store
            .apply_read(&at(id), "panel.json", json, self.t(), &mut self.ids);
        let file = if reads(&self.store.take_effects())
            .iter()
            .any(|(_, file)| *file == "diagram.mmd")
        {
            "diagram.mmd"
        } else {
            "content.md"
        };
        self.store
            .apply_read(&at(id), file, content, self.t(), &mut self.ids);
    }

    /// Takes the effects, applies the one Save as done, and returns the other effects.
    fn finish_save(&mut self) -> Vec<FileWrite> {
        let fx = self.fx();
        let mut all = saves(&fx);
        assert_eq!(all.len(), 1, "{fx:?}");
        let (ticket, files) = all.remove(0);
        self.store.apply_saved(ticket, done(&files), self.t());
        files
    }

    /// Creates a Markdown panel and applies its Save.
    fn saved(&mut self, id: &str, content: &str) -> PanelView {
        let view = self.md(id, content);
        self.finish_save();
        self.fx();
        self.ev();
        view
    }

    /// A change to `path`, then a tick past the debounce.
    fn outside(&mut self, path: &str) -> Vec<PanelEffect> {
        self.store.apply_change(&space(), path, self.t());
        self.tick(self.timing.debounce_ms);
        self.fx()
    }

    fn verdict(
        &mut self,
        id: &str,
        revision: u64,
        result: Result<DiagramVerdict, DiagramCheckError>,
    ) {
        self.store
            .apply_check(&space(), id, revision, result, self.t());
    }
}

// ---- AC1

#[test]
fn create_markdown_goes_validating_then_validated() {
    let mut rig = Rig::new();
    let view = rig.md("p", "# hi");
    assert_eq!(view.state, PanelState::Validated);
    assert_eq!(view.lifetime, Lifetime::Persistent);
    let events = changed(&rig.ev());
    assert_eq!(
        events,
        [
            ("p".to_owned(), view.revision, PanelState::Validating),
            ("p".to_owned(), view.revision, PanelState::Validated),
        ]
    );
    assert!(checks(&rig.fx()).is_empty());
    assert_eq!(rig.get("p").1, "# hi");
}

#[test]
fn markdown_over_204800_bytes_fails_too_large() {
    let mut rig = Rig::new();
    let view = rig.md("p", &"a".repeat(204_801));
    assert_eq!(view.state, PanelState::Failed);
    let error = view.error.unwrap();
    assert_eq!(error.code, "too_large");
    assert_eq!(
        error.message,
        "the source is 204801 bytes; the limit is 204800"
    );
    assert_eq!(rig.get("p").1, "");
    let files = saves(&rig.fx());
    assert_eq!(files[0].1[0].bytes.len(), 204_801);
}

#[test]
fn markdown_with_invalid_utf8_on_disk_fails_invalid_utf8() {
    let mut rig = Rig::new();
    rig.store.apply_listing(&space(), listing(&["p"]), rig.t());
    rig.fx();
    rig.pair("p", found(md_json()), found(&[0x66, 0xff, 0xfe]));
    let (view, source) = rig.get("p");
    assert_eq!(view.state, PanelState::Failed);
    let error = view.error.unwrap();
    assert_eq!(error.code, "invalid_utf8");
    assert_eq!(error.message, "the content is not valid UTF-8");
    assert_eq!(source, "");
}

#[test]
fn create_mermaid_emits_one_check() {
    let mut rig = Rig::new();
    let view = rig.mm("d", "graph TD\n a-->b");
    assert_eq!(view.state, PanelState::Validating);
    let fx = rig.fx();
    assert_eq!(
        checks(&fx),
        [("d".to_owned(), view.revision, "graph TD\n a-->b".to_owned())]
    );
    assert_eq!(rig.store.in_flight(), 1);
}

#[test]
fn create_writes_content_then_panel_json() {
    let mut rig = Rig::new();
    let mut create = create_of("p", Renderer::Markdown, "body");
    create.title = Some("Title".to_owned());
    rig.store
        .create(&space(), create, rig.t(), 0, &mut rig.ids)
        .unwrap();
    let fx = rig.fx();
    let (_, files) = saves(&fx).remove(0);
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].file, "content.md");
    assert_eq!(files[0].bytes, b"body");
    assert_eq!(files[0].expected, None);
    assert_eq!(files[1].file, "panel.json");
    assert_eq!(files[1].expected, Some(FileMark::Missing));
    let text = String::from_utf8(files[1].bytes.clone()).unwrap();
    assert!(text.contains("\"title\": \"Title\""));
    assert!(matches!(
        &fx[0],
        PanelEffect::Save { at: PanelFolder::Workspace { panel, .. }, .. } if panel == "p"
    ));
}

// ---- AC2

#[test]
fn failed_verdict_maps_each_code_with_position() {
    let mut rig = Rig::new();
    for (index, code) in FailureCode::ALL.into_iter().enumerate() {
        let id = format!("d{index}");
        let view = rig.mm(&id, "x");
        rig.fx();
        let mut failed = failure(code);
        failed.message = "m".repeat(2_000);
        rig.verdict(&id, view.revision, Ok(DiagramVerdict::Failed(failed)));
        let (view, _) = rig.get(&id);
        assert_eq!(view.state, PanelState::Failed);
        let error = view.error.unwrap();
        assert_eq!(error.code, code.as_str());
        assert_eq!(error.message.len(), 1_024);
        assert_eq!((error.line, error.column), (Some(3), Some(4)));
        assert_eq!(error.snippet.as_deref(), Some("  a -> b"));
    }
}

#[test]
fn parser_only_maps_to_validated_parser_only() {
    let mut rig = Rig::new();
    let view = rig.mm("d", "x");
    rig.verdict("d", view.revision, Ok(DiagramVerdict::ValidatedParserOnly));
    let (view, _) = rig.get("d");
    assert_eq!(view.state, PanelState::ValidatedParserOnly);
    assert_eq!(view.error, None);
}

#[test]
fn no_verdict_fails_with_its_text_and_no_position() {
    let mut rig = Rig::new();
    let view = rig.mm("d", "x");
    rig.verdict(
        "d",
        view.revision,
        Err(DiagramCheckError::NoVerdict("TypeError: boom".to_owned())),
    );
    let error = rig.get("d").0.error.unwrap();
    assert_eq!(error.code, "no_verdict");
    assert_eq!(error.message, "TypeError: boom");
    assert_eq!(
        (error.line, error.column, error.snippet),
        (None, None, None)
    );

    let view = rig.mm("e", "x");
    rig.verdict(
        "e",
        view.revision,
        Err(DiagramCheckError::NoVerdict(String::new())),
    );
    assert_eq!(
        rig.get("e").0.error.unwrap().message,
        "the diagram validator gave no verdict"
    );
}

fn unavailable() -> Result<DiagramVerdict, DiagramCheckError> {
    Err(DiagramCheckError::Unavailable("engine down".to_owned()))
}

/// Applies `Unavailable`, then checks the retried `Check` waits for exactly `delay_ms`: none just
/// before it, one at it. The store's `next_deadline` is not read.
fn retry_after(rig: &mut Rig, id: &str, revision: u64, delay_ms: u64) {
    rig.verdict(id, revision, unavailable());
    assert_eq!(rig.get(id).0.state, PanelState::Validating);
    assert_eq!(rig.get(id).0.error, None);
    rig.tick(delay_ms - 1);
    assert!(checks(&rig.fx()).is_empty());
    rig.tick(1);
    assert_eq!(checks(&rig.fx()).len(), 1);
}

#[test]
fn unavailable_retries_at_1_5_30_seconds_then_fails() {
    let mut rig = Rig::new();
    let view = rig.mm("d", "x");
    rig.fx();
    retry_after(&mut rig, "d", view.revision, 1_000);
    retry_after(&mut rig, "d", view.revision, 5_000);
    retry_after(&mut rig, "d", view.revision, 30_000);
    rig.verdict("d", view.revision, unavailable());
    let (view, _) = rig.get("d");
    assert_eq!(view.state, PanelState::Failed);
    let error = view.error.unwrap();
    assert_eq!(error.code, "unavailable");
    assert_eq!(error.message, "validation is unavailable: engine down");
    assert_eq!(error.line, None);
}

#[test]
fn an_update_restarts_the_retry_cycle() {
    let mut rig = Rig::new();
    let view = rig.mm("d", "x");
    rig.fx();
    retry_after(&mut rig, "d", view.revision, 1_000);
    retry_after(&mut rig, "d", view.revision, 5_000);
    let updated = rig.update("d", content_update("y")).unwrap();
    rig.fx();
    rig.verdict("d", view.revision, unavailable());
    assert_eq!(checks(&rig.fx()).len(), 1);
    assert_eq!(rig.store.in_flight(), 1);
    retry_after(&mut rig, "d", updated.revision, 1_000);
}

#[test]
fn an_outside_edit_restarts_the_retry_cycle() {
    let mut rig = Rig::new();
    let view = rig.mm("d", "x");
    rig.finish_save();
    rig.fx();
    retry_after(&mut rig, "d", view.revision, 1_000);
    retry_after(&mut rig, "d", view.revision, 5_000);
    rig.verdict("d", view.revision, unavailable());
    rig.store
        .apply_change(&space(), "panels/d/diagram.mmd", rig.t());
    rig.tick(500);
    rig.fx();
    rig.pair("d", found(mm_json()), found(b"outside"));
    let adopted = rig.get("d").0;
    assert!(adopted.revision > view.revision);
    let running = checks(&rig.fx());
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].1, adopted.revision);
    retry_after(&mut rig, "d", adopted.revision, 1_000);
    retry_after(&mut rig, "d", adopted.revision, 5_000);
    retry_after(&mut rig, "d", adopted.revision, 30_000);
    rig.verdict("d", adopted.revision, unavailable());
    assert_eq!(rig.get("d").0.state, PanelState::Failed);
}

// ---- AC3

#[test]
fn update_bumps_the_revision_from_the_counter() {
    let mut rig = Rig::new();
    let created = rig.saved("p", "one");
    rig.ids.0 = 5_000;
    let updated = rig.update("p", content_update("one")).unwrap();
    assert_eq!(updated.revision, 5_001);
    assert!(updated.revision > created.revision);
    assert_eq!(
        rig.update("p", content_update("one")).unwrap().revision,
        5_002
    );
}

#[test]
fn core_take_id_advances_high_water() {
    let mut core = Core::new(
        CoreConfig::default(),
        Capabilities {
            ordered_attach: true,
            byte_credit: false,
            initial_credit: 0,
            display_queries: true,
            automatic_replies: true,
            resync_signal: true,
            agent_state: false,
        },
    )
    .unwrap();
    let mut store = started_store(PanelTiming::default());
    store.add_space(&space(), Millis(0));
    store.apply_listing(&space(), Ok(None), Millis(0));
    let before = core.high_water();
    let created = store
        .create(
            &space(),
            create_of("p", Renderer::Markdown, "a"),
            Millis(0),
            0,
            &mut core,
        )
        .unwrap();
    assert!(created.revision > before);
    assert_eq!(core.high_water(), created.revision);
    let updated = store
        .update(&space(), "p", content_update("b"), Millis(0), 0, &mut core)
        .unwrap();
    assert_eq!(updated.revision, created.revision + 1);
    assert_eq!(core.high_water(), updated.revision);
}

// ---- AC4

#[test]
fn a_stale_result_never_overwrites_a_newer_revision() {
    let mut rig = Rig::new();
    let r1 = rig.mm("d", "s1").revision;
    assert_eq!(checks(&rig.fx()).len(), 1);
    let r2 = rig.update("d", content_update("s2")).unwrap().revision;
    let r3 = rig.update("d", content_update("s3")).unwrap().revision;
    assert!(checks(&rig.fx()).is_empty());

    rig.verdict("d", r1, Ok(DiagramVerdict::Validated));
    let (view, source) = rig.get("d");
    assert_eq!(
        (view.state, view.error, source.as_str()),
        (PanelState::Validating, None, "s3")
    );
    let next = checks(&rig.fx());
    assert_eq!(next, [("d".to_owned(), r3, "s3".to_owned())]);

    rig.verdict("d", r3, Ok(DiagramVerdict::ValidatedParserOnly));
    assert_eq!(rig.get("d").0.state, PanelState::ValidatedParserOnly);
    assert_eq!(rig.store.in_flight(), 0);

    let again = rig.update("d", content_update("s4")).unwrap().revision;
    assert_eq!(checks(&rig.fx()).len(), 1);
    assert_eq!(rig.store.in_flight(), 1);
    rig.verdict(
        "d",
        r2,
        Ok(DiagramVerdict::Failed(failure(FailureCode::Syntax))),
    );
    assert_eq!(rig.store.in_flight(), 1);
    let (view, source) = rig.get("d");
    assert_eq!(
        (view.state, view.revision, source.as_str()),
        (PanelState::Validating, again, "s4")
    );
}

#[test]
fn a_revived_panel_waits_for_the_running_check() {
    let mut rig = Rig::new();
    let r1 = rig.mm("d", "s1").revision;
    rig.finish_save();
    rig.fx();
    rig.store.close(&space(), "d", rig.t()).unwrap();
    rig.fx();
    rig.store
        .apply_change(&space(), "panels/d/diagram.mmd", rig.t());
    rig.tick(500);
    rig.fx();
    rig.pair("d", found(mm_json()), found(b"revived"));
    let (view, _) = rig.get("d");
    assert_eq!(view.state, PanelState::Validating);
    assert!(view.revision > r1);
    assert!(checks(&rig.fx()).is_empty());
    assert_eq!(rig.store.in_flight(), 1);

    rig.verdict("d", r1, Ok(DiagramVerdict::Validated));
    assert_eq!(rig.get("d").0.state, PanelState::Validating);
    let next = checks(&rig.fx());
    assert_eq!(
        next,
        [("d".to_owned(), view.revision, "revived".to_owned())]
    );
}

#[test]
fn a_result_for_another_revision_frees_no_slot() {
    let mut rig = Rig::new();
    let revision = rig.mm("d", "x").revision;
    rig.fx();
    rig.verdict("d", revision + 99, Ok(DiagramVerdict::Validated));
    assert_eq!(rig.store.in_flight(), 1);
    assert_eq!(rig.get("d").0.state, PanelState::Validating);
    rig.verdict("d", revision, Ok(DiagramVerdict::Validated));
    assert_eq!(rig.store.in_flight(), 0);
}

#[test]
fn at_most_two_in_flight_and_one_per_panel() {
    let mut rig = Rig::new();
    let mut revisions = Vec::new();
    for id in ["a", "b", "c", "d"] {
        revisions.push(rig.mm(id, id).revision);
    }
    let first = checks(&rig.fx());
    assert_eq!(
        first.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(rig.store.in_flight(), 2);

    let newer = rig.update("a", content_update("a2")).unwrap().revision;
    assert!(checks(&rig.fx()).is_empty());
    assert_eq!(rig.store.in_flight(), 2);

    rig.verdict("a", revisions[0], Ok(DiagramVerdict::Validated));
    let second = checks(&rig.fx());
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].0, "c");
    assert_eq!(rig.store.in_flight(), 2);
    rig.verdict("b", revisions[1], Ok(DiagramVerdict::Validated));
    rig.verdict("c", revisions[2], Ok(DiagramVerdict::Validated));
    let third: Vec<String> = checks(&rig.fx()).into_iter().map(|c| c.0).collect();
    assert_eq!(third, ["d", "a"]);
    assert!(rig.store.in_flight() <= 2);
    rig.verdict("a", newer, Ok(DiagramVerdict::Validated));
    assert_eq!(rig.get("a").0.state, PanelState::Validated);
}

// ---- AC5

#[test]
fn outside_edit_with_a_new_hash_pair_bumps_and_revalidates() {
    let mut rig = Rig::with_folders(&[("p", "one")]);
    let before = rig.get("p").0;
    rig.outside("panels/p/content.md");
    rig.pair("p", found(md_json()), found(b"two"));
    let (view, source) = rig.get("p");
    assert!(view.revision > before.revision);
    assert_eq!(
        (view.state, source.as_str()),
        (PanelState::Validated, "two")
    );
    let events = changed(&rig.ev());
    assert_eq!(events.last().unwrap().2, PanelState::Validated);
    assert!(rig
        .fx()
        .iter()
        .all(|e| !matches!(e, PanelEffect::Save { .. })));
}

#[test]
fn own_write_echo_is_ignored() {
    let mut rig = Rig::new();
    let revision = rig.md("p", "one").revision;
    let files = rig.finish_save();
    rig.fx();
    rig.ev();
    let fx = rig.outside("panels/p/panel.json");
    assert_eq!(reads(&fx), [("p".to_owned(), "panel.json")]);
    rig.pair("p", found(&files[1].bytes), found(&files[0].bytes));
    assert_eq!(rig.get("p").0.revision, revision);
    assert!(changed(&rig.ev()).is_empty());
    assert!(rig.fx().is_empty());
}

#[test]
fn identical_bytes_rewritten_outside_are_ignored() {
    let mut rig = Rig::with_folders(&[("p", "same")]);
    let revision = rig.get("p").0.revision;
    rig.outside("panels/p/content.md");
    rig.pair("p", found(md_json()), found(b"same"));
    assert_eq!(rig.get("p").0.revision, revision);
    assert!(changed(&rig.ev()).is_empty());
}

#[test]
fn changes_are_debounced_per_panel() {
    let mut rig = Rig::with_folders(&[("p", "1"), ("q", "1")]);
    let t0 = rig.now;
    rig.store
        .apply_change(&space(), "panels/p/content.md", Millis(t0));
    rig.store
        .apply_change(&space(), "panels/q/content.md", Millis(t0));
    rig.store
        .apply_change(&space(), "panels/p/content.md", Millis(t0 + 300));
    rig.set(t0 + 500);
    rig.store.tick(rig.t(), 0);
    assert_eq!(reads(&rig.fx()), [("q".to_owned(), "panel.json")]);
    rig.set(t0 + 799);
    rig.store.tick(rig.t(), 0);
    assert!(rig.fx().is_empty());
    rig.set(t0 + 800);
    rig.store.tick(rig.t(), 0);
    assert_eq!(reads(&rig.fx()), [("p".to_owned(), "panel.json")]);

    for step in 0..6 {
        rig.store.apply_change(
            &space(),
            "panels/p/content.md",
            Millis(t0 + 1_000 + step * 400),
        );
    }
    rig.set(t0 + 2_999);
    rig.store.tick(rig.t(), 0);
    assert!(rig.fx().is_empty());
    rig.set(t0 + 3_000);
    rig.store.tick(rig.t(), 0);
    assert_eq!(reads(&rig.fx()), [("p".to_owned(), "panel.json")]);
}

#[test]
fn save_conflict_reads_the_outside_edit_and_marks_dropped() {
    let mut rig = Rig::new();
    rig.saved("p", "one");
    let r2 = rig.update("p", content_update("two")).unwrap().revision;
    let (ticket, files) = saves(&rig.fx()).remove(0);
    assert_eq!(files.len(), 1);
    assert!(matches!(files[0].expected, Some(FileMark::Hash(_))));
    rig.store.apply_saved(
        ticket,
        SaveReport {
            written: Vec::new(),
            end: SaveEnd::Conflict { file: "content.md" },
        },
        rig.t(),
    );
    assert_eq!(reads(&rig.fx()), [("p".to_owned(), "panel.json")]);
    rig.pair("p", found(md_json()), found(b"theirs"));
    let (view, source) = rig.get("p");
    assert_eq!(source, "theirs");
    assert!(view.revision > r2);
    assert_eq!(view.dropped, Some(r2));
    rig.tick(10_000);
    assert!(saves(&rig.fx()).is_empty());
}

#[test]
fn a_conflict_whose_reread_is_ignored_retries_after_retry_ms() {
    let mut rig = Rig::new();
    let first = rig.md("p", "one");
    let written = rig.finish_save();
    rig.fx();
    rig.update("p", content_update("two")).unwrap();
    let (ticket, _) = saves(&rig.fx()).remove(0);
    rig.store.apply_saved(
        ticket,
        SaveReport {
            written: Vec::new(),
            end: SaveEnd::Conflict { file: "content.md" },
        },
        rig.t(),
    );
    rig.fx();
    let revision = rig.get("p").0.revision;
    rig.pair("p", found(&written[1].bytes), found(&written[0].bytes));
    assert_eq!(rig.get("p").0.revision, revision);
    assert!(revision > first.revision);
    rig.tick(rig.timing.retry_ms - 1);
    assert!(saves(&rig.fx()).is_empty());
    rig.tick(1);
    let (_, files) = saves(&rig.fx()).remove(0);
    assert_eq!(files[0].bytes, b"two");
}

#[test]
fn a_panels_change_rereads_every_known_folder_and_relists() {
    let mut rig = Rig::with_folders(&[("a", "1"), ("b", "2"), ("c", "3")]);
    let revisions: Vec<u64> = ["a", "b", "c"]
        .iter()
        .map(|id| rig.get(id).0.revision)
        .collect();
    let fx = rig.outside("panels");
    assert_eq!(lists(&fx), 1);
    assert!(matches!(fx[0], PanelEffect::ListWorkspace { .. }));
    let mut read: Vec<String> = reads(&fx)
        .into_iter()
        .map(|(id, file)| {
            assert_eq!(file, "panel.json");
            id
        })
        .collect();
    read.sort();
    assert_eq!(read, ["a", "b", "c"]);
    for (id, content) in [("a", "1"), ("b", "2"), ("c", "3")] {
        rig.pair(id, found(md_json()), found(content.as_bytes()));
    }
    let after: Vec<u64> = ["a", "b", "c"]
        .iter()
        .map(|id| rig.get(id).0.revision)
        .collect();
    assert_eq!(after, revisions);
}

#[test]
fn a_rescan_finds_an_edit_inside_an_existing_folder() {
    let mut rig = Rig::with_folders(&[("a", "1"), ("b", "2")]);
    let before = rig.get("b").0.revision;
    rig.outside("panels");
    rig.pair("a", found(md_json()), found(b"1"));
    rig.pair("b", found(md_json()), found(b"changed"));
    assert_eq!(rig.get("b").1, "changed");
    assert!(rig.get("b").0.revision > before);
}

/// A saved panel `p` with "one", and the files its create wrote.
fn saved_with_files(rig: &mut Rig) -> Vec<FileWrite> {
    rig.md("p", "one");
    let files = rig.finish_save();
    rig.fx();
    rig.ev();
    files
}

#[test]
fn a_read_asked_before_a_delete_is_not_adopted_while_it_runs() {
    let mut rig = Rig::with_folders(&[("p", "one")]);
    let fx = rig.outside("panels/p/content.md");
    assert_eq!(reads(&fx), [("p".to_owned(), "panel.json")]);
    rig.store.remove(&space(), "p", rig.t()).unwrap();
    let (ticket, _) = deletes(&rig.fx()).remove(0);
    let removed_at = rig.get("p").0.revision;
    rig.ev();
    rig.store.apply_read(
        &at("p"),
        "panel.json",
        found(md_json()),
        rig.t(),
        &mut rig.ids,
    );
    assert!(reads(&rig.fx()).is_empty());
    rig.store.apply_read(
        &at("p"),
        "content.md",
        found(&[0x66, 0xff]),
        rig.t(),
        &mut rig.ids,
    );
    assert!(changed(&rig.ev()).is_empty());
    let (view, _) = rig.get("p");
    assert_eq!(view.state, PanelState::Removed);
    assert_eq!(view.revision, removed_at);
    rig.store.apply_deleted(
        ticket,
        SaveReport {
            written: Vec::new(),
            end: SaveEnd::Conflict { file: "content.md" },
        },
        rig.t(),
    );
    assert_eq!(reads(&rig.fx()), [("p".to_owned(), "panel.json")]);
    rig.pair("p", found(md_json()), found(&[0x66, 0xff]));
    let (view, source) = rig.get("p");
    assert_eq!(view.state, PanelState::Failed);
    assert_eq!(view.error.unwrap().code, "invalid_utf8");
    assert_eq!(source, "");
    assert!(view.revision > removed_at);
}

#[test]
fn a_content_read_taken_before_a_save_is_not_applied_after_it() {
    let mut rig = Rig::new();
    let files = saved_with_files(&mut rig);
    rig.outside("panels/p/content.md");
    rig.store.apply_read(
        &at("p"),
        "panel.json",
        found(&files[1].bytes),
        rig.t(),
        &mut rig.ids,
    );
    assert_eq!(reads(&rig.fx()), [("p".to_owned(), "content.md")]);
    let updated = rig.update("p", content_update("two")).unwrap().revision;
    let (ticket, written) = saves(&rig.fx()).remove(0);
    rig.store.apply_saved(ticket, done(&written), rig.t());
    assert_eq!(reads(&rig.fx()), [("p".to_owned(), "panel.json")]);
    rig.store
        .apply_read(&at("p"), "content.md", found(b"one"), rig.t(), &mut rig.ids);
    let (view, source) = rig.get("p");
    assert_eq!(source, "two");
    assert_eq!(view.revision, updated);
    assert_eq!(view.dropped, None);
    assert!(rig.fx().is_empty());
    rig.pair("p", found(&files[1].bytes), found(b"two"));
    let (view, source) = rig.get("p");
    assert_eq!((source.as_str(), view.revision), ("two", updated));
}

#[test]
fn no_content_read_is_asked_while_a_save_runs() {
    let mut rig = Rig::new();
    let files = saved_with_files(&mut rig);
    rig.outside("panels/p/content.md");
    rig.update("p", content_update("two")).unwrap();
    let (ticket, written) = saves(&rig.fx()).remove(0);
    rig.store.apply_read(
        &at("p"),
        "panel.json",
        found(&files[1].bytes),
        rig.t(),
        &mut rig.ids,
    );
    assert!(reads(&rig.fx()).is_empty());
    rig.tick(rig.timing.debounce_ms);
    assert!(reads(&rig.fx()).is_empty());
    rig.store.apply_saved(ticket, done(&written), rig.t());
    assert_eq!(reads(&rig.fx()), [("p".to_owned(), "panel.json")]);
    assert_eq!(rig.get("p").1, "two");
}

#[test]
fn a_remove_during_a_save_that_conflicts_stays_removed_and_deletes_with_its_own_marks() {
    let mut rig = Rig::new();
    let files = saved_with_files(&mut rig);
    rig.update("p", content_update("two")).unwrap();
    let (ticket, _) = saves(&rig.fx()).remove(0);
    rig.store.remove(&space(), "p", rig.t()).unwrap();
    assert!(deletes(&rig.fx()).is_empty());
    let last = changed(&rig.ev()).pop().unwrap();
    assert_eq!(last.2, PanelState::Removed);
    rig.store.apply_saved(
        ticket,
        SaveReport {
            written: Vec::new(),
            end: SaveEnd::Conflict { file: "content.md" },
        },
        rig.t(),
    );
    let fx = rig.fx();
    assert!(reads(&fx).is_empty());
    let (delete, marks) = deletes(&fx).remove(0);
    assert_eq!(
        marks,
        [
            ("panel.json", FileMark::Hash(hash_of(&files[1].bytes))),
            ("content.md", FileMark::Hash(hash_of(b"one"))),
        ]
    );
    rig.store.apply_read(
        &at("p"),
        "panel.json",
        found(&files[1].bytes),
        rig.t(),
        &mut rig.ids,
    );
    rig.store.apply_read(
        &at("p"),
        "content.md",
        found(b"theirs"),
        rig.t(),
        &mut rig.ids,
    );
    assert!(rig.fx().is_empty());
    assert!(changed(&rig.ev()).is_empty());
    let (view, _) = rig.get("p");
    assert_eq!(view.state, PanelState::Removed);
    assert_eq!(view.revision, last.1);
    rig.store.apply_deleted(delete, done(&[]), rig.t());
    assert_eq!(lists(&rig.fx()), 1);
    let (view, _) = rig.get("p");
    assert_eq!((view.state, view.revision), (PanelState::Removed, last.1));
}

// ---- AC6

#[test]
fn invalid_json_keeps_last_good_and_is_never_written() {
    let mut rig = Rig::with_folders(&[("p", "one")]);
    let before = rig.get("p").0;
    rig.outside("panels/p/panel.json");
    rig.store.apply_read(
        &at("p"),
        "panel.json",
        found(b"{not json"),
        rig.t(),
        &mut rig.ids,
    );
    let fx = rig.fx();
    assert!(reads(&fx).is_empty());
    let (view, source) = rig.get("p");
    assert_eq!(
        (view.revision, view.state, source.as_str()),
        (before.revision, PanelState::Validated, "one")
    );
    let note = view.file_error.unwrap();
    assert!(note.starts_with("panel.json can't be used: panel.json isn't valid JSON: "));
    assert!(note.ends_with(". Showing the last good spec."));
    assert_eq!(changed(&rig.ev()).len(), 1);
    rig.tick(60_000);
    assert!(saves(&rig.fx()).is_empty());
}

#[test]
fn unsupported_version_keeps_last_good() {
    let mut rig = Rig::with_folders(&[("p", "one")]);
    let revision = rig.get("p").0.revision;
    rig.outside("panels/p/panel.json");
    rig.store.apply_read(
        &at("p"),
        "panel.json",
        found(br#"{"version":2,"renderer":"markdown"}"#),
        rig.t(),
        &mut rig.ids,
    );
    let (view, source) = rig.get("p");
    assert_eq!((view.revision, source.as_str()), (revision, "one"));
    assert!(view
        .file_error
        .unwrap()
        .contains("unsupported version 2; this daemon reads version 1"));
}

fn make_invalid(rig: &mut Rig, id: &str) {
    rig.outside(&format!("panels/{id}/panel.json"));
    rig.store.apply_read(
        &at(id),
        "panel.json",
        found(b"{nope"),
        rig.t(),
        &mut rig.ids,
    );
    rig.fx();
    rig.ev();
}

#[test]
fn content_update_while_invalid_writes_only_the_content() {
    let mut rig = Rig::with_folders(&[("p", "one")]);
    make_invalid(&mut rig, "p");
    rig.update("p", content_update("two")).unwrap();
    let (_, files) = saves(&rig.fx()).remove(0);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].file, "content.md");
    assert_eq!(files[0].bytes, b"two");
    assert_eq!(files[0].expected, Some(FileMark::Hash(hash_of(b"one"))));
}

#[test]
fn title_update_while_invalid_is_refused() {
    let mut rig = Rig::with_folders(&[("p", "one")]);
    make_invalid(&mut rig, "p");
    let error = rig.update("p", title_update(Some("t"))).unwrap_err();
    let PanelApiError::PanelFileInvalid { reason } = &error else {
        panic!("{error:?}");
    };
    assert!(!reason.is_empty());
    let text = error.to_string();
    assert!(text.starts_with("panel.json can't be used ("), "{text}");
    assert!(
        text.ends_with("), so the daemon doesn't rewrite it"),
        "{text}"
    );
    assert!(rig.fx().is_empty());
}

#[test]
fn failed_read_blocks_writes_and_rereads_after_5s() {
    let mut rig = Rig::with_folders(&[("p", "one")]);
    let revision = rig.get("p").0.revision;
    rig.outside("panels/p/panel.json");
    let failed_at = rig.now;
    rig.store.apply_read(
        &at("p"),
        "panel.json",
        PanelRead::Failed("denied".to_owned()),
        rig.t(),
        &mut rig.ids,
    );
    let (view, source) = rig.get("p");
    assert_eq!((view.revision, source.as_str()), (revision, "one"));
    assert_eq!(
        view.file_error.as_deref(),
        Some("Couldn't read .workspace/panels/p/panel.json (denied). Showing the last good revision. Changes can't be saved until it can be read.")
    );
    let refusal = PanelApiError::Unreadable {
        path: ".workspace/panels/p/panel.json".to_owned(),
        detail: "denied".to_owned(),
    };
    assert_eq!(
        rig.update("p", content_update("x")).unwrap_err(),
        refusal.clone()
    );
    assert_eq!(
        rig.store.remove(&space(), "p", rig.t()).unwrap_err(),
        refusal.clone()
    );
    assert!(saves(&rig.fx()).is_empty() && deletes(&rig.fx()).is_empty());

    rig.set(failed_at + 4_999);
    rig.store.tick(rig.t(), 0);
    assert!(reads(&rig.fx()).is_empty());
    rig.set(failed_at + 5_000);
    rig.store.tick(rig.t(), 0);
    assert_eq!(reads(&rig.fx()), [("p".to_owned(), "panel.json")]);
    rig.pair("p", found(md_json()), found(b"one"));
    let (view, _) = rig.get("p");
    assert!(view.revision > revision);
    assert_eq!(view.file_error, None);
    assert!(rig.update("p", content_update("x")).is_ok());
}

fn fail_save(rig: &mut Rig, ticket: SaveTicket, file: &'static str) {
    rig.store.apply_saved(
        ticket,
        SaveReport {
            written: Vec::new(),
            end: SaveEnd::Failed {
                file,
                error: StoreError::Unavailable("busy".to_owned()),
            },
        },
        rig.t(),
    );
}

#[test]
fn a_pending_title_retry_never_rewrites_an_invalid_panel_json() {
    let mut rig = Rig::new();
    rig.saved("p", "one");
    let r = rig.update("p", title_update(Some("t"))).unwrap().revision;
    let (ticket, files) = saves(&rig.fx()).remove(0);
    assert_eq!(files[0].file, "panel.json");
    fail_save(&mut rig, ticket, "panel.json");
    assert_eq!(
        rig.get("p").0.file_error.as_deref(),
        Some("Couldn't save .workspace/panels/p/panel.json (busy); retrying.")
    );
    make_invalid(&mut rig, "p");
    rig.tick(rig.timing.retry_ms + 1);
    assert!(saves(&rig.fx()).is_empty());
    assert_eq!(rig.get("p").0.dropped, Some(r));
}

#[test]
fn an_adopted_outside_edit_cancels_pending_saves() {
    let mut rig = Rig::new();
    rig.saved("p", "one");
    rig.update("p", content_update("mine")).unwrap();
    let (ticket, _) = saves(&rig.fx()).remove(0);
    fail_save(&mut rig, ticket, "content.md");
    rig.outside("panels/p/content.md");
    rig.pair("p", found(md_json()), found(b"theirs"));
    rig.fx();
    rig.tick(rig.timing.retry_ms + 1);
    assert!(saves(&rig.fx()).is_empty());
    rig.update("p", content_update("next")).unwrap();
    let (_, files) = saves(&rig.fx()).remove(0);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].expected, Some(FileMark::Hash(hash_of(b"theirs"))));
}

#[test]
fn no_save_runs_while_a_read_has_failed() {
    let mut rig = Rig::new();
    rig.saved("p", "one");
    rig.update("p", content_update("two")).unwrap();
    let (ticket, _) = saves(&rig.fx()).remove(0);
    fail_save(&mut rig, ticket, "content.md");
    rig.outside("panels/p/panel.json");
    rig.store.apply_read(
        &at("p"),
        "panel.json",
        PanelRead::Failed("denied".to_owned()),
        rig.t(),
        &mut rig.ids,
    );
    rig.fx();
    rig.tick(rig.timing.retry_ms + 1);
    assert!(saves(&rig.fx()).is_empty());
}

// ---- AC7

#[test]
fn same_key_returns_the_same_panel_without_new_work() {
    let mut rig = Rig::new();
    let mut create = create_of("a", Renderer::Markdown, "one");
    create.idempotency_key = Some("k".to_owned());
    let first = rig
        .store
        .create(&space(), create, rig.t(), 0, &mut rig.ids)
        .unwrap();
    rig.finish_save();
    rig.fx();
    rig.ev();
    let mut again = create_of("b", Renderer::Markdown, "other");
    again.idempotency_key = Some("k".to_owned());
    let second = rig
        .store
        .create(&space(), again, rig.t(), 0, &mut rig.ids)
        .unwrap();
    assert_eq!(second.id, "a");
    assert_eq!(second.revision, first.revision);
    assert!(rig.fx().is_empty());
    assert!(rig.ev().is_empty());
    assert!(rig.store.get(&space(), "b").is_err());
}

#[test]
fn same_key_in_another_space_creates_a_new_panel() {
    let mut rig = Rig::new();
    let other = SpaceId("/work/other".to_owned());
    rig.store.add_space(&other, rig.t());
    rig.store.apply_listing(&other, Ok(None), rig.t());
    let mut create = create_of("a", Renderer::Markdown, "one");
    create.idempotency_key = Some("k".to_owned());
    rig.store
        .create(&space(), create.clone(), rig.t(), 0, &mut rig.ids)
        .unwrap();
    rig.fx();
    let view = rig
        .store
        .create(&other, create, rig.t(), 0, &mut rig.ids)
        .unwrap();
    assert_eq!(view.space, other);
    assert_eq!(saves(&rig.fx()).len(), 1);
}

#[test]
fn duplicate_keys_return_the_panel_whose_id_sorts_first() {
    let json = br#"{"version":1,"renderer":"markdown","idempotency_key":"k"}"#;
    let mut rig = Rig::new();
    rig.store
        .apply_listing(&space(), listing(&["b", "c"]), rig.t());
    rig.fx();
    rig.pair("b", found(json), found(b"b"));
    rig.pair("c", found(json), found(b"c"));
    rig.store.close(&space(), "b", rig.t()).unwrap();
    rig.store
        .apply_listing(&space(), listing(&["a", "b", "c"]), rig.t());
    rig.fx();
    rig.pair("a", found(json), found(b"a"));
    let mut create = create_of("z", Renderer::Markdown, "new");
    create.idempotency_key = Some("k".to_owned());
    let view = rig
        .store
        .create(&space(), create.clone(), rig.t(), 0, &mut rig.ids)
        .unwrap();
    assert_eq!(view.id, "a");
    rig.store.close(&space(), "a", rig.t()).unwrap();
    let view = rig
        .store
        .create(&space(), create, rig.t(), 0, &mut rig.ids)
        .unwrap();
    assert_eq!(view.id, "c");
}

// ---- AC8

fn fill(rig: &mut Rig, count: usize) {
    for index in 0..count {
        rig.md(&format!("p{index:02}"), "x");
    }
    for (ticket, files) in saves(&rig.fx()) {
        rig.store.apply_saved(ticket, done(&files), rig.t());
    }
    rig.fx();
    rig.ev();
}

#[test]
fn the_101st_panel_is_refused_with_panel_limit() {
    let mut rig = Rig::new();
    fill(&mut rig, 100);
    let error = rig.create("extra", Renderer::Markdown, "x").unwrap_err();
    assert_eq!(
        error,
        PanelApiError::PanelLimit {
            count: 100,
            limit: 100
        }
    );
    assert_eq!(
        error.to_string(),
        "the space already has 100 panels; the limit is 100"
    );
    assert!(rig.fx().is_empty());
    assert!(rig.ev().is_empty());
}

#[test]
fn create_after_a_removal_works() {
    let mut rig = Rig::new();
    fill(&mut rig, 100);
    rig.store.remove(&space(), "p00", rig.t()).unwrap();
    let (ticket, _) = deletes(&rig.fx()).remove(0);
    rig.store.apply_deleted(ticket, done(&[]), rig.t());
    assert!(rig.create("extra", Renderer::Markdown, "x").is_ok());
}

#[test]
fn a_folder_without_panel_json_does_not_count_or_take_its_id() {
    let mut rig = Rig::new();
    let names: Vec<String> = (0..100).map(|n| format!("d{n:03}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    rig.store.apply_listing(&space(), listing(&refs), rig.t());
    rig.fx();
    for name in &refs[..99] {
        rig.pair(name, found(md_json()), found(b"x"));
    }
    rig.store.apply_read(
        &at("d099"),
        "panel.json",
        PanelRead::Missing,
        rig.t(),
        &mut rig.ids,
    );
    assert!(rig.create("d099", Renderer::Markdown, "x").is_ok());
    assert_eq!(
        rig.create("extra", Renderer::Markdown, "x").unwrap_err(),
        PanelApiError::PanelLimit {
            count: 100,
            limit: 100
        }
    );
}

#[test]
fn remove_compares_both_files_then_deletes_panel_json_first() {
    let mut rig = Rig::new();
    rig.md("p", "one");
    let written = rig.finish_save();
    rig.fx();
    let view = rig.store.remove(&space(), "p", rig.t()).unwrap();
    assert_eq!(view.state, PanelState::Removed);
    let fx = rig.fx();
    let (_, files) = deletes(&fx).remove(0);
    assert_eq!(
        files,
        [
            ("panel.json", FileMark::Hash(hash_of(&written[1].bytes))),
            ("content.md", FileMark::Hash(hash_of(b"one"))),
        ]
    );
    assert!(matches!(
        &fx[0],
        PanelEffect::Delete {
            at: PanelFolder::Workspace { .. },
            ..
        }
    ));
    let (ticket, _) = deletes(&fx).remove(0);
    rig.store.apply_deleted(ticket, done(&[]), rig.t());
    assert_eq!(lists(&rig.fx()), 1);
    assert_eq!(rig.get("p").0.state, PanelState::Removed);
    assert!(rig.store.list(&space()).unwrap().panels.is_empty());
    assert!(rig.create("p", Renderer::Markdown, "again").is_ok());
}

#[test]
fn create_into_a_folder_with_a_leftover_content_file_works() {
    let mut rig = Rig::new();
    rig.store.apply_listing(&space(), listing(&["x"]), rig.t());
    rig.fx();
    rig.store.apply_read(
        &at("x"),
        "panel.json",
        PanelRead::Missing,
        rig.t(),
        &mut rig.ids,
    );
    rig.md("x", "new");
    let (_, files) = saves(&rig.fx()).remove(0);
    assert_eq!(files[0].expected, None);
    assert_eq!(files[1].expected, Some(FileMark::Missing));
}

#[test]
fn a_remove_conflict_restores_the_panel_and_forgets_the_removal() {
    let mut rig = Rig::new();
    let revision = rig.mm("d", "x").revision;
    rig.finish_save();
    rig.fx();
    rig.verdict(
        "d",
        revision,
        Ok(DiagramVerdict::Failed(failure(FailureCode::Syntax))),
    );
    let before = rig.get("d").0;
    rig.ev();
    rig.store.remove(&space(), "d", rig.t()).unwrap();
    assert_eq!(rig.get("d").0.state, PanelState::Removed);
    let (ticket, _) = deletes(&rig.fx()).remove(0);
    rig.store.apply_deleted(
        ticket,
        SaveReport {
            written: Vec::new(),
            end: SaveEnd::Conflict { file: "panel.json" },
        },
        rig.t(),
    );
    let after = rig.get("d").0;
    assert_eq!((after.state, after.error), (before.state, before.error));
    assert_eq!(reads(&rig.fx()), [("d".to_owned(), "panel.json")]);
    assert_eq!(changed(&rig.ev()).last().unwrap().2, PanelState::Failed);
}

fn conflicted(rig: &mut Rig) -> u64 {
    let r = rig.md("x", "ours").revision;
    let (ticket, _) = saves(&rig.fx()).remove(0);
    rig.ev();
    rig.store.apply_saved(
        ticket,
        SaveReport {
            written: Vec::new(),
            end: SaveEnd::Conflict { file: "panel.json" },
        },
        rig.t(),
    );
    r
}

fn conflict_checks(rig: &mut Rig, r: u64) {
    let fx = rig.fx();
    let expected = String::from(
        "the panel x of /work/app isn't created: .workspace/panels/x/panel.json changed while it was written; the folder is read instead"
    );
    assert_eq!(logs(&fx), [expected]);
    assert_eq!(reads(&fx), [("x".to_owned(), "panel.json")]);
    assert!(saves(&fx).is_empty() && deletes(&fx).is_empty() && lists(&fx) == 0);
    let events = rig.ev();
    assert!(changed(&events).contains(&("x".to_owned(), r, PanelState::Removed)));
    let (view, source) = rig.get("x");
    assert_eq!(
        (view.state, view.revision, source.as_str()),
        (PanelState::Removed, r, "")
    );
    assert_eq!(
        rig.create("x", Renderer::Markdown, "again").unwrap_err(),
        PanelApiError::IdTaken { id: "x".to_owned() }
    );
}

#[test]
fn a_create_save_that_finds_a_panel_json_drops_the_record_and_reads_the_folder() {
    // case a: a view panel is there
    let mut rig = Rig::new();
    let r = conflicted(&mut rig);
    conflict_checks(&mut rig, r);
    rig.fx();
    rig.pair("x", found(md_json()), found(b"theirs"));
    let (view, source) = rig.get("x");
    assert_eq!(view.lifetime, Lifetime::Persistent);
    assert_eq!(
        (view.state, source.as_str()),
        (PanelState::Validated, "theirs")
    );
    assert!(view.revision > r);
    assert!(saves(&rig.fx()).is_empty());
    assert_eq!(rig.store.list(&space()).unwrap().panels.len(), 1);

    // case b: something that is not a view panel is there
    let mut rig = Rig::new();
    let r = conflicted(&mut rig);
    conflict_checks(&mut rig, r);
    rig.fx();
    rig.store.apply_read(
        &at("x"),
        "panel.json",
        found(br#"{"version":1}"#),
        rig.t(),
        &mut rig.ids,
    );
    assert!(reads(&rig.fx()).is_empty());
    let list = rig.store.list(&space()).unwrap();
    assert_eq!(list.skipped.len(), 1);
    assert_eq!(
        (
            list.skipped[0].panel.as_str(),
            list.skipped[0].reason.as_str()
        ),
        ("x", "not a view panel")
    );
    let (view, _) = rig.get("x");
    assert_eq!((view.state, view.revision), (PanelState::Removed, r));
    assert_eq!(
        rig.create("x", Renderer::Markdown, "again").unwrap_err(),
        PanelApiError::IdTaken { id: "x".to_owned() }
    );
}

// ---- AC9 (Persistent half), AC11, AC12, fallback reads, loading

#[test]
fn close_of_a_persistent_panel_keeps_files_and_reports_removed() {
    let mut rig = Rig::new();
    rig.saved("p", "one");
    let view = rig.store.close(&space(), "p", rig.t()).unwrap();
    assert_eq!(view.state, PanelState::Removed);
    let fx = rig.fx();
    assert!(saves(&fx).is_empty() && deletes(&fx).is_empty());
    assert_eq!(changed(&rig.ev())[0].2, PanelState::Removed);
    let list = rig.store.list(&space()).unwrap();
    assert_eq!(list.panels.len(), 1);
    assert_eq!(list.panels[0].state, PanelState::Removed);
    assert_eq!(rig.get("p").1, "");
    assert_eq!(
        rig.store.close(&space(), "p", rig.t()).unwrap().state,
        PanelState::Removed
    );
    assert_eq!(
        rig.update("p", content_update("x")).unwrap_err(),
        PanelApiError::Removed { id: "p".to_owned() }
    );
}

#[test]
fn outside_edit_revives_a_closed_persistent_panel() {
    let mut rig = Rig::with_folders(&[("p", "one")]);
    rig.store.close(&space(), "p", rig.t()).unwrap();
    let closed = rig.get("p").0.revision;
    rig.outside("panels/p/content.md");
    rig.pair("p", found(md_json()), found(b"back"));
    let (view, source) = rig.get("p");
    assert_eq!(
        (view.state, source.as_str()),
        (PanelState::Validated, "back")
    );
    assert!(view.revision > closed);
}

#[test]
fn removed_ids_are_remembered_up_to_1000() {
    let mut rig = Rig::new();
    for index in 0..1_001 {
        let id = format!("p{index}");
        rig.md(&id, "x");
        rig.finish_save();
        rig.store.remove(&space(), &id, rig.t()).unwrap();
        let (ticket, _) = deletes(&rig.fx()).remove(0);
        rig.store.apply_deleted(ticket, done(&[]), rig.t());
        rig.fx();
    }
    assert_eq!(
        rig.store.get(&space(), "p0").unwrap_err(),
        PanelApiError::UnknownPanel {
            id: "p0".to_owned()
        }
    );
    for id in ["p1", "p1000"] {
        let (view, source) = rig.get(id);
        assert_eq!((view.state, source.as_str()), (PanelState::Removed, ""));
    }
}

#[test]
fn create_refuses_a_source_that_cannot_fit_one_frame() {
    let mut rig = Rig::new();
    let content = "\u{1}".repeat(204_800);
    let error = rig.create("p", Renderer::Markdown, &content).unwrap_err();
    let PanelApiError::TooLarge { encoded } = error else {
        panic!("{error:?}");
    };
    assert!(encoded > 1_048_576);
    assert_eq!(
        error.to_string(),
        format!("the source can't be sent in one frame: its panel reply would be {encoded} bytes; the limit is 1048576")
    );
    assert!(rig.fx().is_empty() && rig.ev().is_empty());
}

#[test]
fn outside_content_that_cannot_fit_fails_too_large_with_an_empty_source() {
    let mut rig = Rig::new();
    rig.store.apply_listing(&space(), listing(&["p"]), rig.t());
    rig.fx();
    rig.pair(
        "p",
        found(md_json()),
        found("\u{1}".repeat(204_800).as_bytes()),
    );
    let (view, source) = rig.get("p");
    assert_eq!(view.state, PanelState::Failed);
    let error = view.error.unwrap();
    assert_eq!(error.code, "too_large");
    assert!(error
        .message
        .starts_with("the source can't be sent in one frame: its panel reply would be "));
    assert!(error.message.ends_with(" bytes; the limit is 1048576"));
    assert_eq!(source, "");
    assert!(panel_reply_len(&space(), "p", Renderer::Markdown, "") <= 1_048_576);
}

#[test]
fn too_large_reads_fail_with_the_read_size() {
    let mut rig = Rig::new();
    rig.store.apply_listing(&space(), listing(&["p"]), rig.t());
    rig.fx();
    rig.pair(
        "p",
        found(md_json()),
        PanelRead::TooLarge { size: 2_000_000 },
    );
    let error = rig.get("p").0.error.unwrap();
    assert_eq!(
        error.message,
        "the source is 2000000 bytes; the limit is 204800"
    );
    assert_eq!(rig.get("p").1, "");
}

#[test]
fn a_fallback_read_makes_the_panel_read_only() {
    let mut rig = Rig::with_folders(&[("p", "one")]);
    rig.outside("panels/p/content.md");
    rig.pair("p", fallback(md_json()), fallback(b"copy"));
    let (view, source) = rig.get("p");
    assert!(view.read_only);
    assert_eq!(
        (view.state, source.as_str()),
        (PanelState::Validated, "copy")
    );
    rig.fx();
    assert_eq!(
        rig.update("p", content_update("x")).unwrap_err(),
        PanelApiError::ReadOnly
    );
    assert_eq!(
        rig.store.remove(&space(), "p", rig.t()).unwrap_err(),
        PanelApiError::ReadOnly
    );
    assert_eq!(
        rig.store.pin(&space(), "p", rig.t()).unwrap_err(),
        PanelApiError::ReadOnly
    );
    assert_eq!(
        PanelApiError::ReadOnly.to_string(),
        "the panel's folder is read-only; its files can't be changed"
    );
    assert!(rig.fx().is_empty());
    assert_eq!(
        rig.store.close(&space(), "p", rig.t()).unwrap().state,
        PanelState::Removed
    );
}

#[test]
fn a_project_read_clears_read_only() {
    let mut rig = Rig::with_folders(&[("p", "one")]);
    rig.outside("panels/p/content.md");
    rig.pair("p", fallback(md_json()), fallback(b"copy"));
    assert!(rig.get("p").0.read_only);
    rig.outside("panels/p/content.md");
    rig.pair("p", found(md_json()), found(b"copy"));
    assert!(!rig.get("p").0.read_only);
    assert!(rig.update("p", content_update("x")).is_ok());
}

#[test]
fn create_waits_for_every_first_read() {
    let mut rig = Rig::new();
    let mut fresh = started_store(rig.timing);
    fresh.add_space(&space(), rig.t());
    fresh.apply_listing(&space(), listing(&["x"]), rig.t());
    fresh.take_effects();
    let gate = |store: &mut PanelStore, ids: &mut Counter| {
        store
            .create(
                &space(),
                create_of("y", Renderer::Markdown, "a"),
                Millis(0),
                0,
                ids,
            )
            .unwrap_err()
    };
    assert_eq!(gate(&mut fresh, &mut rig.ids), PanelApiError::SpaceLoading);
    fresh.apply_read(
        &at("x"),
        "panel.json",
        found(md_json()),
        Millis(0),
        &mut rig.ids,
    );
    assert_eq!(
        reads(&fresh.take_effects()),
        [("x".to_owned(), "content.md")]
    );
    assert_eq!(gate(&mut fresh, &mut rig.ids), PanelApiError::SpaceLoading);
    fresh.apply_read(&at("x"), "content.md", found(b"c"), Millis(0), &mut rig.ids);
    assert!(fresh
        .create(
            &space(),
            create_of("y", Renderer::Markdown, "a"),
            Millis(0),
            0,
            &mut rig.ids
        )
        .is_ok());
}

#[test]
fn a_failed_listing_refuses_create_until_a_listing_succeeds() {
    let mut rig = Rig::new();
    let mut store = started_store(rig.timing);
    store.add_space(&space(), rig.t());
    store.take_effects();
    store.apply_listing(
        &space(),
        Err(StoreError::Unavailable("boom".to_owned())),
        rig.t(),
    );
    let error = store
        .create(
            &space(),
            create_of("p", Renderer::Markdown, "a"),
            rig.t(),
            0,
            &mut rig.ids,
        )
        .unwrap_err();
    assert_eq!(
        error,
        PanelApiError::ListingFailed {
            detail: "boom".to_owned()
        }
    );
    assert!(store.take_effects().is_empty());
    let list = store.list(&space()).unwrap();
    assert_eq!(
        list.error.as_deref(),
        Some("Couldn't list .workspace/panels (boom)")
    );
    assert!(store
        .next_deadline()
        .is_some_and(|d| d <= rig.t().saturating_add(Millis(rig.timing.retry_ms))));
    store.tick(Millis(rig.now + rig.timing.retry_ms - 1), 0);
    assert_eq!(lists(&store.take_effects()), 0);
    store.tick(Millis(rig.now + rig.timing.retry_ms), 0);
    assert_eq!(lists(&store.take_effects()), 1);
    store.apply_listing(&space(), Ok(None), rig.t());
    assert!(store
        .create(
            &space(),
            create_of("p", Renderer::Markdown, "a"),
            rig.t(),
            0,
            &mut rig.ids
        )
        .is_ok());
    assert_eq!(store.list(&space()).unwrap().error, None);
}

#[test]
fn unknown_ids_while_loading_are_space_loading() {
    let mut rig = Rig::new();
    let mut store = started_store(rig.timing);
    store.add_space(&space(), rig.t());
    store.apply_listing(&space(), listing(&["x"]), rig.t());
    assert_eq!(
        store.get(&space(), "zzz").unwrap_err(),
        PanelApiError::SpaceLoading
    );
    assert_eq!(
        store
            .update(
                &space(),
                "zzz",
                content_update("a"),
                rig.t(),
                0,
                &mut rig.ids
            )
            .unwrap_err(),
        PanelApiError::SpaceLoading
    );
    assert_eq!(
        store.close(&space(), "zzz", rig.t()).unwrap_err(),
        PanelApiError::SpaceLoading
    );
    assert_eq!(
        store.pin(&space(), "zzz", rig.t()).unwrap_err(),
        PanelApiError::SpaceLoading
    );
    assert_eq!(
        store.list(&space()).unwrap_err(),
        PanelApiError::SpaceLoading
    );

    assert_eq!(
        rig.store.get(&space(), "zzz").unwrap_err(),
        PanelApiError::UnknownPanel {
            id: "zzz".to_owned()
        }
    );
    let other = SpaceId("/nowhere".to_owned());
    assert_eq!(
        rig.store.get(&other, "p").unwrap_err(),
        PanelApiError::UnknownSpace
    );
}

#[test]
fn pin_refuses_before_it_does_anything() {
    let mut rig = Rig::new();
    rig.saved("p", "one");
    assert_eq!(
        rig.store.pin(&space(), "p", rig.t()).unwrap_err(),
        PanelApiError::NotScratch { id: "p".to_owned() }
    );
    assert_eq!(
        PanelApiError::NotScratch { id: "p".to_owned() }.to_string(),
        "the panel p is already in .workspace/"
    );
    rig.store.close(&space(), "p", rig.t()).unwrap();
    assert_eq!(
        rig.store.pin(&space(), "p", rig.t()).unwrap_err(),
        PanelApiError::Removed { id: "p".to_owned() }
    );
    assert_eq!(
        rig.store.pin(&space(), "q", rig.t()).unwrap_err(),
        PanelApiError::UnknownPanel { id: "q".to_owned() }
    );
}

#[test]
fn listing_reads_the_first_100_and_notes_the_rest() {
    let mut rig = Rig::new();
    let mut store = started_store(rig.timing);
    store.add_space(&space(), rig.t());
    store.take_effects();
    let mut entries: Vec<ListedEntry> = (0..101)
        .map(|n| entry(&format!("d{n:03}"), EntryKind::Directory))
        .collect();
    entries.push(entry("link", EntryKind::Symlink));
    entries.push(entry(".hidden", EntryKind::Directory));
    entries.push(entry("a-file", EntryKind::File));
    store.apply_listing(
        &space(),
        Ok(Some(DirListing {
            entries,
            truncated: true,
        })),
        rig.t(),
    );
    let read = reads(&store.take_effects());
    assert_eq!(read.len(), 100);
    assert_eq!(read[0].0, "d000");
    let list = {
        for (name, _) in &read {
            store.apply_read(
                &at(name),
                "panel.json",
                PanelRead::Missing,
                rig.t(),
                &mut rig.ids,
            );
        }
        store.list(&space()).unwrap()
    };
    assert_eq!(
        list.error.as_deref(),
        Some(".workspace/panels has more than 256 entries; only some were read; .workspace/panels has more than 100 panel folders; only the first 100 by name were read")
    );
    assert!(list.skipped.iter().any(|s| s.panel == "link"));
    assert!(store
        .create(
            &space(),
            create_of("n", Renderer::Markdown, "x"),
            rig.t(),
            0,
            &mut rig.ids
        )
        .is_err());
    assert!(store
        .take_events()
        .iter()
        .any(|e| matches!(e, PanelEvent::Listing { .. })));
}

#[test]
fn api_errors_display_their_texts() {
    let cases = [
        (PanelApiError::UnknownSpace, "the host lists no such space"),
        (
            PanelApiError::SpaceLoading,
            "the space's panels are still being read; try again",
        ),
        (
            PanelApiError::UnknownPanel { id: "a".into() },
            "the space has no panel a",
        ),
        (
            PanelApiError::Removed { id: "a".into() },
            "the panel a was removed",
        ),
        (
            PanelApiError::InvalidId { why: "w".into() },
            "the panel ID isn't valid: w",
        ),
        (
            PanelApiError::IdTaken { id: "a".into() },
            "the space already has a panel folder a",
        ),
        (
            PanelApiError::InvalidField {
                field: "update",
                rule: "must change the content or the title".into(),
            },
            "update must change the content or the title",
        ),
        (
            PanelApiError::ReadOnly,
            "the panel's folder is read-only; its files can't be changed",
        ),
        (
            PanelApiError::PanelLimit {
                count: 100,
                limit: 100,
            },
            "the space already has 100 panels; the limit is 100",
        ),
        (
            PanelApiError::TooLarge { encoded: 9 },
            "the source can't be sent in one frame: its panel reply would be 9 bytes; the limit is 1048576",
        ),
        (
            PanelApiError::NotScratch { id: "a".into() },
            "the panel a is already in .workspace/",
        ),
        (
            PanelApiError::ListingFailed {
                detail: "boom".into(),
            },
            "the space's panels couldn't be listed: boom",
        ),
        (
            PanelApiError::ScratchListingFailed {
                detail: "gone".into(),
            },
            "the scratch panels couldn't be listed: gone",
        ),
        (
            PanelApiError::PanelFileInvalid {
                reason: "bad".into(),
            },
            "panel.json can't be used (bad), so the daemon doesn't rewrite it",
        ),
        (
            PanelApiError::Unreadable {
                path: ".workspace/panels/p/panel.json".into(),
                detail: "denied".into(),
            },
            "couldn't read .workspace/panels/p/panel.json (denied), so the panel isn't changed until it can be read",
        ),
        (PanelApiError::Stopped, "the server has stopped"),
    ];
    for (error, text) in cases {
        assert_eq!(error.to_string(), text);
    }
}

#[test]
fn field_rules_refuse_before_any_work() {
    let mut rig = Rig::new();
    let mut create = create_of("p", Renderer::Markdown, "x");
    create.title = Some(String::new());
    let error = rig
        .store
        .create(&space(), create, rig.t(), 0, &mut rig.ids)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "title must be a string of 1 to 1024 bytes with no control characters"
    );
    assert_eq!(
        rig.update(
            "q",
            PanelUpdate {
                content: None,
                title: None
            }
        )
        .unwrap_err(),
        PanelApiError::UnknownPanel { id: "q".to_owned() }
    );
    rig.saved("p", "x");
    assert_eq!(
        rig.update(
            "p",
            PanelUpdate {
                content: None,
                title: None
            }
        )
        .unwrap_err()
        .to_string(),
        "update must change the content or the title"
    );
    assert!(rig.fx().is_empty());
}

#[test]
fn create_refuses_a_256_byte_id() {
    let mut rig = Rig::new();
    let create = create_of(&"a".repeat(256), Renderer::Markdown, "x");
    let error = rig
        .store
        .create(&space(), create, rig.t(), 0, &mut rig.ids)
        .unwrap_err();
    assert_eq!(
        error,
        PanelApiError::InvalidId {
            why: "a panel ID can be at most 255 bytes".to_owned()
        }
    );
    assert_eq!(
        error.to_string(),
        "the panel ID isn't valid: a panel ID can be at most 255 bytes"
    );
    assert!(rig.fx().is_empty());
    let create = create_of(&"a".repeat(255), Renderer::Markdown, "x");
    rig.store
        .create(&space(), create, rig.t(), 0, &mut rig.ids)
        .unwrap();
}

#[test]
fn create_refuses_an_invalid_placement_and_stores_a_valid_one() {
    let mut rig = Rig::new();
    let mut create = create_of("p", Renderer::Markdown, "x");
    create.placement = Some("a\nb".to_owned());
    let error = rig
        .store
        .create(&space(), create, rig.t(), 0, &mut rig.ids)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "placement must be a tile ID of 1 to 256 bytes with no control characters"
    );
    assert!(rig.fx().is_empty());
    let mut create = create_of("p", Renderer::Markdown, "x");
    create.placement = Some("main".to_owned());
    rig.store
        .create(&space(), create, rig.t(), 0, &mut rig.ids)
        .unwrap();
    let (_, files) = saves(&rig.fx()).remove(0);
    let panel = files.iter().find(|file| file.file == "panel.json").unwrap();
    assert!(String::from_utf8_lossy(&panel.bytes).contains("\"placement\": \"main\""));
}

// ---- scratch panels (A2)

const EVERY: u64 = 3_600_000;
const DAY: u64 = 86_400_000;
const T: u64 = 1_000;

fn sdir(id: &str) -> ScratchDir {
    ScratchDir {
        key: space().sha256_hex(),
        panel: id.to_owned(),
    }
}

fn meta_json(changed_at_ms: u64) -> Vec<u8> {
    encode_scratch_meta(&space(), changed_at_ms)
}

fn scratch_of(id: &str, content: &str) -> PanelCreate {
    let mut create = create_of(id, Renderer::Markdown, content);
    create.lifetime = Lifetime::Scratch;
    create
}

fn scratch_saves(effects: &[PanelEffect]) -> Vec<(SaveTicket, ScratchDir, Vec<FileWrite>)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            PanelEffect::Save {
                ticket,
                at: PanelFolder::Scratch(dir),
                files,
            } => Some((*ticket, dir.clone(), files.clone())),
            _ => None,
        })
        .collect()
}

fn scratch_deletes(effects: &[PanelEffect]) -> Vec<(SaveTicket, ScratchDir)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            PanelEffect::Delete {
                ticket,
                at: PanelFolder::Scratch(dir),
                files,
            } => {
                assert!(files.is_empty());
                Some((*ticket, dir.clone()))
            }
            _ => None,
        })
        .collect()
}

fn workspace_saves(effects: &[PanelEffect]) -> Vec<(SaveTicket, Vec<FileWrite>)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            PanelEffect::Save {
                ticket,
                at: PanelFolder::Workspace { .. },
                files,
            } => Some((*ticket, files.clone())),
            _ => None,
        })
        .collect()
}

fn skipped_of(rig: &Rig) -> Vec<(String, String)> {
    rig.store
        .list(&space())
        .unwrap()
        .skipped
        .into_iter()
        .map(|skip| (skip.panel, skip.reason))
        .collect()
}

fn open_skip(id: &str) -> Vec<(String, String)> {
    vec![(id.to_owned(), SCRATCH_ID_OPEN.to_owned())]
}

fn has_listing(events: &[PanelEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, PanelEvent::Listing { .. }))
}

impl Rig {
    fn screate(&mut self, id: &str, content: &str, wall: u64) -> Result<PanelView, PanelApiError> {
        self.store.create(
            &space(),
            scratch_of(id, content),
            self.t(),
            wall,
            &mut self.ids,
        )
    }

    /// Creates a Scratch Markdown panel and applies its Save.
    fn ssaved(&mut self, id: &str, content: &str, wall: u64) -> PanelView {
        let view = self.screate(id, content, wall).unwrap();
        self.finish_save();
        self.fx();
        self.ev();
        view
    }

    fn supdate(
        &mut self,
        id: &str,
        update: PanelUpdate,
        wall: u64,
    ) -> Result<PanelView, PanelApiError> {
        self.store
            .update(&space(), id, update, self.t(), wall, &mut self.ids)
    }

    fn sweep(&mut self, now: u64, wall: u64) -> Vec<PanelEffect> {
        self.set(now);
        self.store.tick(self.t(), wall);
        self.fx()
    }

    /// A loaded space whose scratch folders `(id, changed_at_ms)` were read at start.
    fn from_scratch(items: &[(&str, u64)]) -> Rig {
        let timing = PanelTiming::default();
        let mut rig = Rig {
            store: PanelStore::new(timing),
            ids: Counter(1_000),
            now: 10_000,
            timing,
        };
        rig.store.start(rig.t());
        rig.store.add_space(&space(), rig.t());
        rig.store.apply_listing(&space(), Ok(None), rig.t());
        let dirs = items.iter().map(|(id, _)| sdir(id)).collect();
        rig.store.apply_scratch_listing(
            Ok(ScratchListing {
                dirs,
                truncated: false,
            }),
            rig.t(),
        );
        for (id, at) in items {
            let now = rig.t();
            let folder = PanelFolder::Scratch(sdir(id));
            rig.store.apply_read(
                &folder,
                "scratch.json",
                found(&meta_json(*at)),
                now,
                &mut rig.ids,
            );
            rig.store
                .apply_read(&folder, "panel.json", found(md_json()), now, &mut rig.ids);
            rig.store.apply_read(
                &folder,
                "content.md",
                found(id.as_bytes()),
                now,
                &mut rig.ids,
            );
        }
        rig.fx();
        rig.ev();
        rig
    }

    /// A scratch panel `x` open while the folder `panels/x` appeared outside. Returns every
    /// effect taken on the way.
    fn shadowed() -> (Rig, Vec<PanelEffect>) {
        let mut rig = Rig::new();
        rig.ssaved("x", "s1", T);
        rig.store
            .apply_change(&space(), "panels/x/panel.json", rig.t());
        rig.store.apply_change(&space(), "panels", rig.t());
        rig.tick(rig.timing.debounce_ms);
        let mut all = rig.fx();
        rig.store.apply_listing(&space(), listing(&["x"]), rig.t());
        all.extend(rig.fx());
        (rig, all)
    }
}

fn no_workspace_read_of(effects: &[PanelEffect], id: &str) {
    assert!(
        reads(effects).iter().all(|(panel, _)| panel != id),
        "{effects:?}"
    );
}

#[test]
fn scratch_create_saves_scratch_json_first_under_the_space_key() {
    let mut rig = Rig::new();
    let view = rig.screate("x", "hello", 5_000).unwrap();
    assert_eq!(view.lifetime, Lifetime::Scratch);
    let fx = rig.fx();
    let mut all = scratch_saves(&fx);
    assert_eq!(all.len(), 1, "{fx:?}");
    let (_, dir, files) = all.remove(0);
    assert_eq!(dir, sdir("x"));
    assert_eq!(dir.key, space().sha256_hex());
    let names: Vec<&str> = files.iter().map(|f| f.file).collect();
    assert_eq!(names, ["scratch.json", "content.md", "panel.json"]);
    assert!(files.iter().all(|f| f.expected.is_none()));
    assert_eq!(files[0].bytes, meta_json(5_000));
    assert!(workspace_saves(&fx).is_empty());
}

#[test]
fn scratch_panel_expires_7_days_after_its_last_change() {
    let mut rig = Rig::new();
    rig.ssaved("x", "a", T);
    assert!(scratch_deletes(&rig.sweep(20_000, T + SCRATCH_TTL_MS - 1)).is_empty());
    let fx = rig.sweep(20_000 + EVERY, T + SCRATCH_TTL_MS);
    assert_eq!(scratch_deletes(&fx).len(), 1);
    assert_eq!(scratch_deletes(&fx)[0].1, sdir("x"));
    assert_eq!(
        rig.ev()
            .iter()
            .filter(|e| matches!(
                e,
                PanelEvent::Changed {
                    state: PanelState::Removed,
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(rig.get("x").0.state, PanelState::Removed);
}

#[test]
fn a_scratch_panel_created_this_run_expires_without_a_restart() {
    let mut rig = Rig::new();
    assert!(scratch_deletes(&rig.sweep(20_000, 0)).is_empty());
    rig.ssaved("x", "a", 50_000);
    assert!(scratch_deletes(&rig.sweep(20_000 + EVERY, 50_000 + SCRATCH_TTL_MS - 1)).is_empty());
    let fx = rig.sweep(20_000 + 2 * EVERY, 50_000 + SCRATCH_TTL_MS);
    assert_eq!(scratch_deletes(&fx).len(), 1);
}

#[test]
fn update_restamps_changed_at() {
    let mut rig = Rig::new();
    rig.ssaved("x", "a", T);
    rig.supdate("x", content_update("b"), T + 9_000).unwrap();
    let fx = rig.fx();
    let (_, dir, files) = scratch_saves(&fx).remove(0);
    assert_eq!(dir, sdir("x"));
    let names: Vec<&str> = files.iter().map(|f| f.file).collect();
    assert_eq!(names, ["scratch.json", "content.md"]);
    assert_eq!(files[0].bytes, meta_json(T + 9_000));
    rig.supdate("x", title_update(Some("t")), T + 9_500)
        .unwrap();
}

#[test]
fn an_update_on_day_6_prevents_expiry_on_day_7() {
    let mut rig = Rig::from_scratch(&[("x", T)]);
    rig.supdate("x", content_update("n"), T + 6 * DAY).unwrap();
    rig.finish_save();
    rig.fx();
    assert!(scratch_deletes(&rig.sweep(20_000, T + 7 * DAY + 1)).is_empty());
    let fx = rig.sweep(20_000 + EVERY, T + 13 * DAY + 1);
    assert_eq!(scratch_deletes(&fx).len(), 1);
}

#[test]
fn close_deletes_a_scratch_panel_and_reports_removed() {
    let mut rig = Rig::new();
    rig.ssaved("x", "a", T);
    let view = rig.store.close(&space(), "x", rig.t()).unwrap();
    assert_eq!(view.state, PanelState::Removed);
    assert_eq!(view.lifetime, Lifetime::Scratch);
    let fx = rig.fx();
    assert_eq!(
        scratch_deletes(&fx),
        [(scratch_deletes(&fx)[0].0, sdir("x"))]
    );
    assert_eq!(rig.get("x").0.state, PanelState::Removed);
    assert!(rig.store.list(&space()).unwrap().panels.is_empty());
    assert!(changed(&rig.ev())
        .iter()
        .any(|(_, _, state)| *state == PanelState::Removed));
}

#[test]
fn pin_writes_into_workspace_then_deletes_scratch() {
    let mut rig = Rig::new();
    let before = rig.ssaved("x", "a", T);
    rig.store.pin(&space(), "x", rig.t()).unwrap();
    let fx = rig.fx();
    let (ticket, files) = workspace_saves(&fx).remove(0);
    assert_eq!(files[0].file, "content.md");
    assert_eq!(files[0].expected, None);
    assert_eq!(files[1].file, "panel.json");
    assert_eq!(files[1].expected, Some(FileMark::Missing));
    assert!(scratch_deletes(&fx).is_empty());
    let view = rig.get("x").0;
    assert_eq!(view.lifetime, Lifetime::Persistent);
    assert_eq!(view.revision, before.revision);
    assert_eq!(view.state, before.state);
    rig.store.apply_saved(ticket, done(&files), rig.t());
    let fx = rig.fx();
    assert_eq!(scratch_deletes(&fx).len(), 1);
    assert_eq!(scratch_deletes(&fx)[0].1, sdir("x"));
    assert_eq!(rig.get("x").0.lifetime, Lifetime::Persistent);
}

#[test]
fn pin_of_a_persistent_panel_is_not_scratch() {
    let mut rig = Rig::new();
    rig.saved("p", "one");
    assert_eq!(
        rig.store.pin(&space(), "p", rig.t()).unwrap_err(),
        PanelApiError::NotScratch { id: "p".to_owned() }
    );
    assert!(rig.fx().is_empty());
}

#[test]
fn scratch_folder_without_valid_scratch_json_is_never_swept() {
    let timing = PanelTiming::default();
    let mut store = started_store(timing);
    let mut ids = Counter(1);
    store.add_space(&space(), Millis(0));
    store.apply_listing(&space(), Ok(None), Millis(0));
    store.take_effects();
    store.apply_scratch_listing(
        Ok(ScratchListing {
            dirs: vec![sdir("a"), sdir("b")],
            truncated: false,
        }),
        Millis(0),
    );
    store.apply_read(
        &PanelFolder::Scratch(sdir("a")),
        "scratch.json",
        PanelRead::Missing,
        Millis(0),
        &mut ids,
    );
    store.apply_read(
        &PanelFolder::Scratch(sdir("b")),
        "scratch.json",
        found(b"{"),
        Millis(0),
        &mut ids,
    );
    store.take_effects();
    for n in 0..3 {
        store.tick(Millis(n * EVERY), 100 * DAY);
        assert!(scratch_deletes(&store.take_effects()).is_empty());
    }
}

#[test]
fn a_pinned_panel_is_never_swept() {
    let mut rig = Rig::new();
    rig.ssaved("x", "a", T);
    rig.store.pin(&space(), "x", rig.t()).unwrap();
    rig.finish_save();
    rig.fx();
    let revision = rig.get("x").0.revision;
    rig.ev();
    assert!(scratch_deletes(&rig.sweep(20_000, T + 8 * DAY)).is_empty());
    let view = rig.get("x").0;
    assert_eq!(
        (view.lifetime, view.revision),
        (Lifetime::Persistent, revision)
    );
    assert!(rig.ev().is_empty());
}

#[test]
fn a_sweep_skips_a_pin_in_flight() {
    let mut rig = Rig::new();
    rig.ssaved("x", "a", T);
    rig.store.pin(&space(), "x", rig.t()).unwrap();
    let (ticket, files) = workspace_saves(&rig.fx()).remove(0);
    assert!(scratch_deletes(&rig.sweep(20_000, T + SCRATCH_TTL_MS + 1)).is_empty());
    rig.store.apply_saved(ticket, done(&files), rig.t());
    assert_eq!(scratch_deletes(&rig.fx()).len(), 1);
    assert!(scratch_deletes(&rig.sweep(20_000 + EVERY, T + 2 * SCRATCH_TTL_MS)).is_empty());
}

#[test]
fn close_remove_and_pin_clear_the_stamp() {
    for how in ["close", "remove", "pin"] {
        let mut rig = Rig::new();
        rig.ssaved("x", "a", T);
        match how {
            "close" => {
                rig.store.close(&space(), "x", rig.t()).unwrap();
            }
            "remove" => {
                rig.store.remove(&space(), "x", rig.t()).unwrap();
            }
            _ => {
                rig.store.pin(&space(), "x", rig.t()).unwrap();
                rig.finish_save();
            }
        }
        rig.fx();
        assert!(
            scratch_deletes(&rig.sweep(20_000, T + 3 * SCRATCH_TTL_MS)).is_empty(),
            "{how}"
        );
    }
}

fn duplicate_rig() -> Rig {
    let timing = PanelTiming::default();
    let mut rig = Rig {
        store: PanelStore::new(timing),
        ids: Counter(1_000),
        now: 10_000,
        timing,
    };
    rig.store.start(rig.t());
    rig.store.add_space(&space(), rig.t());
    rig.store.apply_scratch_listing(
        Ok(ScratchListing {
            dirs: vec![sdir("x")],
            truncated: false,
        }),
        rig.t(),
    );
    rig.store.apply_listing(&space(), listing(&["x"]), rig.t());
    rig.fx();
    let now = rig.t();
    rig.store.apply_read(
        &PanelFolder::Scratch(sdir("x")),
        "scratch.json",
        found(&meta_json(T)),
        now,
        &mut rig.ids,
    );
    let fx = rig.fx();
    assert!(fx.iter().all(|effect| !matches!(
        effect,
        PanelEffect::Read {
            at: PanelFolder::Scratch(_),
            ..
        }
    )));
    assert_eq!(logs(&fx).len(), 1, "{fx:?}");
    assert!(logs(&fx)[0].contains("isn't loaded"));
    rig.pair("x", found(md_json()), found(b"workspace"));
    rig.fx();
    rig.ev();
    rig
}

#[test]
fn a_sweep_deletes_the_expired_duplicate_folder() {
    let mut rig = duplicate_rig();
    let fx = rig.sweep(20_000, T + SCRATCH_TTL_MS + 1);
    let gone = scratch_deletes(&fx);
    assert_eq!(gone.len(), 1);
    assert_eq!(gone[0].1, sdir("x"));
    rig.store.apply_deleted(gone[0].0, done(&[]), rig.t());
    assert!(scratch_deletes(&rig.sweep(20_000 + EVERY, T + SCRATCH_TTL_MS + 2)).is_empty());
}

#[test]
fn an_interrupted_pin_duplicate_never_removes_the_live_persistent_panel() {
    let mut rig = duplicate_rig();
    let before = rig.get("x").0;
    let fx = rig.sweep(20_000, T + SCRATCH_TTL_MS + 1);
    for (ticket, _) in scratch_deletes(&fx) {
        rig.store.apply_deleted(ticket, done(&[]), rig.t());
    }
    let after = rig.get("x").0;
    assert_eq!(
        (after.state, after.revision, after.lifetime),
        (before.state, before.revision, before.lifetime)
    );
    assert!(rig.ev().is_empty());
}

fn shadow_release(how: &str) {
    let (mut rig, all) = Rig::shadowed();
    no_workspace_read_of(&all, "x");
    let panels = rig.store.list(&space()).unwrap().panels;
    assert_eq!(panels.len(), 1);
    assert_eq!(panels[0].lifetime, Lifetime::Scratch);
    assert_eq!(skipped_of(&rig), open_skip("x"));
    let (view, source) = rig.get("x");
    assert_eq!((view.lifetime, source.as_str()), (Lifetime::Scratch, "s1"));
    assert!(has_listing(&rig.ev()));
    for n in 0..99 {
        rig.md(&format!("p{n:02}"), "x");
    }
    assert_eq!(
        rig.create("extra", Renderer::Markdown, "x").unwrap_err(),
        PanelApiError::PanelLimit {
            count: 100,
            limit: 100
        }
    );
    rig.fx();
    let scratch_rev = rig.get("x").0.revision;
    let fx = match how {
        "close" => {
            rig.store.close(&space(), "x", rig.t()).unwrap();
            rig.fx()
        }
        "remove" => {
            rig.store.remove(&space(), "x", rig.t()).unwrap();
            rig.fx()
        }
        _ => {
            let now = rig.now + EVERY;
            rig.sweep(now, T + SCRATCH_TTL_MS + 1)
        }
    };
    assert_eq!(scratch_deletes(&fx).len(), 1, "{how}");
    assert_eq!(reads(&fx), [("x".to_owned(), "panel.json")], "{how}");
    assert!(skipped_of(&rig).is_empty());
    rig.pair("x", found(md_json()), found(b"w1"));
    let (view, source) = rig.get("x");
    assert_eq!(view.lifetime, Lifetime::Persistent);
    assert_eq!(source, "w1");
    assert_eq!(view.state, PanelState::Validated);
    assert!(view.revision > scratch_rev);
    let mine = rig.store.list(&space()).unwrap().panels;
    assert_eq!(mine.iter().filter(|p| p.id == "x").count(), 1);
}

#[test]
fn a_workspace_folder_with_a_live_scratch_id_is_skipped_until_the_scratch_panel_goes() {
    for how in ["close", "remove", "sweep"] {
        shadow_release(how);
    }
}

#[test]
fn a_pin_into_a_skipped_folder_compares_both_files_and_rolls_back_without_a_second_record() {
    let (mut rig, _) = Rig::shadowed();
    rig.ev();
    rig.store.pin(&space(), "x", rig.t()).unwrap();
    let fx = rig.fx();
    let (ticket, files) = workspace_saves(&fx).remove(0);
    assert_eq!(files[0].file, "content.md");
    assert_eq!(files[0].expected, Some(FileMark::Missing));
    assert_eq!(files[1].file, "panel.json");
    assert_eq!(files[1].expected, Some(FileMark::Missing));
    assert!(skipped_of(&rig).is_empty());
    rig.store.apply_saved(
        ticket,
        SaveReport {
            written: Vec::new(),
            end: SaveEnd::Conflict { file: "content.md" },
        },
        rig.t(),
    );
    let fx = rig.fx();
    let view = rig.get("x").0;
    assert_eq!(view.lifetime, Lifetime::Scratch);
    assert_eq!(
        view.file_error.as_deref(),
        Some("Couldn't pin: .workspace/panels/x/content.md changed while it was written")
    );
    assert!(deletes(&fx).is_empty());
    no_workspace_read_of(&fx, "x");
    assert_eq!(skipped_of(&rig), open_skip("x"));
    assert_eq!(rig.store.list(&space()).unwrap().panels.len(), 1);
    rig.store.pin(&space(), "x", rig.t()).unwrap();
    let (ticket, files) = workspace_saves(&rig.fx()).remove(0);
    rig.ev();
    rig.store.apply_saved(ticket, done(&files), rig.t());
    assert!(changed(&rig.ev()).iter().any(|(panel, ..)| panel == "x"));
    assert_eq!(scratch_deletes(&rig.fx()).len(), 1);
    let view = rig.get("x").0;
    assert_eq!(view.lifetime, Lifetime::Persistent);
    assert_eq!(view.file_error, None);
    assert!(skipped_of(&rig).is_empty());
    assert_eq!(rig.store.list(&space()).unwrap().panels.len(), 1);
}

#[test]
fn a_successful_pin_clears_the_error_of_an_earlier_failed_pin() {
    let mut rig = Rig::new();
    rig.ssaved("x", "a", T);
    rig.store.pin(&space(), "x", rig.t()).unwrap();
    let (ticket, _) = workspace_saves(&rig.fx()).remove(0);
    rig.store
        .apply_saved(ticket, conflict_on_panel_json(), rig.t());
    rig.fx();
    assert!(rig.get("x").0.file_error.is_some());
    rig.store.pin(&space(), "x", rig.t()).unwrap();
    let (ticket, files) = workspace_saves(&rig.fx()).remove(0);
    rig.ev();
    rig.store.apply_saved(ticket, done(&files), rig.t());
    assert!(changed(&rig.ev()).iter().any(|(panel, ..)| panel == "x"));
    let view = rig.get("x").0;
    assert_eq!(view.lifetime, Lifetime::Persistent);
    assert_eq!(view.file_error, None);
}

#[test]
fn two_failed_pins_in_a_row_carry_one_couldnt_pin_prefix() {
    let mut rig = Rig::new();
    rig.screate("x", "a", T).unwrap();
    let (ticket, _) = saves(&rig.fx()).remove(0);
    fail_save(&mut rig, ticket, "content.md");
    rig.fx();
    for _ in 0..2 {
        rig.store.pin(&space(), "x", rig.t()).unwrap();
        let (ticket, _) = workspace_saves(&rig.fx()).remove(0);
        rig.store
            .apply_saved(ticket, conflict_on_panel_json(), rig.t());
        rig.fx();
    }
    let error = rig.get("x").0.file_error.unwrap();
    assert_eq!(error.matches("Couldn't pin: ").count(), 1, "{error}");
    assert_eq!(
        error.matches("Couldn't save the scratch panel").count(),
        1,
        "{error}"
    );
    assert!(error.ends_with("retrying."), "{error}");
}

#[test]
fn a_failed_pin_deletes_only_what_it_wrote() {
    let mut rig = Rig::new();
    rig.ssaved("x", "a", T);
    rig.store.pin(&space(), "x", rig.t()).unwrap();
    let (ticket, files) = workspace_saves(&rig.fx()).remove(0);
    let written = vec![("content.md", hash_of(&files[0].bytes))];
    rig.store.apply_saved(
        ticket,
        SaveReport {
            written,
            end: SaveEnd::Conflict { file: "panel.json" },
        },
        rig.t(),
    );
    let fx = rig.fx();
    let del = deletes(&fx);
    assert_eq!(del.len(), 1);
    assert_eq!(
        del[0].1,
        [("content.md", FileMark::Hash(hash_of(&files[0].bytes)))]
    );
    assert_eq!(lists(&fx), 1);
    assert_eq!(rig.get("x").0.lifetime, Lifetime::Scratch);
}

#[test]
fn a_read_deferred_by_a_pin_is_not_emitted_after_its_rollback() {
    let mut rig = Rig::new();
    rig.ssaved("x", "s1", T);
    rig.store.pin(&space(), "x", rig.t()).unwrap();
    let (ticket, _) = workspace_saves(&rig.fx()).remove(0);
    rig.store
        .apply_change(&space(), "panels/x/content.md", rig.t());
    rig.tick(rig.timing.debounce_ms);
    no_workspace_read_of(&rig.fx(), "x");
    rig.store.apply_saved(
        ticket,
        SaveReport {
            written: Vec::new(),
            end: SaveEnd::Conflict { file: "panel.json" },
        },
        rig.t(),
    );
    let fx = rig.fx();
    no_workspace_read_of(&fx, "x");
    assert_eq!(lists(&fx), 1);
    let (view, source) = rig.get("x");
    assert_eq!((view.lifetime, source.as_str()), (Lifetime::Scratch, "s1"));
    assert_eq!(skipped_of(&rig), open_skip("x"));
    assert_eq!(rig.store.list(&space()).unwrap().panels.len(), 1);
    rig.store.apply_listing(&space(), listing(&["x"]), rig.t());
    rig.tick(rig.timing.debounce_ms);
    no_workspace_read_of(&rig.fx(), "x");
}

#[test]
fn a_workspace_read_for_an_id_that_became_scratch_held_is_dropped() {
    let mut rig = Rig::new();
    rig.store.apply_listing(&space(), listing(&["x"]), rig.t());
    rig.fx();
    rig.store.apply_read(
        &at("x"),
        "panel.json",
        PanelRead::Missing,
        rig.t(),
        &mut rig.ids,
    );
    rig.store
        .apply_change(&space(), "panels/x/panel.json", rig.t());
    rig.tick(rig.timing.debounce_ms);
    assert_eq!(reads(&rig.fx()), [("x".to_owned(), "panel.json")]);
    rig.ssaved("x", "s1", T);
    rig.store.apply_read(
        &at("x"),
        "panel.json",
        found(md_json()),
        rig.t(),
        &mut rig.ids,
    );
    let fx = rig.fx();
    assert!(reads(&fx).is_empty(), "{fx:?}");
    let panels = rig.store.list(&space()).unwrap().panels;
    assert_eq!(panels.len(), 1);
    assert_eq!(panels[0].lifetime, Lifetime::Scratch);
    assert_eq!(rig.get("x").1, "s1");
    assert_eq!(skipped_of(&rig), open_skip("x"));
    assert!(has_listing(&rig.ev()));
    rig.store.close(&space(), "x", rig.t()).unwrap();
    let fx = rig.fx();
    assert_eq!(scratch_deletes(&fx).len(), 1);
    assert_eq!(reads(&fx), [("x".to_owned(), "panel.json")]);
    rig.store.apply_read(
        &at("x"),
        "panel.json",
        PanelRead::Missing,
        rig.t(),
        &mut rig.ids,
    );
    assert!(rig.create("x", Renderer::Markdown, "p").is_ok());
}

#[test]
fn a_sweep_done_never_clears_a_newer_stamp() {
    let mut rig = Rig::new();
    rig.ssaved("x", "a", T);
    let n1 = 20_000;
    let fx = rig.sweep(n1, T + SCRATCH_TTL_MS + 1);
    let (ticket, _) = scratch_deletes(&fx).remove(0);
    assert!(changed(&rig.ev())
        .iter()
        .any(|(_, _, state)| *state == PanelState::Removed));
    let w = T + SCRATCH_TTL_MS + 2;
    rig.screate("x", "b", w).unwrap();
    rig.finish_save();
    rig.fx();
    rig.store.apply_deleted(ticket, done(&[]), rig.t());
    assert!(scratch_deletes(&rig.sweep(n1 + EVERY, w + SCRATCH_TTL_MS - 1)).is_empty());
    let fx = rig.sweep(n1 + 2 * EVERY, w + SCRATCH_TTL_MS);
    assert_eq!(scratch_deletes(&fx).len(), 1);
}

#[test]
fn a_failed_scratch_listing_still_lists_and_creates_workspace_panels() {
    let timing = PanelTiming::default();
    let t0 = Millis(10_000);
    let gone = "the scratch panels couldn't be listed: gone".to_owned();
    let mut store = PanelStore::new(timing);
    let mut ids = Counter(1_000);
    store.start(t0);
    store.add_space(&space(), t0);
    store.apply_scratch_listing(Err(StoreError::Unavailable("gone".into())), t0);
    assert_eq!(logs(&store.take_effects()), std::slice::from_ref(&gone));
    assert!(store
        .next_deadline()
        .is_some_and(|d| d <= t0.saturating_add(Millis(timing.retry_ms))));
    store.apply_listing(&space(), listing(&["a"]), t0);
    store.take_effects();
    store.apply_read(&at("a"), "panel.json", found(md_json()), t0, &mut ids);
    store.apply_read(&at("a"), "content.md", found(b"A"), t0, &mut ids);
    let list = store.list(&space()).unwrap();
    assert_eq!(list.panels.len(), 1);
    assert_eq!(list.panels[0].state, PanelState::Validated);
    assert_eq!(
        list.error.as_deref(),
        Some("Couldn't list the scratch panels (gone)")
    );
    assert_eq!(store.get(&space(), "a").unwrap().1, "A");
    assert_eq!(
        store.get(&space(), "zz").unwrap_err(),
        PanelApiError::ScratchListingFailed {
            detail: "gone".to_owned()
        }
    );
    store
        .create(
            &space(),
            create_of("b", Renderer::Markdown, "B"),
            t0,
            0,
            &mut ids,
        )
        .unwrap();
    assert_eq!(saves(&store.take_effects()).len(), 1);
    assert_eq!(store.list(&space()).unwrap().panels.len(), 2);
    assert_eq!(
        store
            .create(&space(), scratch_of("c", "C"), t0, 0, &mut ids)
            .unwrap_err(),
        PanelApiError::ScratchListingFailed {
            detail: "gone".to_owned()
        }
    );
    assert!(store.take_effects().is_empty());
    let t1 = t0.saturating_add(Millis(timing.retry_ms));
    store.apply_scratch_listing(Err(StoreError::Unavailable("gone".into())), t1);
    assert!(logs(&store.take_effects()).is_empty());
    let t2 = t1.saturating_add(Millis(timing.retry_ms));
    store.tick(t2, 0);
    assert!(store
        .take_effects()
        .iter()
        .any(|effect| matches!(effect, PanelEffect::ListScratch)));
    store.apply_scratch_listing(
        Ok(ScratchListing {
            dirs: vec![sdir("d")],
            truncated: false,
        }),
        t2,
    );
    let fx = store.take_effects();
    assert!(fx.iter().any(|effect| matches!(
        effect,
        PanelEffect::Read {
            at: PanelFolder::Scratch(_),
            file: "scratch.json"
        }
    )));
    assert_eq!(
        store.get(&space(), "d").unwrap_err(),
        PanelApiError::SpaceLoading
    );
    assert_eq!(
        store
            .create(
                &space(),
                create_of("e", Renderer::Markdown, "E"),
                t2,
                0,
                &mut ids
            )
            .unwrap_err(),
        PanelApiError::SpaceLoading
    );
    assert_eq!(store.list(&space()).unwrap().error, None);
    let folder = PanelFolder::Scratch(sdir("d"));
    store.apply_read(&folder, "scratch.json", found(&meta_json(T)), t2, &mut ids);
    store.apply_read(&folder, "panel.json", found(md_json()), t2, &mut ids);
    store.apply_read(&folder, "content.md", found(b"D"), t2, &mut ids);
    let (view, source) = store.get(&space(), "d").unwrap();
    assert_eq!((view.lifetime, source), (Lifetime::Scratch, "D"));
    assert!(store
        .create(&space(), scratch_of("c", "C"), t2, 0, &mut ids)
        .is_ok());
}

#[test]
fn bad_scratch_folders_at_load_are_skipped_logged_once_and_never_deleted() {
    let timing = PanelTiming::default();
    let mut store = started_store(PanelTiming::default());
    let mut ids = Counter(1_000);
    let now = Millis(10_000);
    let k = space().sha256_hex();
    let s = space().0;
    store.add_space(&space(), now);
    store.apply_listing(&space(), Ok(None), now);
    store.take_effects();
    let names = ["a", "b", "c", "d", "e", "f", "g"];
    store.apply_scratch_listing(
        Ok(ScratchListing {
            dirs: names.iter().map(|n| sdir(n)).collect(),
            truncated: false,
        }),
        now,
    );
    let mut all = store.take_effects();
    let meta = found(&meta_json(T));
    let json = |read: PanelRead, id: &str, store: &mut PanelStore, ids: &mut Counter| {
        store.apply_read(
            &PanelFolder::Scratch(sdir(id)),
            "scratch.json",
            read,
            now,
            ids,
        );
    };
    for id in ["a", "b", "c", "d"] {
        json(meta.clone(), id, &mut store, &mut ids);
    }
    json(found(b"{"), "e", &mut store, &mut ids);
    json(
        PanelRead::Failed("denied".into()),
        "f",
        &mut store,
        &mut ids,
    );
    json(PanelRead::Missing, "g", &mut store, &mut ids);
    all.extend(store.take_effects());
    let panel = |store: &mut PanelStore, ids: &mut Counter, id: &str, file: &str, read| {
        store.apply_read(&PanelFolder::Scratch(sdir(id)), file, read, now, ids);
    };
    panel(&mut store, &mut ids, "a", "panel.json", found(b"{"));
    panel(&mut store, &mut ids, "b", "panel.json", PanelRead::Missing);
    panel(
        &mut store,
        &mut ids,
        "c",
        "panel.json",
        found(br#"{"version":1,"title":"t","slot":"s","argv":["sh"]}"#),
    );
    panel(&mut store, &mut ids, "d", "panel.json", found(md_json()));
    panel(
        &mut store,
        &mut ids,
        "d",
        "content.md",
        PanelRead::Failed("denied".into()),
    );
    all.extend(store.take_effects());
    let texts = logs(&all);
    assert_eq!(texts.len(), 7, "{texts:?}");
    let has = |text: String| assert!(texts.contains(&text), "{text} in {texts:?}");
    let starts = |prefix: String| {
        assert!(texts.iter().any(|t| t.starts_with(&prefix)), "{prefix}");
    };
    starts(format!(
        "the scratch folder {k}/e is skipped: scratch.json isn't valid JSON: "
    ));
    has(format!(
        "the scratch folder {k}/f is skipped: couldn't read scratch.json (denied)"
    ));
    has(format!(
        "the scratch folder {k}/g is skipped: scratch.json is missing"
    ));
    starts(format!(
        "the scratch panel a of {s} is skipped: panel.json isn't valid JSON: "
    ));
    has(format!(
        "the scratch panel b of {s} is skipped: no panel.json"
    ));
    has(format!(
        "the scratch panel c of {s} is skipped: not a view panel"
    ));
    has(format!(
        "the scratch panel d of {s} is skipped: couldn't read content.md (denied)"
    ));
    store
        .create(
            &space(),
            scratch_of("n", "N"),
            now,
            T + SCRATCH_TTL_MS,
            &mut ids,
        )
        .unwrap();
    store
        .create(
            &space(),
            create_of("p", Renderer::Markdown, "P"),
            now,
            0,
            &mut ids,
        )
        .unwrap();
    let list = store.list(&space()).unwrap();
    let got: Vec<&str> = list.panels.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(got, ["n", "p"]);
    store.take_effects();
    for n in 1..=2 {
        store.tick(
            Millis(10_000 + n * timing.sweep_every_ms),
            T + SCRATCH_TTL_MS + 1,
        );
        assert!(scratch_deletes(&store.take_effects()).is_empty());
    }
}

#[test]
fn close_and_recreate_never_start_a_second_check_while_one_runs() {
    let mut rig = Rig::new();
    let mut create = scratch_of("x", "g1");
    create.renderer = Renderer::Mermaid;
    let first = rig
        .store
        .create(&space(), create.clone(), rig.t(), T, &mut rig.ids)
        .unwrap();
    assert_eq!(checks(&rig.fx()).len(), 1);
    rig.store.close(&space(), "x", rig.t()).unwrap();
    let second = rig
        .store
        .create(&space(), create, rig.t(), T, &mut rig.ids)
        .unwrap();
    assert!(checks(&rig.fx()).is_empty());
    rig.verdict("x", first.revision, Ok(DiagramVerdict::Validated));
    let started = checks(&rig.fx());
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].1, second.revision);
}

#[test]
fn key_is_free_after_close_remove_or_expiry() {
    for how in ["close", "remove", "expiry"] {
        let mut rig = Rig::new();
        let mut create = scratch_of("x", "a");
        create.idempotency_key = Some("k".to_owned());
        rig.store
            .create(&space(), create.clone(), rig.t(), T, &mut rig.ids)
            .unwrap();
        rig.finish_save();
        rig.fx();
        let same = rig
            .store
            .create(&space(), create.clone(), rig.t(), T, &mut rig.ids)
            .unwrap();
        assert_eq!(same.id, "x");
        match how {
            "close" => {
                rig.store.close(&space(), "x", rig.t()).unwrap();
            }
            "remove" => {
                rig.store.remove(&space(), "x", rig.t()).unwrap();
            }
            _ => {
                rig.sweep(20_000, T + SCRATCH_TTL_MS);
            }
        }
        create.id = Some("y".to_owned());
        let fresh = rig
            .store
            .create(&space(), create, rig.t(), T + SCRATCH_TTL_MS, &mut rig.ids)
            .unwrap();
        assert_eq!(fresh.id, "y", "{how}");
    }
}

#[test]
fn templates_and_scratch_panels_count() {
    let mut rig = Rig::new();
    let names: Vec<String> = (0..60).map(|n| format!("t{n:02}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    rig.store.apply_listing(&space(), listing(&refs), rig.t());
    rig.fx();
    for name in &refs {
        rig.store.apply_read(
            &at(name),
            "panel.json",
            found(br#"{"version":1}"#),
            rig.t(),
            &mut rig.ids,
        );
    }
    for n in 0..40 {
        rig.screate(&format!("s{n:02}"), "x", T).unwrap();
    }
    assert_eq!(
        rig.screate("extra", "x", T).unwrap_err(),
        PanelApiError::PanelLimit {
            count: 100,
            limit: 100
        }
    );
}

#[test]
fn the_100_folder_reason_is_true_at_exactly_100_panels() {
    let mut rig = Rig::new();
    rig.ssaved("s", "x", T);
    fill(&mut rig, 99);
    rig.outside("panels/n/panel.json");
    assert_eq!(
        skipped_of(&rig),
        [(
            "n".to_owned(),
            "the space already has 100 panels; this folder isn't read".to_owned()
        )]
    );
}

#[test]
fn a_remove_that_conflicts_while_a_check_waits_queues_the_check_again() {
    let mut rig = Rig::new();
    rig.mm("a", "g");
    let fx = rig.fx();
    assert_eq!(checks(&fx).len(), 1);
    let (ticket, files) = saves(&fx).remove(0);
    rig.store.apply_saved(ticket, done(&files), rig.t());
    let revision = rig.get("a").0.revision;
    rig.verdict("a", revision, unavailable());
    assert!(checks(&rig.fx()).is_empty());
    rig.store.remove(&space(), "a", rig.t()).unwrap();
    let (ticket, _) = deletes(&rig.fx()).remove(0);
    rig.store.apply_deleted(
        ticket,
        SaveReport {
            written: Vec::new(),
            end: SaveEnd::Conflict { file: "panel.json" },
        },
        rig.t(),
    );
    let mut started = checks(&rig.fx());
    let bytes = |name: &str| {
        files
            .iter()
            .find(|f| f.file == name)
            .map(|f| f.bytes.clone())
            .unwrap_or_default()
    };
    rig.pair(
        "a",
        found(&bytes("panel.json")),
        found(&bytes("diagram.mmd")),
    );
    rig.tick(40_000);
    started.extend(checks(&rig.fx()));
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(started[0].0, "a");
}

fn conflict_on_panel_json() -> SaveReport {
    SaveReport {
        written: Vec::new(),
        end: SaveEnd::Conflict { file: "panel.json" },
    }
}

fn file_names(files: &[FileWrite]) -> Vec<&'static str> {
    files.iter().map(|write| write.file).collect()
}

#[test]
fn a_failed_pin_after_a_failed_scratch_save_saves_the_scratch_folder_again() {
    let mut rig = Rig::new();
    rig.screate("x", "a", T).unwrap();
    let (ticket, _) = saves(&rig.fx()).remove(0);
    fail_save(&mut rig, ticket, "content.md");
    rig.fx();
    rig.store.pin(&space(), "x", rig.t()).unwrap();
    let (ticket, _) = workspace_saves(&rig.fx()).remove(0);
    rig.store
        .apply_saved(ticket, conflict_on_panel_json(), rig.t());
    let fx = rig.fx();
    assert!(scratch_saves(&fx).is_empty(), "{fx:?}");
    let view = rig.get("x").0;
    assert_eq!(view.lifetime, Lifetime::Scratch);
    let error = view.file_error.unwrap();
    assert!(error.starts_with("Couldn't pin: "), "{error}");
    assert!(
        error.contains("Couldn't save the scratch panel x (") && error.ends_with("retrying."),
        "{error}"
    );
    rig.tick(rig.timing.retry_ms - 1);
    assert!(scratch_saves(&rig.fx()).is_empty());
    rig.tick(2);
    let fx = rig.fx();
    let mut all = scratch_saves(&fx);
    assert_eq!(all.len(), 1, "{fx:?}");
    let (ticket, _, files) = all.remove(0);
    assert_eq!(
        file_names(&files),
        ["scratch.json", "content.md", "panel.json"]
    );
    rig.store.apply_saved(ticket, done(&files), rig.t());
    assert_eq!(rig.get("x").0.file_error, None);
}

#[test]
fn a_failed_pin_while_a_scratch_save_is_in_flight_saves_the_scratch_folder_again() {
    let mut rig = Rig::new();
    rig.screate("x", "a", T).unwrap();
    let (scratch_ticket, _) = saves(&rig.fx()).remove(0);
    rig.store.pin(&space(), "x", rig.t()).unwrap();
    let (pin_ticket, _) = workspace_saves(&rig.fx()).remove(0);
    fail_save(&mut rig, scratch_ticket, "content.md");
    rig.store
        .apply_saved(pin_ticket, conflict_on_panel_json(), rig.t());
    let fx = rig.fx();
    let mut all = scratch_saves(&fx);
    assert_eq!(all.len(), 1, "{fx:?}");
    let (_, _, files) = all.remove(0);
    assert_eq!(
        file_names(&files),
        ["scratch.json", "content.md", "panel.json"]
    );
}

#[test]
fn an_update_during_a_pin_restamps_the_scratch_folder_after_a_rollback() {
    let mut rig = Rig::new();
    rig.ssaved("x", "a", T);
    rig.store.pin(&space(), "x", rig.t()).unwrap();
    let (ticket, _) = workspace_saves(&rig.fx()).remove(0);
    rig.supdate("x", content_update("b"), T + 500).unwrap();
    rig.fx();
    rig.store
        .apply_saved(ticket, conflict_on_panel_json(), rig.t());
    let fx = rig.fx();
    let mut all = scratch_saves(&fx);
    assert_eq!(all.len(), 1, "{fx:?}");
    let (_, _, files) = all.remove(0);
    assert_eq!(files[0].file, "scratch.json");
    assert_eq!(files[0].bytes, meta_json(T + 500));
}

#[test]
fn the_scratch_listing_error_emits_listing_when_it_starts_changes_and_clears() {
    let mut rig = Rig::new();
    rig.ev();
    let now = rig.t();
    rig.store
        .apply_scratch_listing(Err(StoreError::Unavailable("gone".into())), now);
    assert!(has_listing(&rig.ev()));
    rig.store
        .apply_scratch_listing(Err(StoreError::Unavailable("gone".into())), now);
    assert!(!has_listing(&rig.ev()));
    rig.store
        .apply_scratch_listing(Err(StoreError::Unavailable("still".into())), now);
    assert!(has_listing(&rig.ev()));
    rig.store.apply_scratch_listing(
        Ok(ScratchListing {
            dirs: Vec::new(),
            truncated: false,
        }),
        now,
    );
    assert!(has_listing(&rig.ev()));
}

const HUNDRED: &str = "the space already has 100 panels; this folder isn't read";

fn skip_of(rig: &Rig, id: &str) -> Option<String> {
    skipped_of(rig)
        .into_iter()
        .find(|(panel, _)| panel == id)
        .map(|(_, reason)| reason)
}

fn scratch_ids(count: usize) -> Vec<String> {
    (0..count).map(|n| format!("s{n:03}")).collect()
}

#[test]
fn a_listing_with_100_scratch_panels_admits_no_outside_folder() {
    let ids = scratch_ids(100);
    let items: Vec<(&str, u64)> = ids.iter().map(|id| (id.as_str(), T)).collect();
    let mut rig = Rig::from_scratch(&items);
    assert_eq!(rig.store.list(&space()).unwrap().panels.len(), 100);

    rig.store
        .apply_listing(&space(), listing(&["outside"]), rig.t());
    assert!(reads(&rig.fx()).is_empty());
    assert_eq!(rig.store.list(&space()).unwrap().panels.len(), 100);
    assert_eq!(skip_of(&rig, "outside").as_deref(), Some(HUNDRED));
}

#[test]
fn a_folder_event_with_100_scratch_panels_admits_nothing() {
    let ids = scratch_ids(100);
    let items: Vec<(&str, u64)> = ids.iter().map(|id| (id.as_str(), T)).collect();
    let mut rig = Rig::from_scratch(&items);

    rig.store
        .apply_change(&space(), "panels/outside/panel.json", rig.t());
    rig.tick(rig.timing.debounce_ms);
    assert!(reads(&rig.fx()).is_empty());
    assert_eq!(rig.store.list(&space()).unwrap().panels.len(), 100);
    assert_eq!(skip_of(&rig, "outside").as_deref(), Some(HUNDRED));
}

#[test]
fn a_restart_with_mixed_lifetimes_loads_exactly_100_and_keeps_the_rest_on_disk() {
    let mut rig = Rig::new();
    let mut store = started_store(rig.timing);
    store.add_space(&space(), rig.t());
    let scratch: Vec<String> = scratch_ids(60);
    let workspace: Vec<String> = (0..60).map(|n| format!("w{n:03}")).collect();
    store.apply_scratch_listing(
        Ok(ScratchListing {
            dirs: scratch.iter().map(|id| sdir(id)).collect(),
            truncated: false,
        }),
        rig.t(),
    );
    for id in &scratch {
        store.apply_read(
            &PanelFolder::Scratch(sdir(id)),
            "scratch.json",
            found(&meta_json(T)),
            rig.t(),
            &mut rig.ids,
        );
    }
    let names: Vec<&str> = workspace.iter().map(String::as_str).collect();
    store.apply_listing(&space(), listing(&names), rig.t());
    store.take_effects();
    for id in &workspace {
        store.apply_read(
            &at(id),
            "panel.json",
            found(md_json()),
            rig.t(),
            &mut rig.ids,
        );
        store.apply_read(
            &at(id),
            "content.md",
            found(id.as_bytes()),
            rig.t(),
            &mut rig.ids,
        );
    }
    for id in &scratch {
        let folder = PanelFolder::Scratch(sdir(id));
        store.apply_read(
            &folder,
            "panel.json",
            found(md_json()),
            rig.t(),
            &mut rig.ids,
        );
        store.apply_read(
            &folder,
            "content.md",
            found(id.as_bytes()),
            rig.t(),
            &mut rig.ids,
        );
    }
    let effects = store.take_effects();
    let panels = store.list(&space()).unwrap().panels;
    assert_eq!(panels.len(), 100);
    let loaded_scratch = panels
        .iter()
        .filter(|view| view.lifetime == Lifetime::Scratch)
        .count();
    assert_eq!(loaded_scratch, 40);
    assert!(panels.iter().any(|view| view.id == "s039"));
    assert!(panels.iter().all(|view| view.id != "s040"));
    assert!(scratch_deletes(&effects).is_empty(), "{effects:?}");
}

#[test]
fn queued_first_sight_reads_completing_together_never_exceed_100() {
    let names: Vec<String> = (0..99).map(|n| format!("p{n:03}")).collect();
    let loaded: Vec<(&str, &str)> = names.iter().map(|id| (id.as_str(), "x")).collect();
    let mut rig = Rig::with_folders(&loaded);
    let fresh = ["n1", "n2", "n3", "n4", "n5"];
    for id in fresh {
        rig.store
            .apply_change(&space(), &format!("panels/{id}/panel.json"), rig.t());
    }
    rig.tick(rig.timing.debounce_ms);
    let asked = reads(&rig.fx());
    assert_eq!(asked.len(), 1, "{asked:?}");
    for (id, _) in &asked {
        rig.pair(id, found(md_json()), found(b"y"));
    }
    assert_eq!(rig.store.list(&space()).unwrap().panels.len(), 100);
    for id in fresh.iter().filter(|id| **id != asked[0].0) {
        assert_eq!(skip_of(&rig, id).as_deref(), Some(HUNDRED), "{id}");
    }
}

/// How early the saved scratch panel's `scratch.json` arrives in a restart.
#[derive(Clone, Copy)]
enum MetaAt {
    BeforeListing,
    WhileReadsAreOut,
}

/// A restart: `valid` workspace panels, `empty` folders with no `panel.json`, and one saved
/// scratch panel `saved`. Every workspace read is answered after the scratch folder is known.
fn restart_with_one_scratch(valid: usize, empty: usize, meta: MetaAt) -> Rig {
    let timing = PanelTiming::default();
    let mut rig = Rig {
        store: PanelStore::new(timing),
        ids: Counter(1_000),
        now: 10_000,
        timing,
    };
    rig.store.start(rig.t());
    rig.store.add_space(&space(), rig.t());
    rig.store.apply_scratch_listing(
        Ok(ScratchListing {
            dirs: vec![sdir("saved")],
            truncated: false,
        }),
        rig.t(),
    );
    let folder = PanelFolder::Scratch(sdir("saved"));
    let meta_read = |rig: &mut Rig| {
        let now = rig.t();
        rig.store.apply_read(
            &folder,
            "scratch.json",
            found(&meta_json(T)),
            now,
            &mut rig.ids,
        );
    };
    if matches!(meta, MetaAt::BeforeListing) {
        meta_read(&mut rig);
    }
    let valid_ids: Vec<String> = (0..valid).map(|n| format!("p{n:03}")).collect();
    let empty_ids: Vec<String> = (0..empty).map(|n| format!("q{n:03}")).collect();
    let names: Vec<&str> = valid_ids
        .iter()
        .chain(&empty_ids)
        .map(String::as_str)
        .collect();
    rig.store.apply_listing(&space(), listing(&names), rig.t());
    if matches!(meta, MetaAt::WhileReadsAreOut) {
        meta_read(&mut rig);
    }
    assert_eq!(reads(&rig.fx()).len(), valid + empty);
    for id in &valid_ids {
        rig.pair(id, found(md_json()), found(id.as_bytes()));
    }
    for id in &empty_ids {
        rig.store.apply_read(
            &at(id),
            "panel.json",
            PanelRead::Missing,
            rig.t(),
            &mut rig.ids,
        );
    }
    let now = rig.t();
    rig.store
        .apply_read(&folder, "panel.json", found(md_json()), now, &mut rig.ids);
    rig.store
        .apply_read(&folder, "content.md", found(b"saved"), now, &mut rig.ids);
    rig
}

fn assert_saved_loaded(rig: &Rig, panels: usize) {
    assert_eq!(rig.store.list(&space()).unwrap().panels.len(), panels);
    assert_eq!(rig.get("saved").1, "saved");
}

#[test]
fn a_saved_scratch_panel_loads_when_an_empty_folder_leaves_the_100th_slot() {
    let mut rig = restart_with_one_scratch(99, 1, MetaAt::BeforeListing);
    assert_saved_loaded(&rig, 100);
    assert!(matches!(
        rig.create("late", Renderer::Markdown, "x"),
        Err(PanelApiError::PanelLimit { .. })
    ));
}

#[test]
fn a_saved_scratch_panel_loads_beside_100_empty_folders() {
    let mut rig = restart_with_one_scratch(0, 100, MetaAt::BeforeListing);
    assert_saved_loaded(&rig, 1);
    rig.md("late", "x");
}

#[test]
fn a_saved_scratch_panel_whose_meta_arrives_while_reads_are_out_still_loads() {
    let mut rig = restart_with_one_scratch(99, 1, MetaAt::WhileReadsAreOut);
    assert_saved_loaded(&rig, 100);
    assert!(matches!(
        rig.create("late", Renderer::Markdown, "x"),
        Err(PanelApiError::PanelLimit { .. })
    ));
}
