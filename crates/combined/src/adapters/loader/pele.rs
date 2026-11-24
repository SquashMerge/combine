//! What the adapter knows about Pele: the pinned bundle, the options it is called with, and how
//! what it threw becomes a verdict. Names no JS engine.

use super::diagram_check::DiagramLimits;
use crate::ports::{DiagramCheckError, DiagramFailure, DiagramVerdict, FailureCode};

pub const PELE_VERSION: &str = "1.13.0";
pub(super) static PELE_BUNDLE: &[u8] = include_bytes!("vendor/pele-1.13.0.global.js");
pub const PELE_GLOBAL_SHA256: &str =
    "0b0a46c57a5a6971670c09f5ec4066705bd78505f90fd11adf6260325efa0910";
pub const PELE_LICENSE_SHA256: &str =
    "311bccc30de6f7b9cbfb64232bb0c911ccb2191abe316550813ae25b2c7925b9";
pub const PELE_INTEGRITY: &str =
    "sha512-Ie8UKY9x00MsuR3jO6AzUwzK2Z6MIV3Tf4EDdITiuk8Uaz8K7g0R082WqsqNaSU8DdgkrRvZThzhit83mi/uiw==";
/// Pele's default is 5,000. The cheapest spelling of an edge is a chained link (`a-->b-->c`, 4
/// bytes per edge), so 204,800 / 4 = 51,200 admits every diagram that spells each edge as a
/// link. `&` fan-out (`a & b --> c & d` is 4 edges in one link) can exceed it; Pele then answers
/// `limit`, which is the right verdict. The time and memory caps bound the cost below that.
pub const PELE_MAX_EDGES: u32 = 51_200;
/// Pele's default, passed explicitly so the web renderer passes the same. The largest output
/// seen, for the 200 KB diagram, is 1,826,043 characters (docs/m2-s2-spike.md).
pub const PELE_OUTPUT_LIMIT: u32 = 4_000_000;
pub const MAX_MESSAGE_BYTES: usize = 1_024;
pub const MAX_SNIPPET_BYTES: usize = 1_024;

/// Which of Pele's exports a check calls.
pub(super) const ENTRY_POINT: &str = "render";

/// Words that mark a Pele message as being about front matter, lower-cased.
pub(super) const FRONT_MATTER_WORDS: &[&str] =
    &["front matter", "front-matter", "frontmatter", "yaml"];

/// The verdict a passing check gives in the mode the spike chose.
pub(super) const SUCCESS: DiagramVerdict = DiagramVerdict::Validated;

const NO_MESSAGE: &str = "Pele refused the diagram (no message)";
const BOM: char = '\u{feff}';
const DASHES: [char; 3] = ['-', '-', '-'];

/// What a call into Pele threw, read from the thrown value (see `engine.rs`).
#[derive(Debug, Clone, Default)]
pub(super) struct Thrown {
    pub name: Option<String>,
    pub message: Option<String>,
    pub code: Option<String>,
    pub line: Option<f64>,
    pub column: Option<f64>,
    pub snippet: Option<String>,
    /// The engine heap, read after the call, was within a margin of its cap.
    pub heap_near_limit: bool,
    /// The thrown value was JS `null`: QuickJS's answer when it can't allocate even an error.
    pub thrown_null: bool,
}

/// What to do with the runtime after a call, and the answer.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Ending {
    /// The runtime is sound: keep it.
    Keep(DiagramVerdict),
    /// The runtime may be broken or over a cap: rebuild it.
    Discard(Result<DiagramVerdict, DiagramCheckError>),
}

/// A position found in the source, for the errors Pele gives none for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Anchor {
    pub line: u32,
    pub column: u32,
    pub snippet: Option<String>,
}

fn failed(
    code: FailureCode,
    message: String,
    line: Option<u32>,
    column: Option<u32>,
    snippet: Option<String>,
) -> DiagramVerdict {
    DiagramVerdict::Failed(DiagramFailure {
        code,
        message,
        line,
        column,
        snippet,
    })
}

fn limit_verdict(code: FailureCode, message: String) -> DiagramVerdict {
    failed(code, message, None, None, None)
}

