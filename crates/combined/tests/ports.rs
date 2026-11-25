//! The ports and model: neutral, object safe, and free of I/O.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use combine_protocol::{AttachRequestOptions, TermId};
use combined::model::{
    ContentHash, Millis, RootKind, Space, SpaceId, SpawnSpec, TermEvent, TermInfo, TermState,
};
use combined::ports::{
    AttachOptions, Attachment, BackendError, CallerIdentity, DiagramCheck, FileMark, FileStore,
    GitEntry, GitProbe, ScratchStore, StoreError, StoreRevision, StoredFile, TerminalBackend,
    WallClock,
};
use common::{display, request_options, spawn_spec, FakeGitProbe};
use sha2::{Digest, Sha256};

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

/// Removes `//` and `/* */` comments, leaving string, raw string and char literals untouched so
/// that a `//` inside one doesn't hide code after it.
fn strip_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &text[i..];
        let skip = if rest.starts_with("//") {
            rest.find('\n').unwrap_or(rest.len())
        } else if rest.starts_with("/*") {
            let mut depth = 0;
            let mut j = 0;
            while j < rest.len() {
                if rest[j..].starts_with("/*") {
                    depth += 1;
                    j += 2;
                } else if rest[j..].starts_with("*/") {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            j
        } else {
            let literal = literal_len(rest);
            if literal > 0 {
                out.push_str(&rest[..literal]);
                i += literal;
                continue;
            }
            let ch = rest.chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        };
        i += skip;
    }
    out
}

/// The length of the string, raw string or char literal at the start of `rest`, or 0.
fn literal_len(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let (prefix, raw) = match bytes[0] {
        b'r' => (1, true),
        b'b' if bytes.get(1) == Some(&b'r') => (2, true),
        b'b' => (1, false),
        _ => (0, false),
    };
    if raw {
        let hashes = bytes[prefix..].iter().take_while(|&&b| b == b'#').count();
        if bytes.get(prefix + hashes) != Some(&b'"') {
            return 0;
        }
        let close = format!("\"{}", "#".repeat(hashes));
        let body = prefix + hashes + 1;
        return rest[body..]
            .find(&close)
            .map_or(rest.len(), |end| body + end + close.len());
    }
    let quote = match bytes.get(prefix) {
        Some(&q @ (b'"' | b'\'')) => q,
        _ => return 0,
    };
    if prefix == 1 && bytes[0] == b'r' {
        return 0;
    }
    let mut j = prefix + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b if b == quote => return j + 1,
            b'\n' if quote == b'\'' => return 0,
            _ => j += 1,
        }
    }
    0
}

#[test]
fn strip_comments_keeps_literals_and_drops_comments() {
    let source = "let a = \"//\"; std::fs::read(); // tail\n/* block */ let b = r#\"/*\"#; std::env::var(\"x\");\nlet c = '\"'; /* a /* nested */ still */ done";
    let code = strip_comments(source);
    assert!(code.contains("std::fs::read()"));
    assert!(code.contains("std::env::var"));
    assert!(code.contains("\"//\""));
    assert!(code.contains("r#\"/*\"#"));
    assert!(code.contains("let c = '\"';"));
    assert!(code.contains("done"));
    assert!(!code.contains("tail"));
    assert!(!code.contains("block"));
    assert!(!code.contains("nested"));
    assert!(!code.contains("still"));
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

/// Production code in the layers never panics on a `None` or an `Err`. The scan reads whole
/// lines and skips only those that start with `//`, so no comment stripper (which can misread a
/// lifetime tick) can hide a call: a spelling in a trailing comment fails the test too.
#[test]
fn core_ports_and_model_have_no_unwrap_or_expect() {
    for file in layer_sources() {
        let text = fs::read_to_string(&file).unwrap();
        assert!(
            !text.contains("cfg(test)"),
            "{} has inline test code; this scan assumes none",
            file.display()
        );
        for (number, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for banned in [".unwrap()", ".expect("] {
                assert!(
                    !line.contains(banned),
                    "{}:{} uses {banned}",
                    file.display(),
                    number + 1
                );
            }
        }
    }
}

#[test]
fn traits_are_object_safe() {
    fn _backend(_: &dyn TerminalBackend) {}
    fn _attachment(_: &dyn Attachment) {}
    fn _git(_: &dyn GitProbe) {}
    fn _callers(_: &dyn CallerIdentity) {}
    fn _store(_: &dyn FileStore) {}
    fn _scratch(_: &dyn ScratchStore) {}
    fn _clock(_: &dyn WallClock) {}
    fn _diagram(_: &dyn DiagramCheck) {}
}

fn read_of(bytes: &[u8], read_only: bool) -> Result<Option<StoredFile>, StoreError> {
    Ok(Some(StoredFile {
        bytes: bytes.to_vec(),
        revision: StoreRevision(1),
        hash: ContentHash(Sha256::digest(bytes).into()),
        read_only,
    }))
}

#[test]
fn file_mark_matches_and_matches_project() {
    let hash = ContentHash(Sha256::digest(b"x").into());
    let other = ContentHash(Sha256::digest(b"y").into());
    let mark = FileMark::Hash(hash);
    assert_eq!(FileMark::from(Some(hash)), mark);
    assert_eq!(FileMark::from(None), FileMark::Missing);

    assert!(mark.matches(&read_of(b"x", false)));
    assert!(mark.matches_project(&read_of(b"x", false)));
    assert!(mark.matches(&read_of(b"x", true)));
    assert!(!mark.matches_project(&read_of(b"x", true)));
    assert!(!mark.matches(&read_of(b"y", false)));
    assert!(!FileMark::Hash(other).matches(&read_of(b"x", false)));
    assert!(!mark.matches(&Ok(None)));

    assert!(FileMark::Missing.matches(&Ok(None)));
    assert!(FileMark::Missing.matches_project(&Ok(None)));
    assert!(!FileMark::Missing.matches(&read_of(b"x", false)));

    let large = |read_only| Err(StoreError::TooLarge { size: 9, read_only });
    assert!(FileMark::TooLarge.matches(&large(false)));
    assert!(FileMark::TooLarge.matches_project(&large(false)));
    assert!(FileMark::TooLarge.matches(&large(true)));
    assert!(!FileMark::TooLarge.matches_project(&large(true)));
    assert!(!mark.matches(&large(false)));

    for error in [
        StoreError::InvalidPath,
        StoreError::Unavailable("x".into()),
        StoreError::ReadOnly("x".into()),
    ] {
        for mark in [FileMark::Missing, mark, FileMark::TooLarge] {
            assert!(!mark.matches(&Err(error.clone())), "{mark:?} {error:?}");
            assert!(!mark.matches_project(&Err(error.clone())));
        }
    }
}

#[test]
fn space_sha256_hex_matches_the_fallback_folder() {
    for id in ["/p", "/Users/me/work tree", ""] {
        let digest = Sha256::digest(id.as_bytes());
        let expected: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        let hex = SpaceId(id.to_owned()).sha256_hex();
        assert_eq!(hex, expected);
        assert_eq!(hex.len(), 64);
    }
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
        hash: ContentHash([0; 32]),
        read_only: false,
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
