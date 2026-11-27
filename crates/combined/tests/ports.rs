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
/// that a `//` inside one doesn't hide code after it. A lifetime or label tick (`'a`,
/// `&'static str`, `'outer:`) is code, not the start of a char literal.
fn strip_comments(text: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    let mut after_ident = false;
    while i < text.len() {
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
            let literal = literal_len(rest, after_ident);
            if literal > 0 {
                out.push_str(&rest[..literal]);
                i += literal;
                after_ident = false;
                continue;
            }
            let ch = rest.chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            after_ident = ch.is_alphanumeric() || ch == '_';
            continue;
        };
        i += skip;
        after_ident = false;
    }
    out
}

/// The length of the string, raw string, char or byte literal at the start of `rest`, or 0 when
/// none starts there. `after_ident` says an identifier character came just before, so an `r` or
/// `b` there ends that identifier instead of prefixing a literal.
fn literal_len(rest: &str, after_ident: bool) -> usize {
    let bytes = rest.as_bytes();
    let (prefix, raw) = match bytes[0] {
        b'r' | b'b' if after_ident => return 0,
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
    match bytes.get(prefix) {
        Some(b'"') => string_len(rest, prefix),
        Some(b'\'') => char_len(rest, prefix),
        _ => 0,
    }
}

/// The length of the string literal whose opening quote is at `rest[open]`. One left open runs
/// to the end of the text.
fn string_len(rest: &str, open: usize) -> usize {
    let bytes = rest.as_bytes();
    let mut j = open + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b'"' => return j + 1,
            _ => j += 1,
        }
    }
    rest.len()
}

/// The length of the char literal whose opening tick is at `rest[tick]`, or 0 when the tick
/// starts a lifetime or a label instead. A char literal is one character, or one escape (`\n`,
/// `\'`, `\x7f`, `\u{..}`), then a closing tick; anything else after a tick is a lifetime.
fn char_len(rest: &str, tick: usize) -> usize {
    let body = &rest[tick + 1..];
    let Some(first) = body.chars().next() else {
        return 0;
    };
    let end = match first {
        '\\' => {
            let Some(escaped) = body[1..].chars().next() else {
                return 0;
            };
            match escaped {
                'u' => match body.find('}') {
                    Some(close) if body[..close].bytes().all(|b| b != b'\n') => close + 1,
                    _ => return 0,
                },
                'x' => 4,
                _ => 1 + escaped.len_utf8(),
            }
        }
        '\'' | '\n' => return 0,
        _ => first.len_utf8(),
    };
    if body.get(end..).is_some_and(|after| after.starts_with('\'')) {
        tick + 1 + end + 1
    } else {
        0
    }
}

/// The calls the layers may not make: no I/O, no threads, no environment and no clock.
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

/// The [`BANNED`] spellings left in `source` once its comments are stripped.
fn banned_in(source: &str) -> Vec<&'static str> {
    let code = strip_comments(source);
    BANNED
        .into_iter()
        .filter(|banned| code.contains(banned))
        .collect()
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

/// The #18 case: read as a char literal, the tick of `&'static` ran to the `'` inside the first
/// string, so the quotes paired up wrongly, the `//` inside the second string read as a comment,
/// and the banned call after it was stripped unseen.
#[test]
fn a_lifetime_tick_cannot_hide_a_banned_call_behind_a_string() {
    let source = "let s: &'static str = \"'\"; let u = \"//\"; std::env::var(\"X\");\n";
    assert_eq!(banned_in(source), ["std::env"]);
    assert_eq!(strip_comments(source), source);

    let generic = "fn f<'a>(x: &'a str) -> &'a str { let _ = \"it's\"; \"//\"; tokio::spawn(x) }";
    assert_eq!(banned_in(generic), ["tokio"]);
    assert_eq!(strip_comments(generic), generic);
}

#[test]
fn lifetimes_and_labels_are_code_and_their_comments_still_go() {
    let source = "impl<'a, 'b: 'a> T<'a> for &'b str {} // alpha\n'outer: loop { break 'outer; } /* beta */ done";
    let code = strip_comments(source);
    assert!(code.contains("impl<'a, 'b: 'a> T<'a> for &'b str {}"));
    assert!(code.contains("'outer: loop { break 'outer; }"));
    assert!(code.contains("done"));
    assert!(!code.contains("alpha"));
    assert!(!code.contains("beta"));
}

#[test]
fn char_and_byte_literals_are_still_literals() {
    for literal in [
        "'x'",
        "'\\n'",
        "'\\''",
        "'\\\\'",
        "'\\x7f'",
        "'\\u{1F600}'",
        "'\u{e9}'",
        "'\"'",
        "'/'",
        "b'x'",
        "b'\\''",
        "b'\"'",
    ] {
        let rest = format!("{literal} rest");
        assert_eq!(literal_len(&rest, false), literal.len(), "{literal}");
        // A `//` or `"` inside the literal neither opens a comment nor a string.
        let source = format!("let c = {literal}; let d = '/'; std::fs::read(); // gone\n");
        let code = strip_comments(&source);
        assert!(
            code.contains(&format!("let c = {literal};")),
            "{literal}: {code}"
        );
        assert!(code.contains("std::fs::read()"), "{literal}: {code}");
        assert!(!code.contains("gone"), "{literal}: {code}");
    }
    for lifetime in ["'a>", "'static str", "'outer: loop", "'_>", "'ab'"] {
        assert_eq!(literal_len(lifetime, false), 0, "{lifetime}");
    }
    // An `r` or `b` that ends an identifier prefixes nothing.
    assert_eq!(literal_len("r\"x\"", true), 0);
    assert_eq!(literal_len("b'x'", true), 0);
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
    for file in layer_sources() {
        let banned = banned_in(&fs::read_to_string(&file).unwrap());
        assert!(banned.is_empty(), "{} uses {banned:?}", file.display());
    }
}

/// Production code in the layers never panics on a `None` or an `Err`. The scan reads whole
/// lines and skips only those that start with `//`, so no bug in a comment stripper can hide a
/// call: a spelling in a trailing comment fails the test too.
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