fn pele_code(code: &str) -> Option<FailureCode> {
    match code {
        "unsupported-diagram" => Some(FailureCode::UnsupportedDiagram),
        "syntax" => Some(FailureCode::Syntax),
        "semantic" => Some(FailureCode::Semantic),
        "limit" => Some(FailureCode::Limit),
        _ => None,
    }
}

/// Turns the outcome of one call into an answer. `thrown` is `None` when the call returned. The
/// first matching rule wins; the two engine-failure rules (memory, stack) apply only to a throw
/// that carries no Pele code, and a returned call is never a memory failure.
pub(super) fn classify(
    thrown: Option<&Thrown>,
    interrupted: bool,
    limits: &DiagramLimits,
    source: &str,
) -> Ending {
    if interrupted {
        return Ending::Discard(Ok(limit_verdict(
            FailureCode::TimeLimit,
            format!(
                "validation ran past the time limit ({:?}) and was stopped",
                limits.time_limit
            ),
        )));
    }
    if let Some(thrown) = thrown.filter(|thrown| thrown.code.is_none()) {
        let out_of_memory = thrown.thrown_null
            || thrown.heap_near_limit
            || thrown.message.as_deref() == Some("out of memory");
        if out_of_memory {
            return Ending::Discard(Ok(limit_verdict(
                FailureCode::MemoryLimit,
                format!(
                    "validation needed more than the {}-byte memory limit and was stopped",
                    limits.memory_limit
                ),
            )));
        }
        if thrown.message.as_deref() == Some("Maximum call stack size exceeded") {
            return Ending::Discard(Ok(limit_verdict(
                FailureCode::Limit,
                "the diagram is nested too deeply to validate".to_owned(),
            )));
        }
    }
    let Some(thrown) = thrown else {
        return Ending::Keep(SUCCESS);
    };
    if let Some(code) = thrown.code.as_deref().and_then(pele_code) {
        return Ending::Keep(pele_verdict(code, thrown, source));
    }
    let detail = match (thrown.name.as_deref(), thrown.message.as_deref()) {
        (Some(name), Some(message)) => format!("{name}: {message}"),
        (None, Some(message)) => message.to_owned(),
        (Some(name), None) => name.to_owned(),
        (None, None) => "(no message)".to_owned(),
    };
    Ending::Discard(Err(DiagramCheckError::NoVerdict(truncate_utf8(
        &detail,
        MAX_MESSAGE_BYTES,
    ))))
}

fn pele_verdict(code: FailureCode, thrown: &Thrown, source: &str) -> DiagramVerdict {
    let message = match thrown.message.as_deref() {
        Some(message) if !message.is_empty() => truncate_utf8(message, MAX_MESSAGE_BYTES),
        _ => NO_MESSAGE.to_owned(),
    };
    let line = thrown.line.and_then(position);
    if let Some(line) = line {
        let column = thrown.column.and_then(position);
        let snippet = thrown
            .snippet
            .as_deref()
            .filter(|snippet| !snippet.is_empty())
            .map(|snippet| truncate_utf8(snippet, MAX_SNIPPET_BYTES));
        return failed(code, message, Some(line), column, snippet);
    }
    match anchor(source, code, &message) {
        Some(anchor) => failed(
            code,
            message,
            Some(anchor.line),
            Some(anchor.column),
            anchor.snippet,
        ),
        None => {
            let snippet = thrown
                .snippet
                .as_deref()
                .filter(|snippet| !snippet.is_empty())
                .map(|snippet| truncate_utf8(snippet, MAX_SNIPPET_BYTES));
            failed(code, message, None, None, snippet)
        }
    }
}

/// A position Pele reports: a whole number from 1 to `u32::MAX`. Its default of 0 means none.
pub(super) fn position(n: f64) -> Option<u32> {
    let whole = n.is_finite() && n.fract() == 0.0;
    (whole && (1.0..=f64::from(u32::MAX)).contains(&n)).then_some(n as u32)
}

