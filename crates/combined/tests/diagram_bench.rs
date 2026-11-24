//! The benchmark for headless Mermaid validation (docs/m2-s2-bench.md). Run it with
//! `sh tools/diagram-bench`, which takes the large heavy-run tier and builds in release.
//! `./tools/verify` never runs it. It prints one line per metric and never fails on the
//! thresholds; the go line reports them.

mod diagram_support;

use std::process::Command;
use std::time::Duration;

use combined::adapters::loader::{DiagramLimits, PeleDiagramCheck, MEMORY_LIMIT};
use diagram_support::{
    calibrate_time_limit, class_diagram, class_diagram_of_bytes, slow_input,
    smallest_memory_limit_that_starts, smallest_memory_limit_that_validates, timed, CLASS, MIB,
    SUCCESS,
};
use sha2::{Digest, Sha256};

const BYTES_200_KB: usize = 204_800;
const SEARCH_CEILING: usize = 512 * MIB;
const LIMIT_MS: f64 = 5_000.0;
const LIMIT_MIB: usize = MEMORY_LIMIT / MIB;

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

/// The value at rank ceil(p * n) of the sorted samples (nearest rank).
fn percentile(sorted: &[f64], p: f64) -> f64 {
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn rss_kib() -> u64 {
    let output = Command::new("/bin/ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("ps prints the resident size in KiB")
}

async fn warm_checker(limits: DiagramLimits) -> PeleDiagramCheck {
    let (check, warmup) = PeleDiagramCheck::start(limits);
    warmup.finished().await.expect("the engine starts");
    check
}

struct Warm {
    samples: Vec<f64>,
    max_heap: u64,
}

/// Discards `discard` runs, times `runs`, and asserts every verdict is `SUCCESS`.
async fn warm_runs(check: &PeleDiagramCheck, source: &str, discard: usize, runs: usize) -> Warm {
    let mut samples = Vec::new();
    let mut max_heap = 0;
    for run in 0..discard + runs {
        let (took, answer) = timed(check, source).await;
        assert_eq!(answer, Some(SUCCESS), "run {run}");
        let stats = check.stats().await.expect("stats");
        max_heap = max_heap.max(stats.heap_after_last_check.expect("a check has run"));
        if run >= discard {
            samples.push(ms(took));
        }
    }
    samples.sort_by(f64::total_cmp);
    Warm { samples, max_heap }
}

fn print_warm(input: &str, warm: &Warm) {
    let s = &warm.samples;
    println!(
        "diagram_bench metric=warm_check input={input} unit=ms n={} min={:.1} p50={:.1} p95={:.1} max={:.1}",
        s.len(),
        s[0],
        percentile(s, 0.5),
        percentile(s, 0.95),
        s[s.len() - 1]
    );
}

fn mib_or_over(cap: Option<usize>) -> String {
    match cap {
        Some(bytes) => (bytes / MIB).to_string(),
        None => format!(">{}", SEARCH_CEILING / MIB),
    }
}

#[tokio::test]
#[ignore = "benchmark: run tools/diagram-bench (heavy.sh, release)"]
async fn diagram_validation_benchmark() {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    println!(
        "diagram_bench header engine=rquickjs pele=1.13.0 verdict=validated profile={profile}"
    );

    let i50 = class_diagram(50);
    let i200 = class_diagram_of_bytes(BYTES_200_KB);
    println!(
        "diagram_bench input name=50_classes bytes={} classes=50 sha256={}",
        i50.len(),
        sha256(&i50)
    );
    println!(
        "diagram_bench input name=200_kb bytes={BYTES_200_KB} classes={} sha256={}",
        i200.matches("\n  class C").count(),
        sha256(&i200)
    );

    let mut cold = Vec::new();
    for _ in 0..5 {
        let started = std::time::Instant::now();
        warm_checker(DiagramLimits::default()).await;
        cold.push(ms(started.elapsed()));
    }
    cold.sort_by(f64::total_cmp);
    println!(
        "diagram_bench metric=cold_load unit=ms n=5 min={:.1} p50={:.1} max={:.1}",
        cold[0],
        percentile(&cold, 0.5),
        cold[4]
    );

    let fresh = warm_checker(DiagramLimits::default()).await;
    let (first, answer) = timed(&fresh, &i50).await;
    assert_eq!(answer, Some(SUCCESS));
    println!(
        "diagram_bench metric=first_check input=50_classes unit=ms value={:.1}",
        ms(first)
    );
    drop(fresh);

    let check = warm_checker(DiagramLimits::default()).await;
    let heap_after_warmup = check.stats().await.expect("stats").heap_now;
    let rss_after_warmup = rss_kib();
    let warm50 = warm_runs(&check, &i50, 3, 30).await;
    print_warm("50_classes", &warm50);
    let warm200 = warm_runs(&check, &i200, 3, 20).await;
    print_warm("200_kb", &warm200);
    let rss_after_200 = rss_kib();

    let calibration = calibrate_time_limit().await;
    let capped = warm_checker(DiagramLimits {
        time_limit: calibration.limit,
        ..DiagramLimits::default()
    })
    .await;
    let (_, answer) = timed(&capped, &slow_input()).await;
    assert!(matches!(
        answer,
        Some(combined::ports::DiagramVerdict::Failed(ref failure))
            if failure.code == combined::ports::FailureCode::TimeLimit
    ));
    let (next, answer) = timed(&capped, CLASS).await;
    assert_eq!(answer, Some(SUCCESS));
    assert_eq!(capped.stats().await.expect("stats").builds, 2);
    println!(
        "diagram_bench metric=rebuild_after_cap unit=ms value={:.1}",
        ms(next.saturating_sub(calibration.t_class))
    );

    println!("diagram_bench metric=heap_after_warmup unit=bytes value={heap_after_warmup}");
    println!(
        "diagram_bench metric=heap_after_check input=50_classes unit=bytes max={}",
        warm50.max_heap
    );
    println!(
        "diagram_bench metric=heap_after_check input=200_kb unit=bytes max={}",
        warm200.max_heap
    );

    let w = smallest_memory_limit_that_starts().await;
    println!(
        "diagram_bench metric=smallest_cap input=warmup unit=MiB value={}",
        w / MIB
    );
    let cap50 = smallest_memory_limit_that_validates(&i50, SEARCH_CEILING).await;
    println!(
        "diagram_bench metric=smallest_cap input=50_classes unit=MiB value={}",
        mib_or_over(cap50)
    );
    let cap200 = smallest_memory_limit_that_validates(&i200, SEARCH_CEILING).await;
    println!(
        "diagram_bench metric=smallest_cap input=200_kb unit=MiB value={}",
        mib_or_over(cap200)
    );

    println!("diagram_bench metric=rss input=after_warmup unit=KiB value={rss_after_warmup}");
    println!("diagram_bench metric=rss input=after_200_kb unit=KiB value={rss_after_200}");

    let p95 = percentile(&warm200.samples, 0.95);
    let go = p95 <= LIMIT_MS && cap200.is_some_and(|bytes| bytes / MIB <= LIMIT_MIB);
    println!(
        "diagram_bench go={go} warm_p95_200_kb_ms={p95:.1} limit_ms=5000 smallest_cap_200_kb_mib={} limit_mib={LIMIT_MIB}",
        mib_or_over(cap200)
    );
}
