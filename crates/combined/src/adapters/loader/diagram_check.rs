//! The loader adapter's [`DiagramCheck`]: Pele, run in a JS engine on its own thread. Names no
//! JS engine.

use std::sync::mpsc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::oneshot;

use super::engine::{self, Request};
use crate::ports::{DiagramCheck, DiagramCheckError, DiagramFailure, DiagramVerdict, FailureCode};

/// SPEC.md's "Mermaid source size", 200 KiB.
pub const MAX_SOURCE_BYTES: usize = 204_800;
/// How long one check may run before it is stopped.
pub const TIME_LIMIT: Duration = Duration::from_secs(10);
/// The engine heap's cap. It is the heap, not the process.
pub const MEMORY_LIMIT: usize = 64 * 1024 * 1024;

const ENGINE_STACK_BYTES: usize = 8 * 1024 * 1024;

/// The caps a [`PeleDiagramCheck`] enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagramLimits {
    pub max_source_bytes: usize,
    pub time_limit: Duration,
    pub memory_limit: usize,
}

impl Default for DiagramLimits {
    fn default() -> Self {
        DiagramLimits {
            max_source_bytes: MAX_SOURCE_BYTES,
            time_limit: TIME_LIMIT,
            memory_limit: MEMORY_LIMIT,
        }
    }
}

/// The engine's state, for diagnostics, tests and the benchmark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineStats {
    /// Bytes the engine heap holds now (QuickJS `malloc_size`), after the last check's garbage
    /// was collected.
    pub heap_now: u64,
    /// Bytes it held when the last check finished, before collection; `None` before the first
    /// check.
    pub heap_after_last_check: Option<u64>,
    /// The memory cap.
    pub heap_limit: u64,
    /// Runtimes built successfully so far: 1 after the warm-up, and one more per rebuild.
    pub builds: u64,
}

/// Pele in a JS engine on a thread of its own. One runtime lives for as long as the checker; a
/// check that ends over a cap, or in anything but a verdict, has its runtime rebuilt.
pub struct PeleDiagramCheck {
    limits: DiagramLimits,
    requests: mpsc::Sender<Request>,
    refused: Option<String>,
}

/// The first build of the warm runtime, which [`PeleDiagramCheck::start`] begins and does not
/// wait for.
pub struct Warmup(oneshot::Receiver<Result<(), DiagramCheckError>>);

fn stopped() -> DiagramCheckError {
    DiagramCheckError::Unavailable("the engine thread has stopped".to_owned())
}

impl Warmup {
    /// Waits for the warm-up: `Ok` when the runtime is ready, else why it is not.
    pub async fn finished(self) -> Result<(), DiagramCheckError> {
        self.0.await.unwrap_or_else(|_| Err(stopped()))
    }
}

impl PeleDiagramCheck {
    /// Spawns the engine thread, which builds the warm runtime (Pele evaluated once) and then
    /// serves checks in order. Returns at once; checks sent before the warm-up finishes wait for
    /// it. The queue is unbounded: callers bound their own concurrency.
    pub fn start(limits: DiagramLimits) -> (PeleDiagramCheck, Warmup) {
        let (requests, inbox) = mpsc::channel();
        let (warmed, warmup) = oneshot::channel();
        let refused = if limits.max_source_bytes == 0
            || limits.time_limit.is_zero()
            || limits.memory_limit == 0
        {
            Some("the diagram check's limits must be non-zero".to_owned())
        } else {
            std::thread::Builder::new()
                .name("combined-diagram-check".into())
                .stack_size(ENGINE_STACK_BYTES)
                .spawn(move || engine::serve(limits, inbox, warmed))
                .err()
                .map(|error| format!("cannot start the engine thread: {error}"))
        };
        let warmup = match &refused {
            None => warmup,
            Some(reason) => {
                let (warmed, warmup) = oneshot::channel();
                let _ = warmed.send(Err(DiagramCheckError::Unavailable(reason.clone())));
                warmup
            }
        };
        (
            PeleDiagramCheck {
                limits,
                requests,
                refused,
            },
            Warmup(warmup),
        )
    }

    /// The engine's heap and build counts.
    pub async fn stats(&self) -> Result<EngineStats, DiagramCheckError> {
        if let Some(reason) = &self.refused {
            return Err(DiagramCheckError::Unavailable(reason.clone()));
        }
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(Request::Stats { reply })
            .map_err(|_| stopped())?;
        answer.await.map_err(|_| stopped())?
    }
}

#[async_trait]
impl DiagramCheck for PeleDiagramCheck {
    async fn check(&self, source: &str) -> Result<DiagramVerdict, DiagramCheckError> {
        let len = source.len();
        let max = self.limits.max_source_bytes;
        if len > max {
            return Ok(DiagramVerdict::Failed(DiagramFailure {
                code: FailureCode::TooLarge,
                message: format!("the diagram source is {len} bytes; the limit is {max} bytes"),
                line: None,
                column: None,
                snippet: None,
            }));
        }
        if let Some(reason) = &self.refused {
            return Err(DiagramCheckError::Unavailable(reason.clone()));
        }
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(Request::Check {
                source: source.to_owned(),
                reply,
            })
            .map_err(|_| stopped())?;
        answer.await.map_err(|_| stopped())?
    }
}
