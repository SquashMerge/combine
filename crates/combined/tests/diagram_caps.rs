//! The caps through the port: a check that runs past the time cap or the memory cap is stopped,
//! and the next check works on a rebuilt runtime. These measure time and memory, so they run in
//! their own test binary, one at a time (`SERIAL`).

mod diagram_support;

use std::time::Duration;

use combined::adapters::loader::{DiagramLimits, PeleDiagramCheck};
use combined::ports::{DiagramCheck, DiagramVerdict, FailureCode};
use diagram_support::{
    calibrate_time_limit, memory_input, slow_input, smallest_headroom,
    smallest_memory_limit_that_starts, CLASS, SERIAL, SUCCESS,
};

fn failure_code(answer: Option<DiagramVerdict>) -> Option<FailureCode> {
    match answer {
        Some(DiagramVerdict::Failed(failure)) => Some(failure.code),
        _ => None,
    }
}

#[tokio::test]
async fn time_limit_stops_a_run_and_the_next_check_works() {
    let _serial = SERIAL.lock().await;
    let calibration = calibrate_time_limit().await;
    let limit = calibration.limit;
    let (check, warmup) = PeleDiagramCheck::start(DiagramLimits {
        time_limit: limit,
        ..DiagramLimits::default()
    });
    warmup.finished().await.expect("the engine starts");
    let slow = slow_input();
    for cycle in 0..3 {
        let (took, answer) = diagram_support::timed(&check, &slow).await;
        assert_eq!(
            failure_code(answer),
            Some(FailureCode::TimeLimit),
            "cycle {cycle}"
        );
        assert!(
            took <= limit + Duration::from_secs(1),
            "cycle {cycle}: {took:?}"
        );
        assert_eq!(check.check(CLASS).await, Ok(SUCCESS), "cycle {cycle}");
    }
    assert_eq!(check.stats().await.expect("stats").builds, 4);
}

#[tokio::test]
async fn memory_limit_stops_a_run_and_the_next_check_works() {
    let _serial = SERIAL.lock().await;
    let w = smallest_memory_limit_that_starts().await;
    let h = smallest_headroom(w, CLASS).await;
    let (check, warmup) = PeleDiagramCheck::start(DiagramLimits {
        memory_limit: w + h,
        time_limit: Duration::from_secs(60),
        ..DiagramLimits::default()
    });
    warmup.finished().await.expect("the engine starts at W + h");
    let input = memory_input();
    for cycle in 0..3 {
        let (_, answer) = diagram_support::timed(&check, &input).await;
        assert_eq!(
            failure_code(answer),
            Some(FailureCode::MemoryLimit),
            "cycle {cycle}"
        );
        assert_eq!(check.check(CLASS).await, Ok(SUCCESS), "cycle {cycle}");
    }
    assert_eq!(check.stats().await.expect("stats").builds, 4);
}

#[tokio::test(flavor = "current_thread")]
async fn checks_do_not_block_the_async_caller() {
    let _serial = SERIAL.lock().await;
    let (check, warmup) = PeleDiagramCheck::start(DiagramLimits::default());
    warmup.finished().await.expect("the engine starts");
    let slow = slow_input();
    let pending = check.check(&slow);
    tokio::pin!(pending);
    tokio::select! {
        () = tokio::time::sleep(Duration::from_millis(10)) => {}
        _ = &mut pending => panic!("the check finished before a 10 ms sleep"),
    }
    assert_eq!(pending.await, Ok(SUCCESS));
}
