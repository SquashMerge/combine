//! The loader adapter's `DiagramCheck`, through its public surface: verdicts, positions, the
//! size cap and an engine that cannot start. The timing-sensitive caps are in `diagram_caps.rs`.

mod diagram_support;

use std::time::Duration;

use combined::adapters::loader::{
    DiagramLimits, PeleDiagramCheck, MAX_MESSAGE_BYTES, MAX_SNIPPET_BYTES, MAX_SOURCE_BYTES,
    MEMORY_LIMIT, TIME_LIMIT,
};
use combined::ports::{
    DiagramCheck, DiagramCheckError, DiagramFailure, DiagramVerdict, FailureCode,
};
use diagram_support::{
    class_diagram, class_diagram_of_bytes, nested_subgraphs, pad_to, CLASS, MIB, SUCCESS,
};
use sha2::{Digest, Sha256};

async fn warm() -> PeleDiagramCheck {
    let (check, warmup) = PeleDiagramCheck::start(DiagramLimits::default());
    warmup.finished().await.expect("the engine starts");
    check
}

fn failure(answer: Result<DiagramVerdict, DiagramCheckError>) -> DiagramFailure {
    match answer {
        Ok(DiagramVerdict::Failed(failure)) => failure,
        other => panic!("expected a failure, got {other:?}"),
    }
}

fn hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn generators_match_the_spike_inputs() {
    let fifty = class_diagram(50);
    assert_eq!(fifty.len(), 6_661);
    assert_eq!(
        hex(&fifty),
        "b06519def05745bb06b7a33389899728dbf71ff3fbea4142a9e4a99426980ee9"
    );
    let big = class_diagram_of_bytes(204_800);
    assert_eq!(big.len(), 204_800);
    assert_eq!(
        hex(&big),
        "8f59fa859369d2970c37d1535aefa860b097979be21812c8bddb8863a63fc3f0"
    );
    let fan = diagram_support::fan_out(226);
    assert_eq!(fan.len(), 2_957);
    assert_eq!(
        hex(&fan),
        "aa5277c930da858a80ab32aa5f139688dc7897f95f9b7152f8b20e41ef479108"
    );
}

#[tokio::test]
async fn class_diagram_is_validated() {
    let check = warm().await;
    assert_eq!(check.check(CLASS).await, Ok(SUCCESS));
}

struct Expected {
    name: &'static str,
    source: &'static str,
    code: FailureCode,
    line: u32,
    column: u32,
    snippet: &'static str,
}

const BROKEN: [Expected; 5] = [
    Expected {
        name: "broken-syntax",
        source: include_str!("fixtures/diagrams/broken-syntax.mmd"),
        code: FailureCode::Syntax,
        line: 6,
        column: 3,
        snippet: "  @@@ not a statement\n--^",
    },
    Expected {
        name: "broken-unknown-type",
        source: include_str!("fixtures/diagrams/broken-unknown-type.mmd"),
        code: FailureCode::UnsupportedDiagram,
        line: 1,
        column: 1,
        snippet: "notADiagram",
    },
    Expected {
        name: "broken-unclosed-block",
        source: include_str!("fixtures/diagrams/broken-unclosed-block.mmd"),
        code: FailureCode::Syntax,
        line: 5,
        column: 18,
        snippet: "  Animal <|-- Dog\n-----------------^",
    },
    Expected {
        name: "broken-relation",
        source: include_str!("fixtures/diagrams/broken-relation.mmd"),
        code: FailureCode::Syntax,
        line: 4,
        column: 12,
        snippet: "  Animal <|-x- Dog\n-----------^",
    },
    Expected {
        name: "broken-front-matter",
        source: include_str!("fixtures/diagrams/broken-front-matter.mmd"),
        code: FailureCode::Syntax,
        line: 1,
        column: 1,
        snippet: "---",
    },
];

