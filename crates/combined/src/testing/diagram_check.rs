#![cfg(feature = "test-support")]
//! A scripted [`DiagramCheck`], for tests of what runs on top of validation. It applies no caps,
//! runs no threads and reads no clock.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use tokio::sync::watch;

use crate::ports::{DiagramCheck, DiagramCheckError, DiagramVerdict};

/// What a scripted check answers.
pub type FakeAnswer = Result<DiagramVerdict, DiagramCheckError>;

struct FakeState {
    default: FakeAnswer,
    answers: HashMap<String, FakeAnswer>,
    holds: HashMap<String, watch::Receiver<bool>>,
    calls: Vec<String>,
}

/// A [`DiagramCheck`] that answers from a script and records what it was asked.
pub struct FakeDiagramCheck {
    state: Mutex<FakeState>,
    in_flight: AtomicUsize,
    peak_in_flight: AtomicUsize,
}

/// Counts one check as in flight until it is dropped.
struct InFlight<'a>(&'a AtomicUsize);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Default for FakeDiagramCheck {
    fn default() -> Self {
        FakeDiagramCheck::new()
    }
}

impl FakeDiagramCheck {
    /// Answers every check `Ok(Validated)` until scripted otherwise.
    pub fn new() -> FakeDiagramCheck {
        FakeDiagramCheck {
            state: Mutex::new(FakeState {
                default: Ok(DiagramVerdict::Validated),
                answers: HashMap::new(),
                holds: HashMap::new(),
                calls: Vec::new(),
            }),
            in_flight: AtomicUsize::new(0),
            peak_in_flight: AtomicUsize::new(0),
        }
    }

    fn state(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The answer for every source without its own.
    pub fn answer_all(&self, answer: FakeAnswer) {
        self.state().default = answer;
    }

    /// The answer for checks of exactly `source`.
    pub fn answer(&self, source: &str, answer: FakeAnswer) {
        self.state().answers.insert(source.to_owned(), answer);
    }

    /// Checks of `source` wait until the returned hold is released or dropped.
    pub fn hold(&self, source: &str) -> FakeHold {
        let (released, receiver) = watch::channel(false);
        self.state().holds.insert(source.to_owned(), receiver);
        FakeHold { released }
    }

    /// Every source checked so far, in call order.
    pub fn calls(&self) -> Vec<String> {
        self.state().calls.clone()
    }

    /// How many checks have started and not yet answered.
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }

    /// The most checks that were in flight at once.
    pub fn peak_in_flight(&self) -> usize {
        self.peak_in_flight.load(Ordering::SeqCst)
    }
}

/// Keeps checks of one source waiting. Releasing or dropping it lets them go.
pub struct FakeHold {
    released: watch::Sender<bool>,
}

impl FakeHold {
    /// Lets the held checks answer.
    pub fn release(self) {
        self.released.send_replace(true);
    }
}

impl Drop for FakeHold {
    fn drop(&mut self) {
        self.released.send_replace(true);
    }
}

