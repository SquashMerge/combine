#![cfg(feature = "test-support")]
//! The fake [`TerminalBackend`]: the adapter layer over the world. It filters other machines'
//! terminals out of `list` and `watch` and rejects them in `attach` and `kill`.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_trait::async_trait;
use combine_protocol::TermId;
use futures::stream::{BoxStream, Stream, StreamExt};

use super::fake_attachment::FakeAttachment;
use super::world::{address_of, is_remote, lock, Shared, World, ID_PREFIX};
use super::{FakeConfig, FakeControl};
use crate::model::{SpawnSpec, TermEvent, TermInfo};
use crate::ports::{
    AttachOptions, Attachment, BackendError, BackendResult, Capabilities, TerminalBackend,
};

/// An in-memory [`TerminalBackend`]; see [`FakeConfig`] for what it simulates.
pub struct FakeBackend {
    world: Shared,
}

impl FakeBackend {
    /// The backend and the handle that drives it. The backend's `id()` is `"fake"`.
    pub fn new(config: FakeConfig) -> (Arc<FakeBackend>, FakeControl) {
        let world = Arc::new(Mutex::new(World::new(config)));
        let backend = Arc::new(FakeBackend {
            world: Arc::clone(&world),
        });
        (backend, FakeControl::new(world))
    }

    /// The address of a live terminal on this machine, or why the call can't have it.
    fn resolve(&self, id: &TermId) -> BackendResult<String> {
        let address = address_of(id).ok_or_else(|| BackendError::UnknownTerminal(id.clone()))?;
        if is_remote(address) {
            return Err(BackendError::RemoteTerminal(id.clone()));
        }
        if lock(&self.world).live_local(address).is_none() {
            return Err(BackendError::UnknownTerminal(id.clone()));
        }
        Ok(address.to_string())
    }
}

struct Watch {
    world: Shared,
    subscriber: usize,
}

impl Stream for Watch {
    type Item = TermEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<TermEvent>> {
        match lock(&self.world).poll_subscriber(self.subscriber, cx.waker()) {
            Some(event) => Poll::Ready(Some(event)),
            None => Poll::Pending,
        }
    }
}

#[async_trait]
impl TerminalBackend for FakeBackend {
    fn id(&self) -> &'static str {
        ID_PREFIX
    }

    fn capabilities(&self) -> Capabilities {
        lock(&self.world).capabilities()
    }

    async fn list(&self) -> BackendResult<Vec<TermInfo>> {
        Ok(lock(&self.world).list())
    }

    async fn watch(&self) -> BackendResult<BoxStream<'static, TermEvent>> {
        let subscriber = lock(&self.world).subscribe();
        Ok(Watch {
            world: Arc::clone(&self.world),
            subscriber,
        }
        .boxed())
    }

    async fn spawn(&self, spec: SpawnSpec) -> BackendResult<TermId> {
        lock(&self.world).spawn(spec)
    }

    async fn attach(&self, id: &TermId, opts: AttachOptions) -> BackendResult<Box<dyn Attachment>> {
        let address = self.resolve(id)?;
        let reference = lock(&self.world).attach(&address, opts)?;
        Ok(Box::new(FakeAttachment::new(
            Arc::clone(&self.world),
            reference.0,
        )))
    }

    async fn kill(&self, id: &TermId) -> BackendResult<()> {
        let address = self.resolve(id)?;
        lock(&self.world).kill(&address);
        Ok(())
    }
}
