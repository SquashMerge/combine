#![allow(dead_code)]
//! Shared by the diagram tests and the benchmark: input generators, the caps' calibration and
//! the searches for the smallest memory caps. Each test crate uses part of it.

use std::time::{Duration, Instant};

use combined::adapters::loader::{DiagramLimits, PeleDiagramCheck, MEMORY_LIMIT, TIME_LIMIT};
use combined::ports::{DiagramCheck, DiagramVerdict};

pub const MIB: usize = 1024 * 1024;

/// A small valid class diagram.
pub const CLASS: &str = include_str!("../fixtures/diagrams/class.mmd");

/// What a passing check answers in the mode docs/m2-s2-spike.md chose: Pele's full `render()`.
pub const SUCCESS: DiagramVerdict = DiagramVerdict::Validated;

/// The tests that stop a check at a cap measure time and memory, so they run one at a time.
/// A tokio mutex, so holding it across an `.await` is sound.
pub static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const RELATIONS: [&str; 6] = ["<|--", "*--", "o--", "-->", "..>", "..|>"];

const CLASS_BLOCK: &str = "    +String name\n    +int count\n    +List~String~ tags\n    +load(int id) bool\n    +save() void\n  }\n";

fn class_block(i: usize) -> String {
    format!("  class C{i:04} {{\n{CLASS_BLOCK}")
}

fn relation(i: usize) -> String {
    let parent = (i - 1) / 2;
    format!("  C{parent:04} {} C{i:04}\n", RELATIONS[i % 6])
}

/// `n` classes of five members each, joined in a binary tree by relations of six kinds.
pub fn class_diagram(n: usize) -> String {
    let mut source = String::from("classDiagram\n");
    for i in 0..n {
        source.push_str(&class_block(i));
    }
    for i in 1..n {
        source.push_str(&relation(i));
    }
    source
}

/// The largest class diagram that `pad_to` can fill to exactly `bytes`.
pub fn class_diagram_of_bytes(bytes: usize) -> String {
    let mut n = 0;
    let mut len = "classDiagram\n".len();
    loop {
        let next = len + class_block(n).len() + if n >= 1 { relation(n).len() } else { 0 };
        if next + 3 > bytes {
            break;
        }
        len = next;
        n += 1;
    }
    // `len` is the length of class_diagram(n) only once the relation lines are counted in the
    // same order; the loop above adds each relation with its class, which gives the same total.
    let source = class_diagram(n);
    assert_eq!(source.len(), len);
    pad_to(&source, bytes, 'x')
}

/// Appends a comment line that brings `source` to exactly `bytes` bytes: `fill` as often as it
/// fits, then `x` for the rest.
pub fn pad_to(source: &str, bytes: usize, fill: char) -> String {
    assert!(source.ends_with('\n'));
    let room = bytes
        .checked_sub(source.len() + 3)
        .expect("room for a comment line");
    let mut padded = String::with_capacity(bytes);
    padded.push_str(source);
    padded.push_str("%%");
    for _ in 0..room / fill.len_utf8() {
        padded.push(fill);
    }
    for _ in 0..room % fill.len_utf8() {
        padded.push('x');
    }
    padded.push('\n');
    assert_eq!(padded.len(), bytes);
    padded
}

/// `depth` subgraphs inside one another around one node.
pub fn nested_subgraphs(depth: usize) -> String {
    let mut source = String::from("flowchart TD\n");
    for k in 0..depth {
        source.push_str(&format!("subgraph g{k}\n"));
    }
    source.push_str("leaf[leaf]\n");
    source.push_str(&"end\n".repeat(depth));
    source
}

/// `n` sources joined to `n` targets with `&`: n squared edges in one line.
pub fn fan_out(n: usize) -> String {
    let side = |prefix: &str| {
        (0..n)
            .map(|i| format!("{prefix}{i}"))
            .collect::<Vec<_>>()
            .join(" & ")
    };
    format!("flowchart LR\n{} --> {}\n", side("a"), side("b"))
}

/// The input that takes long enough to stop on a time cap and still ends in `SUCCESS` at the
/// default caps: docs/m2-s2-spike.md, "Checkpoint A decisions" (A2 and N1).
pub fn slow_input() -> String {
    class_diagram_of_bytes(204_800)
}

/// The input that runs out of memory at a small cap: docs/m2-s2-spike.md, P9 and "Checkpoint A
/// decisions" (A1: it answers `MemoryLimit` at W + h through a thrown `null`).
pub fn memory_input() -> String {
    fan_out(226)
}

