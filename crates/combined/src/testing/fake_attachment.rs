#![cfg(feature = "test-support")]
//! One viewer of a fake terminal.

use std::pin::Pin;
use std::task::{Context, Poll};

use async_trait::async_trait;
use combine_protocol::{BackendGeneration, Display, Fidelity, OwnershipGeneration};
use futures::stream::{Stream, StreamExt};

use super::world::{lock, Shared};
use crate::ports::{AttachEvent, AttachEvents, Attachment, BackendResult};

pub(crate) struct FakeAttachment {
    world: Shared,
    id: u64,
    events_taken: bool,
}

impl FakeAttachment {
    pub(crate) fn new(world: Shared, id: u64) -> Self {
        FakeAttachment {
            world,
            id,
            events_taken: false,
        }
    }
}

struct EventStream {
    world: Shared,
    id: u64,
}

impl Stream for EventStream {
    type Item = (BackendGeneration, AttachEvent);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match lock(&self.world).pop_event(self.id, cx.waker()) {
            Some(item) => Poll::Ready(item),
            None => Poll::Pending,
        }
    }
}

impl Drop for FakeAttachment {
    fn drop(&mut self) {
        lock(&self.world).close(self.id, None);
    }
}

#[async_trait]
impl Attachment for FakeAttachment {
    fn fidelity(&self) -> Fidelity {
        lock(&self.world).fidelity(self.id)
    }

    fn ordered_resizes(&self) -> bool {
        lock(&self.world).ordered_resizes()
    }

    fn events(&mut self) -> AttachEvents {
        if self.events_taken {
            return futures::stream::empty().boxed();
        }
        self.events_taken = true;
        EventStream {
            world: self.world.clone(),
            id: self.id,
        }
        .boxed()
    }

    async fn write(&mut self, ownership: OwnershipGeneration, bytes: &[u8]) -> BackendResult<()> {
        lock(&self.world).write(self.id, ownership, bytes)
    }

    async fn write_automatic(
        &mut self,
        generation: BackendGeneration,
        bytes: &[u8],
    ) -> BackendResult<()> {
        lock(&self.world).write_automatic(self.id, generation, bytes)
    }

    async fn resize(
        &mut self,
        ownership: OwnershipGeneration,
        cols: u16,
        rows: u16,
    ) -> BackendResult<()> {
        lock(&self.world).resize(self.id, ownership, cols, rows)
    }

    async fn focus(&mut self, ownership: OwnershipGeneration) -> BackendResult<()> {
        lock(&self.world).focus(self.id, ownership)
    }

    async fn set_display(
        &mut self,
        ownership: OwnershipGeneration,
        display: Display,
    ) -> BackendResult<()> {
        lock(&self.world).set_display(self.id, ownership, display)
    }

    async fn credit(&mut self, bytes: u32) -> BackendResult<()> {
        lock(&self.world).credit(self.id, bytes)
    }

    async fn detach(self: Box<Self>) -> BackendResult<()> {
        lock(&self.world).detach(self.id);
        Ok(())
    }
}
