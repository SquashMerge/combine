//! What the contract suite needs from a backend under test.
//!
//! A case never names a backend: it drives a [`Target`]. The fake implements it in
//! `fake_target.rs`; an adapter's test crate implements it for the real backend and reuses
//! every case file unchanged.

use std::future::Future;
use std::sync::Arc;

use combine_protocol::TermId;
use combined::model::SpawnSpec;
use combined::ports::TerminalBackend;
use combined::testing::Half;

/// Why a target can't do something. Cases declare what they need, so a case that gets this
/// has found a gap in its own `needs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported(pub &'static str);

/// A condition a backend may or may not be able to produce. A case lists the ones it needs and
/// is skipped on a target that can't.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    InexactTerminal,
    RemoteTwin,
    DelayedOutput,
    ForcedResync,
    HalfFailure,
    OpenFailure,
    BackendRestart,
    BackendCrash,
    LateOldGenerationEvents,
    NoReplacementPaint,
    NoByteCredit,
    Observation,
}

#[allow(async_fn_in_trait)]
pub trait Target: Sized + 'static {
    /// Recorded with every result.
    fn describe(&self) -> String;

    /// Runs a case to completion on this target's runtime. Fake: `futures::executor::block_on`.
    /// A real adapter: a tokio runtime.
    fn block_on<F: Future>(future: F) -> F::Output;

    /// The same instance for the target's life. It outlives daemon restarts.
    fn backend(&self) -> Arc<dyn TerminalBackend>;

    fn supports(&self, fault: Fault) -> bool;

    /// A directory a terminal may start in, as the canonical absolute path the backend will
    /// report as its cwd (macOS: `/private/tmp/...`, never `/tmp/...`). Fake:
    /// `/fake-scratch/<name>`. A real target creates it under its scratch root.
    fn scratch_dir(&mut self, name: &str) -> String;

    /// A spawn spec for a program that echoes typed input and writes whatever `emit` asks.
    /// Fake: argv `["fake-echo"]`.
    fn spawn_spec(&mut self, cwd: &str, cols: u16, rows: u16) -> SpawnSpec;

    /// The bytes that mark where measurement starts. A backend that doesn't pass an unknown
    /// OSC through unchanged overrides this with `generator::printable_marker(nonce)`.
    fn marker(nonce: &str) -> Vec<u8> {
        crate::contract::generator::osc_marker(nonce)
    }

    async fn emit(&mut self, term: &TermId, bytes: &[u8]) -> Result<(), Unsupported>;

    /// Gives the backend up to one quiet interval to make progress, and says whether it did
    /// something. Fake: runs `control.step()` until it returns false and reports whether any
    /// step ran, with no waiting. A real target sleeps its quiet interval on its runtime and
    /// returns false.
    async fn idle(&mut self) -> bool;

    /// Rounds with nothing moving that count as settled. Fake: 1.
    fn quiet_rounds(&self) -> usize;

    /// The hard bound on rounds before a wait fails. Fake: 100,000.
    fn max_rounds(&self) -> usize;

    fn typed_input(&self, term: &TermId) -> Result<Vec<u8>, Unsupported>;
    fn automatic_input(&self, term: &TermId) -> Result<Vec<Vec<u8>>, Unsupported>;
    fn pty_size(&self, term: &TermId) -> Result<(u16, u16), Unsupported>;
    fn open_halves(&self, term: &TermId) -> Result<usize, Unsupported>;
    fn set_inexact(&mut self, term: &TermId) -> Result<(), Unsupported>;
    fn remote_twin(&mut self, local: &TermId) -> Result<TermId, Unsupported>;
    fn delay_output(&mut self, term: &TermId) -> Result<(), Unsupported>;
    fn release_output(&mut self, term: &TermId) -> Result<(), Unsupported>;

    /// `index` is the attach order on `term`, from 0.
    fn force_resync(&mut self, term: &TermId, index: usize) -> Result<(), Unsupported>;

    /// `index` is the attach order on `term`, from 0.
    fn fail_half(&mut self, term: &TermId, index: usize, half: Half) -> Result<(), Unsupported>;

    fn fail_next_opens(&mut self, term: &TermId, count: u32) -> Result<(), Unsupported>;
    fn max_reopen_attempts(&self) -> Option<u32>;
    async fn restart_backend(&mut self) -> Result<(), Unsupported>;
    async fn crash_backend(&mut self) -> Result<(), Unsupported>;
    fn set_late_old_generation_events(&mut self, on: bool) -> Result<(), Unsupported>;
    fn set_paint_after_resize(&mut self, on: bool) -> Result<(), Unsupported>;

    /// Rebuilds the backend without byte credit. Only before the first `backend()` call.
    fn disable_byte_credit(&mut self) -> Result<(), Unsupported>;
}