/// Cuts `s` to at most `max` bytes at a character boundary, ending in "…" when it cut.
pub(super) fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max.saturating_sub(3);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// JS `\s`: what Rust calls white space and the byte-order mark, but not U+0085.
fn is_ws(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == BOM
}

fn is_newline(c: char) -> bool {
    c == '\n' || c == '\r'
}

/// Where Pele's `Rw` finds front matter in `text`.
#[derive(Debug, PartialEq, Eq)]
enum FrontMatter {
    /// No opener: the text doesn't start with an indented `---` line.
    Absent,
    /// An opener, and `Rw` finds no closer.
    Unclosed,
    /// An opener and a closer; the closer's `---` is on this 1-based line.
    Closed { closing_line: usize },
}

fn starts_with_at(text: &[char], at: usize, pattern: &[char]) -> bool {
    text.get(at..at + pattern.len())
        .is_some_and(|window| window == pattern)
}

/// JS `lastIndexOf(pattern, from)`.
fn last_index_of(text: &[char], pattern: &[char], from: usize) -> Option<usize> {
    let last_start = text.len().checked_sub(pattern.len())?;
    (0..=from.min(last_start))
        .rev()
        .find(|&at| starts_with_at(text, at, pattern))
}

/// JS `indexOf(pattern, from)`.
fn index_of(text: &[char], pattern: &[char], from: usize) -> Option<usize> {
    let last_start = text.len().checked_sub(pattern.len())?;
    (from..=last_start).find(|&at| starts_with_at(text, at, pattern))
}

/// A port of Pele's `Rw` (bundle 1.13.0; docs/m2-s2-spike.md quotes it). `text` has its line
/// endings normalised to `\n`, as Pele does before it calls `Rw`.
fn front_matter(text: &[char]) -> FrontMatter {
    let mut indent_end = 0;
    while indent_end < text.len() && is_ws(text[indent_end]) && !is_newline(text[indent_end]) {
        indent_end += 1;
    }
    if !starts_with_at(text, indent_end, &DASHES) {
        return FrontMatter::Absent;
    }
    let mut newlines = Vec::new();
    let mut at = indent_end + DASHES.len();
    while at < text.len() && is_ws(text[at]) {
        if is_newline(text[at]) {
            newlines.push(at);
        }
        at += 1;
    }
    let Some(&first) = newlines.first() else {
        return FrontMatter::Absent;
    };
    let mut marker = text[..indent_end].to_vec();
    marker.extend(DASHES);
    let mut pattern = vec!['\n'];
    pattern.extend(&marker);
    // `s(h)` in Rw: the newline at `h` opens a closer line: the opener's indent and `---`, then
    // only white space that includes a newline.
    let closes = |at: usize| -> bool {
        if !text.get(at).copied().is_some_and(is_newline) || !starts_with_at(text, at + 1, &marker)
        {
            return false;
        }
        let mut after = at + 1 + marker.len();
        while after < text.len() && is_ws(text[after]) {
            if is_newline(text[after]) {
                return true;
            }
            after += 1;
        }
        false
    };
    // `o`: the last closer whose leading newline lies after the opener line's own newline.
    let mut last = last_index_of(text, &pattern, text.len());
    let mut last_closer = None;
    while let Some(candidate) = last.filter(|&candidate| candidate > first) {
        if closes(candidate) {
            last_closer = Some(candidate);
            break;
        }
        last = last_index_of(text, &pattern, candidate - 1);
    }
    let Some(last_closer) = last_closer else {
        return FrontMatter::Unclosed;
    };
    // `c`: the last newline of the opener's white-space run that is before `o`.
    let blank_end = newlines
        .iter()
        .copied()
        .take_while(|&newline| newline < last_closer)
        .last()
        .unwrap_or(first);
    let mut candidate = index_of(text, &pattern, blank_end);
    while let Some(at) = candidate.filter(|&at| at <= last_closer) {
        if at > blank_end && closes(at) {
            let closing_line = text[..=at].iter().filter(|&&c| c == '\n').count() + 1;
            return FrontMatter::Closed { closing_line };
        }
        candidate = index_of(text, &pattern, at + 1);
    }
    FrontMatter::Unclosed
}

