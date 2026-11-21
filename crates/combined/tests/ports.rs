//! The ports and model: neutral, object safe, and free of I/O.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use combine_protocol::{AttachRequestOptions, TermId};
use combined::model::{
    Millis, RootKind, Space, SpaceId, SpawnSpec, TermEvent, TermInfo, TermState,
};
use combined::ports::{
    AttachOptions, Attachment, BackendError, FileStore, GitEntry, GitProbe, StoreError,
    StoreRevision, StoredFile, TerminalBackend,
};
use common::{display, request_options, spawn_spec, FakeGitProbe};

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Every source file under `src/{core,ports,model}`.
fn layer_sources() -> Vec<PathBuf> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    for layer in ["core", "ports", "model"] {
        sources(&src.join(layer), &mut files);
    }
    assert!(!files.is_empty());
    files
}

fn strip_comments(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("//") {
            rest = after.find('\n').map_or("", |end| &after[end..]);
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.find("*/").map_or("", |end| &after[end + 2..]);
        } else {
            let mut chars = rest.chars();
            out.push(chars.next().unwrap());
            rest = chars.as_str();
        }
    }
    out
}

#[test]
fn ports_model_and_core_name_no_backend() {
    const FRAME_KIND_SPELLINGS: [&str; 7] = [
        "DATA",
        "CREDIT",
        "CLOSE",
        "RESIZE",
        "FOCUS",
        "FACTS",
        "AUTOMATIC_INPUT",
    ];
    for file in layer_sources() {
        let text = fs::read_to_string(&file).unwrap();
        assert!(!text.to_lowercase().contains("herdr"), "{}", file.display());
        for token in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            assert!(
                !token.eq_ignore_ascii_case("hc"),
                "{} names the backend CLI",
                file.display()
            );
            assert!(
                !FRAME_KIND_SPELLINGS.contains(&token),
                "{} uses {token}",
                file.display()
            );
        }
    }
}

#[test]
fn core_ports_and_model_do_no_io_or_read_a_clock() {
    const BANNED: [&str; 8] = [
        "tokio",
        "std::thread",
        "std::env",
        "SystemTime",
        "Instant",
        "std::fs",
        "std::net",
        "std::process",
    ];
    for file in layer_sources() {
        let code = strip_comments(&fs::read_to_string(&file).unwrap());
        for banned in BANNED {
            assert!(!code.contains(banned), "{} uses {banned}", file.display());
        }
    }
}

#[test]
fn traits_are_object_safe() {
    fn _backend(_: &dyn TerminalBackend) {}
    fn _attachment(_: &dyn Attachment) {}
    fn _git(_: &dyn GitProbe) {}
    fn _store(_: &dyn FileStore) {}
}

#[test]
fn attach_options_come_from_the_wire_options() {
    let wire: AttachRequestOptions = request_options(120, 40, true);
    let options = AttachOptions::from(wire.clone());
    assert_eq!(options.cols, 120);
    assert_eq!(options.rows, 40);
    assert_eq!(options.display, display());
    assert!(options.observe);
    assert!(!options.resumed);
    assert_eq!(options.history, wire.history);
}

#[test]
fn backend_errors_read_as_documented() {
    let id = TermId("fake:7".to_string());
    assert_eq!(
        BackendError::UnknownTerminal(id.clone()).to_string(),
        "unknown terminal fake:7"
    );
    assert_eq!(
        BackendError::RemoteTerminal(id).to_string(),
        "fake:7 is another machine's terminal"
    );
    assert_eq!(
        BackendError::InvalidRequest("x".into()).to_string(),
        "invalid request: x"
    );
    assert_eq!(
        BackendError::Closed.to_string(),
        "the attachment has closed"
    );
    assert_eq!(
        BackendError::Failed("y".into()).to_string(),
        "backend failure: y"
    );
    let _: &dyn std::error::Error = &BackendError::Closed;
}

#[test]
fn model_types_construct() {
    let info = TermInfo {
        id: TermId("fake:1".to_string()),
        cwd: "/work".to_string(),
        argv: vec!["sh".to_string()],
        state: TermState::Running,
        labels: Default::default(),
        cols: 80,
        rows: 24,
    };
    let events = [
        TermEvent::Created(info.clone()),
        TermEvent::Changed(info.clone()),
        TermEvent::Exited {
            id: info.id.clone(),
            exit_code: Some(0),
        },
        TermEvent::Lost {
            id: info.id.clone(),
        },
        TermEvent::Restarted {
            id: info.id.clone(),
        },
    ];
    assert_eq!(events.len(), 5);
    assert_ne!(TermState::Running, TermState::Exited { exit_code: None });

    let spec: SpawnSpec = spawn_spec("/work", 80, 24);
    assert_eq!((spec.cols, spec.rows), (80, 24));

    let space = Space {
        id: SpaceId("/work".to_string()),
        kind: RootKind::Repository,
        terminals: vec![info.id],
    };
    assert_eq!(space.kind, RootKind::Repository);
    assert!(SpaceId("/a".into()) < SpaceId("/b".into()));

    assert_eq!(Millis(u64::MAX).saturating_add(Millis(1)), Millis(u64::MAX));
    assert_eq!(Millis(2).saturating_add(Millis(3)), Millis(5));
    assert_eq!(Millis::default(), Millis(0));

    let file = StoredFile {
        bytes: vec![1],
        revision: StoreRevision(0),
    };
    assert!(file.revision < StoreRevision(1));
    assert_ne!(StoreError::InvalidPath, StoreError::Unavailable("x".into()));
}

#[test]
fn fake_git_probe_answers_three_ways_by_exact_string() {
    let probe = FakeGitProbe::default()
        .with_repository("/work/repo")
        .with_worktree("/work/tree");
    assert_eq!(probe.git_entry("/work/repo"), GitEntry::Directory);
    assert_eq!(probe.git_entry("/work/tree"), GitEntry::File);
    assert_eq!(probe.git_entry("/work/plain"), GitEntry::Absent);
    assert_eq!(probe.git_entry("/work/repo/"), GitEntry::Absent);
    assert_eq!(probe.git_entry("/work"), GitEntry::Absent);
}
