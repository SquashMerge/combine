#![cfg(feature = "test-support")]
//! The handle a test uses to drive and observe the fake.

use combine_protocol::{Display, Fidelity, TermId};

use crate::model::SpawnSpec;
use crate::ports::{BackendError, Half};

use super::world::{lock, Shared};
use super::{FakeAttachmentRef, FakeCall};

/// Drives the program in each fake terminal, injects faults and reports what the fake was
/// asked to do. Clones share one world.
#[derive(Clone)]
pub struct FakeControl {
    world: Shared,
}

impl FakeControl {
    pub(crate) fn new(world: Shared) -> Self {
        FakeControl { world }
    }

    // program and terminals

    /// The program prints `bytes`.
    pub fn emit(&self, term: &TermId, bytes: &[u8]) {
        lock(&self.world).emit(term, bytes);
    }

    pub fn exit(&self, term: &TermId, exit_code: Option<i32>) {
        lock(&self.world).exit(term, exit_code);
    }

    pub fn set_cwd(&self, term: &TermId, cwd: &str) {
        lock(&self.world).set_cwd(term, cwd);
    }

    pub fn set_fidelity(&self, term: &TermId, fidelity: Fidelity) {
        lock(&self.world).set_fidelity(term, fidelity);
    }

    /// The same terminal as another machine has it: `fake:work/<local address>`.
    pub fn remote_twin(&self, local: &TermId) -> TermId {
        lock(&self.world).remote_twin(local)
    }

    // observation

    pub fn pty_size(&self, term: &TermId) -> Option<(u16, u16)> {
        lock(&self.world).pty_size(term)
    }

    pub fn typed_input(&self, term: &TermId) -> Vec<u8> {
        lock(&self.world).typed_input(term)
    }

    pub fn automatic_input(&self, term: &TermId) -> Vec<Vec<u8>> {
        lock(&self.world).automatic_input(term)
    }

    pub fn calls(&self) -> Vec<FakeCall> {
        lock(&self.world).calls()
    }

    /// Every spawn the fake was asked for, in order, refused ones included.
    pub fn spawn_specs(&self) -> Vec<SpawnSpec> {
        lock(&self.world).spawn_specs()
    }

    /// In attach order, closed ones included.
    pub fn attachments(&self, term: &TermId) -> Vec<FakeAttachmentRef> {
        lock(&self.world).attachments_of(term)
    }

    pub fn open_halves(&self, term: &TermId) -> usize {
        lock(&self.world).open_halves(term)
    }

    /// `Data` bytes produced but not yet released to a viewer.
    pub fn pending_output(&self, term: &TermId) -> usize {
        lock(&self.world).pending_output(term)
    }

    /// The size and display the attachment last asked for. `None` for a ref whose attach
    /// failed, which never opened an attachment.
    pub fn requested(&self, att: FakeAttachmentRef) -> Option<((u16, u16), Display)> {
        lock(&self.world).requested(att)
    }

    /// World events the adapter's filter dropped as another machine's.
    pub fn filtered_events(&self) -> usize {
        lock(&self.world).filtered_events()
    }

    // faults

    /// Holds the terminal's output back, in order, until `release_output`.
    pub fn delay_output(&self, term: &TermId) {
        lock(&self.world).delay_output(term);
    }

    pub fn release_output(&self, term: &TermId) {
        lock(&self.world).release_output(term);
    }

    pub fn force_resync(&self, att: FakeAttachmentRef) {
        lock(&self.world).force_resync(att);
    }

    pub fn fail_half(&self, att: FakeAttachmentRef, half: Half) {
        lock(&self.world).fail_half(att, half);
    }

    /// The next `count` opens of the terminal fail halfway.
    pub fn fail_next_opens(&self, term: &TermId, count: u32) {
        lock(&self.world).fail_next_opens(term, count);
    }

    /// The next spawn fails with `error`: no terminal, no event and no `FakeCall::Spawn`.
    pub fn fail_next_spawn(&self, error: BackendError) {
        lock(&self.world).fail_next_spawn(error);
    }

    pub fn restart_backend(&self) {
        lock(&self.world).restart_backend();
    }

    pub fn crash_backend(&self) {
        lock(&self.world).crash_backend();
    }

    pub fn set_late_old_generation_events(&self, on: bool) {
        lock(&self.world).set_late_old_generation_events(on);
    }

    pub fn set_paint_after_resize(&self, on: bool) {
        lock(&self.world).set_paint_after_resize(on);
    }

    /// One reopen attempt for each reopening attachment, in id order. True if any happened.
    pub fn step(&self) -> bool {
        lock(&self.world).step()
    }
}