/// The 1-based position and text of `line` as the anchor reports them: the column counts the
/// line's leading white space, not the byte-order mark; the snippet keeps that white space and
/// drops the mark and the trailing white space.
fn anchor_at(number: usize, line: &str) -> Option<Anchor> {
    let body_start = line.find(|c| !is_ws(c)).unwrap_or(line.len());
    let (leading, body) = line.split_at(body_start);
    let leading: String = leading.chars().filter(|&c| c != BOM).collect();
    let snippet = format!("{leading}{}", body.trim_end_matches(is_ws));
    Some(Anchor {
        line: u32::try_from(number).ok()?,
        column: u32::try_from(leading.chars().count() + 1).ok()?,
        snippet: Some(truncate_utf8(&snippet, MAX_SNIPPET_BYTES)).filter(|text| !text.is_empty()),
    })
}

/// A position for an error Pele gave none for: front matter, or an unknown diagram type.
/// Pure, and used only when Pele gave no line.
pub(super) fn anchor(source: &str, code: FailureCode, message: &str) -> Option<Anchor> {
    let normalised = source.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<&str> = normalised.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let chars: Vec<char> = normalised.chars().collect();
    let front = front_matter(&chars);
    if matches!(
        code,
        FailureCode::UnsupportedDiagram | FailureCode::Syntax | FailureCode::Semantic
    ) {
        let lowered = message.to_lowercase();
        let about_front_matter = match front {
            FrontMatter::Absent => false,
            FrontMatter::Unclosed => true,
            FrontMatter::Closed { .. } => {
                FRONT_MATTER_WORDS.iter().any(|word| lowered.contains(word))
            }
        };
        if about_front_matter {
            return anchor_at(1, lines.first()?);
        }
    }
    if code != FailureCode::UnsupportedDiagram {
        return None;
    }
    let skip = match front {
        FrontMatter::Closed { closing_line } => closing_line,
        FrontMatter::Absent | FrontMatter::Unclosed => 0,
    };
    let first_content = lines.iter().enumerate().skip(skip).find(|(_, line)| {
        let line = line.trim_matches(is_ws);
        !line.is_empty() && !line.starts_with("%%")
    });
    match first_content {
        Some((index, line)) => anchor_at(index + 1, line),
        None => Some(Anchor {
            line: 1,
            column: 1,
            snippet: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    fn hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn vendored_bundle_is_the_pinned_npm_artifact() {
        assert_eq!(PELE_BUNDLE.len(), 733_670);
        assert_eq!(hex(PELE_BUNDLE), PELE_GLOBAL_SHA256);
    }

    #[test]
    fn vendored_license_is_the_npm_artifact() {
        let license = include_bytes!("vendor/LICENSE");
        assert_eq!(license.len(), 1_934);
        assert_eq!(hex(license), PELE_LICENSE_SHA256);
    }

    #[test]
    fn vendor_readme_records_the_pinned_artifact() {
        let readme = include_str!("vendor/README.md");
        for needle in [
            PELE_GLOBAL_SHA256,
            PELE_LICENSE_SHA256,
            PELE_INTEGRITY,
            "pele-1.13.0.tgz",
        ] {
            assert!(readme.contains(needle), "the vendor README lacks {needle}");
        }
    }

    #[test]
    fn pele_codes_map_to_failure_codes() {
        assert_eq!(
            pele_code("unsupported-diagram"),
            Some(FailureCode::UnsupportedDiagram)
        );
        assert_eq!(pele_code("syntax"), Some(FailureCode::Syntax));
        assert_eq!(pele_code("semantic"), Some(FailureCode::Semantic));
        assert_eq!(pele_code("limit"), Some(FailureCode::Limit));
        assert_eq!(pele_code("other"), None);
        assert_eq!(pele_code(""), None);
    }

    fn limits() -> DiagramLimits {
        DiagramLimits::default()
    }

    fn pele_error(code: &str, message: &str) -> Thrown {
        Thrown {
            name: Some("PeleError".to_owned()),
            message: Some(message.to_owned()),
            code: Some(code.to_owned()),
            ..Thrown::default()
        }
    }

    fn engine_error(message: &str) -> Thrown {
        Thrown {
            name: Some("InternalError".to_owned()),
            message: Some(message.to_owned()),
            ..Thrown::default()
        }
    }

    fn verdict_of(ending: Ending) -> DiagramVerdict {
        match ending {
            Ending::Keep(verdict) => verdict,
            Ending::Discard(answer) => panic!("expected Keep, got Discard({answer:?})"),
        }
    }

    fn failure_of(verdict: DiagramVerdict) -> DiagramFailure {
        match verdict {
            DiagramVerdict::Failed(failure) => failure,
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    fn discarded_failure(ending: Ending) -> DiagramFailure {
        match ending {
            Ending::Discard(Ok(verdict)) => failure_of(verdict),
            other => panic!("expected Discard(Ok(Failed)), got {other:?}"),
        }
    }

    #[test]
    fn classify_covers_every_ending() {
        let limits = limits();
        let source = "classDiagram\n";

        let timed_out = discarded_failure(classify(None, true, &limits, source));
        assert_eq!(timed_out.code, FailureCode::TimeLimit);
        assert_eq!(
            timed_out.message,
            "validation ran past the time limit (10s) and was stopped"
        );
        assert_eq!(
            (timed_out.line, timed_out.column, timed_out.snippet),
            (None, None, None)
        );

        let beaten_by_interrupt = pele_error("syntax", "boom");
        let ending = classify(Some(&beaten_by_interrupt), true, &limits, source);
        assert_eq!(discarded_failure(ending).code, FailureCode::TimeLimit);

        let oom = engine_error("out of memory");
        let memory = discarded_failure(classify(Some(&oom), false, &limits, source));
        assert_eq!(memory.code, FailureCode::MemoryLimit);
        assert_eq!(
            memory.message,
            "validation needed more than the 67108864-byte memory limit and was stopped"
        );

        let near_limit = Thrown {
            heap_near_limit: true,
            ..engine_error("something else")
        };
        let ending = classify(Some(&near_limit), false, &limits, source);
        assert_eq!(discarded_failure(ending).code, FailureCode::MemoryLimit);

        let stack = engine_error("Maximum call stack size exceeded");
        let nested = discarded_failure(classify(Some(&stack), false, &limits, source));
        assert_eq!(nested.code, FailureCode::Limit);
        assert_eq!(
            nested.message,
            "the diagram is nested too deeply to validate"
        );

        assert_eq!(
            classify(None, false, &limits, source),
            Ending::Keep(SUCCESS)
        );

        let syntax = Thrown {
            line: Some(4.0),
            column: Some(12.0),
            snippet: Some("  Animal <|-x- Dog\n-----------^".to_owned()),
            ..pele_error("syntax", "Parse error on line 4")
        };
        let failure = failure_of(verdict_of(classify(Some(&syntax), false, &limits, source)));
        assert_eq!(failure.code, FailureCode::Syntax);
        assert_eq!((failure.line, failure.column), (Some(4), Some(12)));
        assert_eq!(
            failure.snippet.as_deref(),
            Some("  Animal <|-x- Dog\n-----------^")
        );

        for (code, expected) in [
            ("unsupported-diagram", FailureCode::UnsupportedDiagram),
            ("semantic", FailureCode::Semantic),
            ("limit", FailureCode::Limit),
        ] {
            let thrown = pele_error(code, "no good");
            let failure = failure_of(verdict_of(classify(Some(&thrown), false, &limits, "x\n")));
            assert_eq!(failure.code, expected);
        }

        let empty = pele_error("syntax", "");
        let failure = failure_of(verdict_of(classify(Some(&empty), false, &limits, source)));
        assert_eq!(failure.message, "Pele refused the diagram (no message)");
        let missing = Thrown {
            message: None,
            ..pele_error("syntax", "")
        };
        let failure = failure_of(verdict_of(classify(Some(&missing), false, &limits, source)));
        assert_eq!(failure.message, "Pele refused the diagram (no message)");
    }

    #[test]
    fn a_pele_error_that_says_out_of_memory_stays_a_pele_verdict() {
        let thrown = Thrown {
            heap_near_limit: true,
            ..pele_error("syntax", "out of memory")
        };
        let ending = classify(Some(&thrown), false, &limits(), "x\n");
        assert_eq!(failure_of(verdict_of(ending)).code, FailureCode::Syntax);
    }

    #[test]
    fn a_thrown_null_is_a_memory_limit() {
        let thrown = Thrown {
            message: Some("null".to_owned()),
            thrown_null: true,
            ..Thrown::default()
        };
        let ending = classify(Some(&thrown), false, &limits(), "x\n");
        assert_eq!(discarded_failure(ending).code, FailureCode::MemoryLimit);
    }

    #[test]
    fn a_thrown_string_null_is_no_verdict() {
        let thrown = Thrown {
            message: Some("null".to_owned()),
            ..Thrown::default()
        };
        let ending = classify(Some(&thrown), false, &limits(), "x\n");
        assert_eq!(
            ending,
            Ending::Discard(Err(DiagramCheckError::NoVerdict("null".to_owned())))
        );
    }

    #[test]
    fn a_returned_call_is_a_success_even_with_the_heap_at_the_cap() {
        // The heap flag lives on a throw; a call that returned has none (review finding N5).
        assert_eq!(
            classify(None, false, &limits(), "x\n"),
            Ending::Keep(SUCCESS)
        );
    }

    #[test]
    fn stack_overflow_inside_pele_is_pele_s_limit_verdict() {
        let wrapped = pele_error("limit", "Diagram is nested too deeply.");
        let failure = failure_of(verdict_of(classify(
            Some(&wrapped),
            false,
            &limits(),
            "x\n",
        )));
        assert_eq!(failure.code, FailureCode::Limit);
        assert_eq!(failure.message, "Diagram is nested too deeply.");
    }

    #[test]
    fn any_other_throw_is_no_verdict() {
        let cases = [
            (Some("TypeError"), Some("boom"), "TypeError: boom"),
            (None, Some("boom"), "boom"),
            (Some("TypeError"), None, "TypeError"),
            (None, None, "(no message)"),
        ];
        for (name, message, detail) in cases {
            let thrown = Thrown {
                name: name.map(str::to_owned),
                message: message.map(str::to_owned),
                ..Thrown::default()
            };
            let ending = classify(Some(&thrown), false, &limits(), "x\n");
            assert_eq!(
                ending,
                Ending::Discard(Err(DiagramCheckError::NoVerdict(detail.to_owned())))
            );
        }
        let unknown_code = pele_error("future-code", "hm");
        let ending = classify(Some(&unknown_code), false, &limits(), "x\n");
        assert_eq!(
            ending,
            Ending::Discard(Err(DiagramCheckError::NoVerdict(
                "PeleError: hm".to_owned()
            )))
        );
    }

    #[test]
    fn positions_are_whole_one_based_numbers() {
        assert_eq!(position(1.0), Some(1));
        assert_eq!(position(4_294_967_295.0), Some(u32::MAX));
        for bad in [
            0.0,
            -1.0,
            1.5,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            4_294_967_296.0,
        ] {
            assert_eq!(position(bad), None, "{bad}");
        }
    }

    #[test]
    fn truncation_keeps_whole_characters() {
        assert_eq!(truncate_utf8("short", 10), "short");
        let text = "é".repeat(600);
        let cut = truncate_utf8(&text, MAX_MESSAGE_BYTES);
        assert!(cut.len() <= MAX_MESSAGE_BYTES);
        assert!(cut.ends_with('…'));
        assert!(cut.trim_end_matches('…').chars().all(|c| c == 'é'));
        let cut = truncate_utf8("ééééé", 8);
        assert_eq!(cut, "éé…");
    }

    fn anchored(anchor: Option<Anchor>) -> Option<(u32, u32, Option<String>)> {
        anchor.map(|anchor| (anchor.line, anchor.column, anchor.snippet))
    }

    fn at(line: u32, column: u32, snippet: Option<&str>) -> Option<(u32, u32, Option<String>)> {
        Some((line, column, snippet.map(str::to_owned)))
    }

    const NO_DIAGRAM: &str = "No diagram type detected for the given text.";

    #[test]
    fn anchor_for_an_unsupported_diagram_skips_front_matter_blank_and_comment_lines() {
        let source = "---\ntitle: x\n---\n\n%% note\n  notADiagram\n  class A\n";
        let found = anchor(source, FailureCode::UnsupportedDiagram, NO_DIAGRAM);
        assert_eq!(anchored(found), at(6, 3, Some("  notADiagram")));
    }

    #[test]
    fn anchor_for_an_unsupported_diagram_on_the_first_line() {
        let source = "notADiagram\r\n  class Animal\n";
        let found = anchor(source, FailureCode::UnsupportedDiagram, NO_DIAGRAM);
        assert_eq!(anchored(found), at(1, 1, Some("notADiagram")));
    }

    #[test]
    fn anchor_for_a_source_with_no_diagram_line() {
        let source = "\n   \n%% only a comment\n";
        let found = anchor(source, FailureCode::UnsupportedDiagram, NO_DIAGRAM);
        assert_eq!(anchored(found), at(1, 1, None));
        let found = anchor("", FailureCode::UnsupportedDiagram, NO_DIAGRAM);
        assert_eq!(anchored(found), at(1, 1, None));
    }

    #[test]
    fn anchor_for_unclosed_front_matter() {
        let source = "---\ntitle: Animals\nclassDiagram\n  class Animal\n";
        for code in [
            FailureCode::UnsupportedDiagram,
            FailureCode::Syntax,
            FailureCode::Semantic,
        ] {
            let found = anchor(source, code, NO_DIAGRAM);
            assert_eq!(anchored(found), at(1, 1, Some("---")), "{code}");
        }
        assert_eq!(anchor(source, FailureCode::Limit, "too big"), None);
    }

    #[test]
    fn anchor_for_closed_front_matter_needs_pele_to_name_it() {
        let source = "---\ntitle: [unclosed\n---\nclassDiagram\n";
        let found = anchor(
            source,
            FailureCode::Syntax,
            "Front matter is not valid YAML.",
        );
        assert_eq!(anchored(found), at(1, 1, Some("---")));
        assert_eq!(
            anchor(source, FailureCode::Syntax, "Unexpected token"),
            None
        );
    }

    #[test]
    fn front_matter_opener_is_trimmed_like_peles() {
        let source = "\u{feff}  ---  \t\r\ntitle: x\n";
        let found = anchor(source, FailureCode::UnsupportedDiagram, NO_DIAGRAM);
        assert_eq!(anchored(found), at(1, 3, Some("  ---")));
    }

    #[test]
    fn front_matter_after_leading_blank_lines() {
        let source = "\n  \n---\ntitle: x\nclassDiagram\n";
        let found = anchor(source, FailureCode::UnsupportedDiagram, NO_DIAGRAM);
        assert_eq!(anchored(found), at(3, 1, Some("---")));
    }

    #[test]
    fn pele_positions_are_never_replaced() {
        let thrown = Thrown {
            line: Some(3.0),
            column: Some(5.0),
            snippet: Some("title: x".to_owned()),
            ..pele_error("unsupported-diagram", NO_DIAGRAM)
        };
        let source = "---\ntitle: Animals\nclassDiagram\n";
        let failure = failure_of(verdict_of(classify(
            Some(&thrown),
            false,
            &limits(),
            source,
        )));
        assert_eq!((failure.line, failure.column), (Some(3), Some(5)));
        assert_eq!(failure.snippet.as_deref(), Some("title: x"));
    }

    #[test]
    fn anchor_uses_js_white_space() {
        // U+0085 is white space to Rust but not to JS: it is neither trimmed nor counted.
        let source = "\u{85}notADiagram\n";
        let found = anchor(source, FailureCode::UnsupportedDiagram, NO_DIAGRAM);
        assert_eq!(anchored(found), at(1, 1, Some("\u{85}notADiagram")));
        // U+FEFF and U+3000 are white space to JS: U+3000 is counted and kept, U+FEFF is neither.
        let source = "\u{feff}\u{3000}notADiagram\u{3000}\n";
        let found = anchor(source, FailureCode::UnsupportedDiagram, NO_DIAGRAM);
        assert_eq!(anchored(found), at(1, 2, Some("\u{3000}notADiagram")));
        // A line holding only U+0085 is not blank.
        let source = "\u{85}\nnotADiagram\n";
        let found = anchor(source, FailureCode::UnsupportedDiagram, NO_DIAGRAM);
        assert_eq!(anchored(found), at(1, 1, Some("\u{85}")));
    }

    fn front_matter_of(source: &str) -> FrontMatter {
        let chars: Vec<char> = source.chars().collect();
        front_matter(&chars)
    }

    #[test]
    fn front_matter_is_chosen_as_pele_chooses_it() {
        use FrontMatter::{Absent, Closed, Unclosed};
        // A closer on line 2 is never a closer.
        assert_eq!(front_matter_of("---\n---\nclassDiagram\n"), Unclosed);
        // A closer right after the opener's blank lines closes only as the last closer.
        assert_eq!(
            front_matter_of("---\n\n---\nclassDiagram\n  class A\n"),
            Closed { closing_line: 3 }
        );
        assert_eq!(
            front_matter_of("---\n---\n---\nclassDiagram\n  class A\n"),
            Closed { closing_line: 3 }
        );
        assert_eq!(
            front_matter_of("---\n---\ntitle: [x\n---\nclassDiagram\n"),
            Closed { closing_line: 4 }
        );
        // Pele-confirmed: opener, blank, closer, invalid YAML, closer, classDiagram closes at 5.
        assert_eq!(
            front_matter_of("---\n\n---\ntitle: [x\n---\nclassDiagram\n  class A\n"),
            Closed { closing_line: 5 }
        );
        assert_eq!(
            front_matter_of("---\n\n---\nnotADiagram\n---\nclassDiagram\n  class A\n"),
            Closed { closing_line: 5 }
        );
        assert_eq!(
            front_matter_of("---\n\n---\nfoo: 1\n---\nnotADiagram\n"),
            Closed { closing_line: 5 }
        );
        // The closer repeats the opener's indent, BOM included, and needs a newline after it.
        assert_eq!(
            front_matter_of("  \t---\ntitle: [x\n  \t---\nc\n"),
            Closed { closing_line: 3 }
        );
        assert_eq!(front_matter_of("  \t---\ntitle: [x\n---\nc\n"), Unclosed);
        assert_eq!(front_matter_of("\u{feff}---\nt: 1\n---\nc\n"), Unclosed);
        assert_eq!(
            front_matter_of("\u{feff}---\nt: 1\n\u{feff}---\nc\n"),
            Closed { closing_line: 3 }
        );
        assert_eq!(front_matter_of("---\ntitle: [x\n---"), Unclosed);
        // Only line 1 opens, and nothing may follow the dashes.
        assert_eq!(front_matter_of("\n---\ntitle: [x\n---\nc\n"), Absent);
        assert_eq!(front_matter_of("--- x\ntitle: y\n---\nc\n"), Absent);
        assert_eq!(front_matter_of("classDiagram\n"), Absent);
        assert_eq!(front_matter_of(""), Absent);
    }

    #[test]
    fn anchor_follows_the_closer_pele_chooses() {
        let source = "---\n\n---\nfoo: 1\n---\nnotADiagram\n";
        let found = anchor(source, FailureCode::UnsupportedDiagram, NO_DIAGRAM);
        assert_eq!(anchored(found), at(6, 1, Some("notADiagram")));
        let source = "---\n\n---\ntitle: [x\n---\nclassDiagram\n  class A\n";
        let found = anchor(
            source,
            FailureCode::Syntax,
            "Front matter is not valid YAML.",
        );
        assert_eq!(anchored(found), at(1, 1, Some("---")));
        let source = "---\n---\nnotADiagram\n";
        let found = anchor(source, FailureCode::UnsupportedDiagram, NO_DIAGRAM);
        assert_eq!(anchored(found), at(1, 1, Some("---")));
    }
}
