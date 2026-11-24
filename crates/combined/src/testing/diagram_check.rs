#![cfg(feature = "test-support")]
//! A scripted [`DiagramCheck`], for tests of what runs on top of validation. It applies no caps,
//! runs no threads and reads no clock.

use std::collections::HashMap;
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
        if let Some(mut released) = hold {
            let _ = released.wait_for(|released| *released).await;
        }
        let state = self.state();
        state.answers.get(source).unwrap_or(&state.default).clone()
    }
}

#[cfg(test)]
mod tests {
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

    #[tokio::test]
    async fn validates_by_default_and_records_calls() {
        let fake = FakeDiagramCheck::new();
        assert_eq!(fake.check("a").await, Ok(DiagramVerdict::Validated));
        assert_eq!(fake.check("b").await, Ok(DiagramVerdict::Validated));
        assert_eq!(fake.calls(), ["a", "b"]);
    }

    #[tokio::test]
    async fn per_source_answers_win_over_the_default() {
        let fake = FakeDiagramCheck::new();
        fake.answer_all(Err(DiagramCheckError::Unavailable("down".into())));
        fake.answer("bad", Ok(failure()));
        assert_eq!(fake.check("bad").await, Ok(failure()));
        assert_eq!(
            fake.check("other").await,
            Err(DiagramCheckError::Unavailable("down".into()))
        );
    }

    #[tokio::test]
    async fn a_hold_waits_until_released_or_dropped() {
        let fake = std::sync::Arc::new(FakeDiagramCheck::new());
        let hold = fake.hold("slow");
        let task = tokio::spawn({
            let fake = fake.clone();
            async move { fake.check("slow").await }
        });
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        hold.release();
        assert_eq!(task.await.unwrap(), Ok(DiagramVerdict::Validated));

        let dropped = fake.hold("slow");
        drop(dropped);
        assert_eq!(fake.check("slow").await, Ok(DiagramVerdict::Validated));
    }
}
