//! The in-memory fake, as a contract [`Target`].

use std::cell::Cell;
use std::future::Future;
use std::sync::Arc;

use combine_protocol::TermId;
use combined::model::SpawnSpec;
use combined::ports::TerminalBackend;
use combined::testing::{
    FakeBackend, FakeConfig, FakeControl, Half, Mutation, ResyncMode, FAKE_PAINT_START,
};

use super::target::{Fault, Target, Unsupported};

pub struct FakeTarget {
    config: FakeConfig,
    backend: Arc<FakeBackend>,
    control: FakeControl,
    backend_taken: Cell<bool>,
    /// Faults this target pretends it can't produce, so a test can see a case skipped.
    pub unsupported: Vec<Fault>,
    /// Overrides `quiet_rounds`.
    pub quiet: usize,
    /// Overrides `max_rounds`.
    pub max: usize,
}

impl FakeTarget {
    pub fn new() -> Self {
        Self::with_config(FakeConfig::default())
    }

    pub fn with_config(config: FakeConfig) -> Self {
        let (backend, control) = FakeBackend::new(config.clone());
        FakeTarget {
            config,
            backend,
            control,
            backend_taken: Cell::new(false),
            unsupported: Vec::new(),
            quiet: 1,
            max: 100_000,
        }
    }

    pub fn mutated(mutation: Mutation) -> Self {
        Self::with_config(FakeConfig {
            mutation: Some(mutation),
            ..FakeConfig::default()
        })
    }

    /// A fake that repaints in band, with no resync signal.
    pub fn silent() -> Self {
        Self::with_config(FakeConfig {
            resync: ResyncMode::Silent,
            ..FakeConfig::default()
        })
    }

    /// The fake's own handle, for a test of the fake itself.
    pub fn control(&self) -> &FakeControl {
        &self.control
    }
}

impl Default for FakeTarget {
    fn default() -> Self {
        FakeTarget::new()
    }
}

impl Target for FakeTarget {
    fn describe(&self) -> String {
        match self.config.mutation {
            Some(mutation) => format!("fake (mutation {mutation:?})"),
            None if self.config.resync == ResyncMode::Silent => "fake-silent".to_string(),
            None => "fake".to_string(),
        }
    }

    fn paint_start(&self) -> Option<&'static [u8]> {
        Some(FAKE_PAINT_START)
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        futures::executor::block_on(future)
    }

    fn backend(&self) -> Arc<dyn TerminalBackend> {
        self.backend_taken.set(true);
        Arc::clone(&self.backend) as Arc<dyn TerminalBackend>
    }

    fn supports(&self, fault: Fault) -> bool {
        !self.unsupported.contains(&fault)
    }

    fn scratch_dir(&mut self, name: &str) -> String {
        format!("/fake-scratch/{name}")
    }

    fn spawn_spec(&mut self, cwd: &str, cols: u16, rows: u16) -> SpawnSpec {
        SpawnSpec {
            argv: vec!["fake-echo".to_string()],
            cwd: cwd.to_string(),
            env: Default::default(),
            labels: Default::default(),
            cols,
            rows,
        }
    }

    async fn emit(&mut self, term: &TermId, bytes: &[u8]) -> Result<(), Unsupported> {
        self.control.emit(term, bytes);
        Ok(())
    }

    async fn idle(&mut self) -> bool {
        let mut stepped = false;
        while self.control.step() {
            stepped = true;
        }
        stepped
    }

    fn quiet_rounds(&self) -> usize {
        self.quiet
    }

    fn max_rounds(&self) -> usize {
        self.max
    }

    fn typed_input(&self, term: &TermId) -> Result<Vec<u8>, Unsupported> {
        Ok(self.control.typed_input(term))
    }

    fn automatic_input(&self, term: &TermId) -> Result<Vec<Vec<u8>>, Unsupported> {
        Ok(self.control.automatic_input(term))
    }

    fn pty_size(&self, term: &TermId) -> Result<(u16, u16), Unsupported> {
        self.control
            .pty_size(term)
            .ok_or(Unsupported("the fake has no such terminal"))
    }

    fn open_halves(&self, term: &TermId) -> Result<usize, Unsupported> {
        Ok(self.control.open_halves(term))
    }

    fn set_inexact(&mut self, term: &TermId) -> Result<(), Unsupported> {
        self.control
            .set_fidelity(term, combine_protocol::Fidelity::Inexact);
        Ok(())
    }

    fn remote_twin(&mut self, local: &TermId) -> Result<TermId, Unsupported> {
        Ok(self.control.remote_twin(local))
    }

    fn delay_output(&mut self, term: &TermId) -> Result<(), Unsupported> {
        self.control.delay_output(term);
        Ok(())
    }

    fn release_output(&mut self, term: &TermId) -> Result<(), Unsupported> {
        self.control.release_output(term);
        Ok(())
    }

    fn force_resync(&mut self, term: &TermId, index: usize) -> Result<(), Unsupported> {
        let attachment = self
            .control
            .attachments(term)
            .get(index)
            .copied()
            .ok_or(Unsupported("no attachment at that index"))?;
        self.control.force_resync(attachment);
        Ok(())
    }

    fn fail_half(&mut self, term: &TermId, index: usize, half: Half) -> Result<(), Unsupported> {
        let attachment = self
            .control
            .attachments(term)
            .get(index)
            .copied()
            .ok_or(Unsupported("no attachment at that index"))?;
        self.control.fail_half(attachment, half);
        Ok(())
    }

    fn fail_next_opens(&mut self, term: &TermId, count: u32) -> Result<(), Unsupported> {
        self.control.fail_next_opens(term, count);
        Ok(())
    }

    fn max_reopen_attempts(&self) -> Option<u32> {
        Some(self.config.max_reopen_attempts)
    }

    async fn restart_backend(&mut self) -> Result<(), Unsupported> {
        self.control.restart_backend();
        Ok(())
    }

    async fn crash_backend(&mut self) -> Result<(), Unsupported> {
        self.control.crash_backend();
        Ok(())
    }

    fn set_late_old_generation_events(&mut self, on: bool) -> Result<(), Unsupported> {
        self.control.set_late_old_generation_events(on);
        Ok(())
    }

    fn late_events_delivered(&self) -> Result<(), Unsupported> {
        Ok(())
    }

    fn set_paint_after_resize(&mut self, on: bool) -> Result<(), Unsupported> {
        self.control.set_paint_after_resize(on);
        Ok(())
    }

    fn disable_byte_credit(&mut self) -> Result<(), Unsupported> {
        if self.backend_taken.get() {
            return Err(Unsupported("the backend is already in use"));
        }
        self.config.capabilities.byte_credit = false;
        let (backend, control) = FakeBackend::new(self.config.clone());
        self.backend = backend;
        self.control = control;
        Ok(())
    }
}