#[tokio::test]
async fn broken_fixtures_fail_with_a_position() {
    let check = warm().await;
    for pass in 0..2 {
        for expected in &BROKEN {
            let failure = failure(check.check(expected.source).await);
            let name = expected.name;
            assert_eq!(failure.code, expected.code, "{name}, pass {pass}");
            assert_eq!(failure.line, Some(expected.line), "{name} line");
            assert_eq!(failure.column, Some(expected.column), "{name} column");
            assert_eq!(
                failure.snippet.as_deref(),
                Some(expected.snippet),
                "{name} snippet"
            );
            assert!(!failure.message.is_empty(), "{name} message");
            assert!(failure.message.len() <= MAX_MESSAGE_BYTES, "{name} message");
            assert!(
                expected.snippet.len() <= MAX_SNIPPET_BYTES,
                "{name} snippet"
            );
        }
        assert_eq!(check.check(CLASS).await, Ok(SUCCESS), "pass {pass}");
    }
}

#[tokio::test]
async fn source_at_the_cap_is_accepted() {
    let check = warm().await;
    for fill in ['x', 'é'] {
        let source = pad_to(CLASS, 204_800, fill);
        assert_eq!(source.len(), 204_800);
        assert_eq!(check.check(&source).await, Ok(SUCCESS), "fill {fill}");
    }
}

#[tokio::test]
async fn source_over_the_cap_is_refused_before_the_engine_runs() {
    let source = pad_to(CLASS, 204_801, 'x');
    let too_large = |answer| {
        let failure = failure(answer);
        assert_eq!(failure.code, FailureCode::TooLarge);
        assert_eq!(
            failure.message,
            "the diagram source is 204801 bytes; the limit is 204800 bytes"
        );
        assert_eq!(
            (failure.line, failure.column, failure.snippet),
            (None, None, None)
        );
    };

    let (broken, _) = PeleDiagramCheck::start(DiagramLimits {
        memory_limit: MIB,
        ..DiagramLimits::default()
    });
    assert!(matches!(
        broken.check(CLASS).await,
        Err(DiagramCheckError::Unavailable(_))
    ));
    too_large(broken.check(&source).await);

    too_large(warm().await.check(&source).await);
}

#[tokio::test]
async fn engine_that_cannot_start_is_unavailable() {
    let (check, warmup) = PeleDiagramCheck::start(DiagramLimits {
        memory_limit: MIB,
        ..DiagramLimits::default()
    });
    assert!(matches!(
        warmup.finished().await,
        Err(DiagramCheckError::Unavailable(_))
    ));
    assert!(matches!(
        check.check(CLASS).await,
        Err(DiagramCheckError::Unavailable(_))
    ));
    assert!(matches!(
        check.stats().await,
        Err(DiagramCheckError::Unavailable(_))
    ));
}

#[tokio::test]
async fn zero_limits_are_refused() {
    let default = DiagramLimits::default();
    let zeroed = [
        DiagramLimits {
            max_source_bytes: 0,
            ..default
        },
        DiagramLimits {
            time_limit: Duration::ZERO,
            ..default
        },
        DiagramLimits {
            memory_limit: 0,
            ..default
        },
    ];
    for limits in zeroed {
        let (check, warmup) = PeleDiagramCheck::start(limits);
        for answer in [
            warmup.finished().await.map(|()| SUCCESS),
            check.check("").await,
            check.stats().await.map(|_| SUCCESS),
        ] {
            match answer {
                Err(DiagramCheckError::Unavailable(reason)) => {
                    assert!(reason.contains("non-zero"), "{reason}");
                }
                other => panic!("expected Unavailable for {limits:?}, got {other:?}"),
            }
        }
    }
}

#[tokio::test]
async fn engine_stats_report_the_warm_runtime() {
    let check = warm().await;
    assert_eq!(check.check(CLASS).await, Ok(SUCCESS));
    let stats = check.stats().await.expect("stats");
    assert!(stats.heap_now > 0);
    assert_eq!(stats.heap_limit, MEMORY_LIMIT as u64);
    assert!(stats.heap_after_last_check.is_some());
    assert_eq!(stats.builds, 1);
}