fn limits_with_memory(memory_limit: usize, time_limit: Duration) -> DiagramLimits {
    DiagramLimits {
        memory_limit,
        time_limit,
        ..DiagramLimits::default()
    }
}

/// Whether a fresh checker with `limits` warms up.
pub async fn starts(limits: DiagramLimits) -> bool {
    let (_check, warmup) = PeleDiagramCheck::start(limits);
    warmup.finished().await.is_ok()
}

/// Whether a fresh checker with `limits` warms up and answers `SUCCESS` for `source`.
pub async fn validates(limits: DiagramLimits, source: &str) -> bool {
    let (check, warmup) = PeleDiagramCheck::start(limits);
    warmup.finished().await.is_ok() && check.check(source).await == Ok(SUCCESS)
}

/// W: the smallest memory cap, at 1 MiB resolution, at which the warm-up succeeds.
pub async fn smallest_memory_limit_that_starts() -> usize {
    let trial = |mib: usize| limits_with_memory(mib * MIB, Duration::from_secs(60));
    let (mut low, mut high) = (1, MEMORY_LIMIT / MIB);
    assert!(!starts(trial(low)).await, "the warm-up starts at 1 MiB");
    assert!(
        starts(trial(high)).await,
        "the warm-up fails at the default cap"
    );
    while high - low > 1 {
        let middle = (low + high) / 2;
        if starts(trial(middle)).await {
            high = middle;
        } else {
            low = middle;
        }
    }
    high * MIB
}

/// h: the least headroom over `w`, from 1 to 16 MiB, at which a fresh checker warms up and
/// answers `SUCCESS` for `source` as its first check.
pub async fn smallest_headroom(w: usize, source: &str) -> usize {
    for mib in 1..=16 {
        let limits = limits_with_memory(w + mib * MIB, TIME_LIMIT);
        if validates(limits, source).await {
            return mib * MIB;
        }
    }
    panic!("no headroom up to 16 MiB; escalate (G9)");
}

/// The smallest memory cap, at 1 MiB resolution and at most `hi`, at which `source` answers
/// `SUCCESS`; `None` when it doesn't at `hi`.
pub async fn smallest_memory_limit_that_validates(source: &str, hi: usize) -> Option<usize> {
    let trial = |mib: usize| limits_with_memory(mib * MIB, Duration::from_secs(60));
    let (mut low, mut high) = (0, hi / MIB);
    if !validates(trial(high), source).await {
        return None;
    }
    while high - low > 1 {
        let middle = (low + high) / 2;
        if validates(trial(middle), source).await {
            high = middle;
        } else {
            low = middle;
        }
    }
    Some(high * MIB)
}

/// How long `check` takes to answer `source`.
pub async fn timed(check: &PeleDiagramCheck, source: &str) -> (Duration, Option<DiagramVerdict>) {
    let started = Instant::now();
    let answer = check.check(source).await.ok();
    (started.elapsed(), answer)
}

/// What the time-cap test and the benchmark calibrate under the default 10 s cap (review
/// finding N1): L = a slow check's best time / 8, and the time of a warm small check.
pub struct TimeCalibration {
    pub limit: Duration,
    pub t_class: Duration,
}

/// Measures `t_first`, `t_class` and `t_slow` at the default caps and returns L. Panics, naming
/// the numbers, when L is under 10 ms or under 10 times a small check.
pub async fn calibrate_time_limit() -> TimeCalibration {
    let (check, warmup) = PeleDiagramCheck::start(DiagramLimits::default());
    warmup.finished().await.expect("the engine starts");
    let (t_first, answer) = timed(&check, CLASS).await;
    assert_eq!(answer, Some(SUCCESS));
    let mut warm = Vec::new();
    for _ in 0..5 {
        let (took, answer) = timed(&check, CLASS).await;
        assert_eq!(answer, Some(SUCCESS));
        warm.push(took);
    }
    warm.sort();
    let t_class = warm[warm.len() / 2];
    let slow = slow_input();
    let mut t_slow = TIME_LIMIT;
    for _ in 0..2 {
        let (took, answer) = timed(&check, &slow).await;
        let took = if answer == Some(SUCCESS) {
            took
        } else {
            TIME_LIMIT
        };
        t_slow = t_slow.min(took);
    }
    let limit = t_slow / 8;
    let small = t_class.max(t_first);
    assert!(
        limit >= Duration::from_millis(10) && limit >= small * 10,
        "t_first {t_first:?}, t_class {t_class:?}, t_slow {t_slow:?}, L {limit:?}: \
         re-run the spike's P7/P9 rule"
    );
    TimeCalibration { limit, t_class }
}