#[async_trait]
impl DiagramCheck for FakeDiagramCheck {
    async fn check(&self, source: &str) -> FakeAnswer {
        let hold = {
            let mut state = self.state();
            state.calls.push(source.to_owned());
            state.holds.get(source).cloned()
        };
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(now, Ordering::SeqCst);
        let _counted = InFlight(&self.in_flight);
        if let Some(mut released) = hold {
            // `Ok`: the hold was released. `Err`: its sender is gone, which only happens once
            // the hold is dropped, so that releases the check too.
            match released.wait_for(|released| *released).await {
                Ok(_) | Err(_) => {}
            }
        }
        let state = self.state();
        state.answers.get(source).unwrap_or(&state.default).clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ports::{DiagramFailure, FailureCode};

    fn failure() -> DiagramVerdict {
        DiagramVerdict::Failed(DiagramFailure {
            code: FailureCode::Syntax,
            message: "bad".into(),
            line: Some(1),
            column: Some(2),
            snippet: None,
        })
    }

    async fn settle() {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn answers_validated_by_default() {
        let fake = FakeDiagramCheck::new();
        assert_eq!(fake.check("a").await, Ok(DiagramVerdict::Validated));
        assert_eq!(
            FakeDiagramCheck::default().check("b").await,
            Ok(DiagramVerdict::Validated)
        );
    }

    #[tokio::test]
    async fn answer_all_scripts_every_check() {
        let fake = FakeDiagramCheck::new();
        fake.answer_all(Ok(failure()));
        assert_eq!(fake.check("a").await, Ok(failure()));
        assert_eq!(fake.check("b").await, Ok(failure()));
        fake.answer_all(Ok(DiagramVerdict::ValidatedParserOnly));
        assert_eq!(
            fake.check("a").await,
            Ok(DiagramVerdict::ValidatedParserOnly)
        );
    }

    #[tokio::test]
    async fn a_source_answer_overrides_answer_all() {
        let fake = FakeDiagramCheck::new();
        fake.answer_all(Ok(DiagramVerdict::ValidatedParserOnly));
        fake.answer("bad", Ok(failure()));
        assert_eq!(fake.check("bad").await, Ok(failure()));
        assert_eq!(
            fake.check("other").await,
            Ok(DiagramVerdict::ValidatedParserOnly)
        );
    }

    #[tokio::test]
    async fn scripts_both_errors() {
        let fake = FakeDiagramCheck::new();
        let unavailable = DiagramCheckError::Unavailable("the engine thread has stopped".into());
        let no_verdict = DiagramCheckError::NoVerdict("TypeError: boom".into());
        fake.answer("down", Err(unavailable.clone()));
        fake.answer_all(Err(no_verdict.clone()));
        assert_eq!(fake.check("down").await, Err(unavailable));
        assert_eq!(fake.check("other").await, Err(no_verdict));
    }

    #[tokio::test]
    async fn records_checked_sources_in_order() {
        let fake = FakeDiagramCheck::new();
        assert!(fake.calls().is_empty());
        for source in ["b", "a", "b"] {
            let _ = fake.check(source).await;
        }
        assert_eq!(fake.calls(), ["b", "a", "b"]);
    }

    #[tokio::test]
    async fn a_held_check_waits_until_released() {
        let fake = Arc::new(FakeDiagramCheck::new());
        fake.answer("slow", Ok(failure()));
        let hold = fake.hold("slow");
        let task = tokio::spawn({
            let fake = fake.clone();
            async move { fake.check("slow").await }
        });
        settle().await;
        assert_eq!(fake.calls(), ["slow"]);
        assert!(!task.is_finished());
        assert_eq!(fake.check("fast").await, Ok(DiagramVerdict::Validated));
        hold.release();
        settle().await;
        assert!(task.is_finished());
        assert_eq!(task.await.unwrap(), Ok(failure()));
    }

    #[tokio::test]
    async fn counts_checks_in_flight_and_their_peak() {
        let fake = Arc::new(FakeDiagramCheck::new());
        assert_eq!((fake.in_flight(), fake.peak_in_flight()), (0, 0));
        let hold = fake.hold("slow");
        let tasks: Vec<_> = (0..2)
            .map(|_| {
                let fake = fake.clone();
                tokio::spawn(async move { fake.check("slow").await })
            })
            .collect();
        settle().await;
        assert_eq!((fake.in_flight(), fake.peak_in_flight()), (2, 2));
        assert_eq!(fake.check("fast").await, Ok(DiagramVerdict::Validated));
        assert_eq!((fake.in_flight(), fake.peak_in_flight()), (2, 3));
        hold.release();
        for task in tasks {
            let _ = task.await;
        }
        assert_eq!((fake.in_flight(), fake.peak_in_flight()), (0, 3));
    }

    #[tokio::test]
    async fn a_dropped_hold_releases() {
        let fake = Arc::new(FakeDiagramCheck::new());
        fake.answer("slow", Ok(failure()));
        let hold = fake.hold("slow");
        let task = tokio::spawn({
            let fake = fake.clone();
            async move { fake.check("slow").await }
        });
        settle().await;
        assert_eq!(fake.calls(), ["slow"]);
        assert!(!task.is_finished());
        drop(hold);
        settle().await;
        assert!(task.is_finished());
        assert_eq!(task.await.unwrap(), Ok(failure()));
    }
}