#[tokio::test]
async fn deeply_nested_diagram_gets_a_verdict() {
    let check = warm().await;
    // P10: a thousand nested subgraphs validate (docs/m2-s2-spike.md).
    assert_eq!(check.check(&nested_subgraphs(1_000)).await, Ok(SUCCESS));
    assert_eq!(check.check(CLASS).await, Ok(SUCCESS));
}

/// Where Pele's front matter closes decides what the anchor says; each case is one the
/// spike confirmed through Pele itself (docs/m2-s2-spike.md, "Closer selection").
#[tokio::test]
async fn front_matter_anchors_agree_with_pele() {
    struct Case {
        source: &'static str,
        verdict: Option<(FailureCode, u32, &'static str)>,
    }
    let cases = [
        // Closes at line 5, so the YAML between is front matter and invalid.
        Case {
            source: "---\n\n---\ntitle: [x\n---\nclassDiagram\n  class A\n",
            verdict: Some((FailureCode::Syntax, 1, "---")),
        },
        // Closes at line 5: `notADiagram` is front matter.
        Case {
            source: "---\n\n---\nnotADiagram\n---\nclassDiagram\n  class A\n",
            verdict: None,
        },
        // Closes at line 5, so the diagram text starts at line 6.
        Case {
            source: "---\n\n---\nfoo: 1\n---\nnotADiagram\n",
            verdict: Some((FailureCode::UnsupportedDiagram, 6, "notADiagram")),
        },
        // A closer straight after the blank line closes when it is the last one.
        Case {
            source: "---\n\n---\nclassDiagram\n  class A\n",
            verdict: None,
        },
        Case {
            source: "---\n---\n---\nclassDiagram\n  class A\n",
            verdict: None,
        },
        // A closer on line 2 is not a closer.
        Case {
            source: "---\n---\nclassDiagram\n  class A\n",
            verdict: Some((FailureCode::UnsupportedDiagram, 1, "---")),
        },
        Case {
            source: "---\n---\ntitle: [x\n---\nclassDiagram\n",
            verdict: Some((FailureCode::Syntax, 1, "---")),
        },
        // Only line 1 opens front matter.
        Case {
            source: "\n---\ntitle: [x\n---\nclassDiagram\n",
            verdict: Some((FailureCode::UnsupportedDiagram, 2, "---")),
        },
        // A byte-order mark is indent: a plain closer doesn't close it.
        Case {
            source: "\u{feff}---\ntitle: x\n---\nclassDiagram\n  class A\n",
            verdict: Some((FailureCode::UnsupportedDiagram, 1, "---")),
        },
    ];
    let check = warm().await;
    for case in cases {
        let answer = check.check(case.source).await;
        match case.verdict {
            None => assert_eq!(answer, Ok(SUCCESS), "{:?}", case.source),
            Some((code, line, snippet)) => {
                let failure = failure(answer);
                let context = format!("{:?}: {failure:?}", case.source);
                assert_eq!(failure.code, code, "{context}");
                assert_eq!(failure.line, Some(line), "{context}");
                assert_eq!(failure.column, Some(1), "{context}");
                assert_eq!(failure.snippet.as_deref(), Some(snippet), "{context}");
            }
        }
    }
}

/// AC3 and AC4: the shipped caps are the story's numbers, 200 KB (SPEC's unit, 204,800 bytes),
/// 10 s and 64 MB, and the default limits use them.
#[test]
fn default_limits_are_the_story_caps() {
    assert_eq!(MAX_SOURCE_BYTES, 200 * 1024);
    assert_eq!(TIME_LIMIT, Duration::from_secs(10));
    assert_eq!(MEMORY_LIMIT, 64 * MIB);
    assert_eq!(
        DiagramLimits::default(),
        DiagramLimits {
            max_source_bytes: MAX_SOURCE_BYTES,
            time_limit: TIME_LIMIT,
            memory_limit: MEMORY_LIMIT,
        }
    );
}

/// AC8: the JS engine is pinned to an exact version in the crate manifest.
#[test]
fn rquickjs_is_pinned_exactly() {
    let manifest = include_str!("../Cargo.toml");
    let line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("rquickjs"))
        .expect("the manifest names rquickjs");
    assert!(line.contains("version = \"="), "not an exact pin: {line}");
}
