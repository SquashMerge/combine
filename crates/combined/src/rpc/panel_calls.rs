//! The panel API's seam: calls queued to the server loop, answered from it.

use tokio::sync::{mpsc, oneshot};

use crate::core::{PanelApiError, PanelCreate, PanelList, PanelUpdate, PanelView};
use crate::model::SpaceId;

/// One call to the panel API, with where its answer goes. The server runs it between two wakes
/// and sends the answer after the call's file work has run.
pub enum PanelCall {
    Create {
        space: SpaceId,
        create: PanelCreate,
        reply: oneshot::Sender<Result<PanelView, PanelApiError>>,
    },
    Update {
        space: SpaceId,
        id: String,
        update: PanelUpdate,
        reply: oneshot::Sender<Result<PanelView, PanelApiError>>,
    },
    Remove {
        space: SpaceId,
        id: String,
        reply: oneshot::Sender<Result<PanelView, PanelApiError>>,
    },
    Close {
        space: SpaceId,
        id: String,
        reply: oneshot::Sender<Result<PanelView, PanelApiError>>,
    },
    Pin {
        space: SpaceId,
        id: String,
        reply: oneshot::Sender<Result<PanelView, PanelApiError>>,
    },
    List {
        space: SpaceId,
        reply: oneshot::Sender<Result<PanelList, PanelApiError>>,
    },
    /// Answers with the panel, without its source.
    Status {
        space: SpaceId,
        id: String,
        reply: oneshot::Sender<Result<PanelView, PanelApiError>>,
    },
    /// Answers with the panel and its served source.
    Get {
        space: SpaceId,
        id: String,
        reply: oneshot::Sender<Result<(PanelView, String), PanelApiError>>,
    },
}

/// A way to call the panel API of a running server. Every method blocks until the server
/// answers: call from outside a runtime.
#[derive(Clone)]
pub struct PanelHandle(mpsc::Sender<PanelCall>);

fn stopped() -> PanelApiError {
    PanelApiError::Stopped
}

impl PanelHandle {
    pub(super) fn new(calls: mpsc::Sender<PanelCall>) -> PanelHandle {
        PanelHandle(calls)
    }

    fn call<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, PanelApiError>>) -> PanelCall,
    ) -> Result<T, PanelApiError> {
        let (reply, answer) = oneshot::channel();
        self.0.blocking_send(make(reply)).map_err(|_| stopped())?;
        answer.blocking_recv().map_err(|_| stopped())?
    }

    pub fn create(&self, space: &SpaceId, create: PanelCreate) -> Result<PanelView, PanelApiError> {
        self.call(|reply| PanelCall::Create {
            space: space.clone(),
            create,
            reply,
        })
    }

    pub fn update(
        &self,
        space: &SpaceId,
        id: &str,
        update: PanelUpdate,
    ) -> Result<PanelView, PanelApiError> {
        self.call(|reply| PanelCall::Update {
            space: space.clone(),
            id: id.to_owned(),
            update,
            reply,
        })
    }

    pub fn remove(&self, space: &SpaceId, id: &str) -> Result<PanelView, PanelApiError> {
        self.call(|reply| PanelCall::Remove {
            space: space.clone(),
            id: id.to_owned(),
            reply,
        })
    }

    pub fn close(&self, space: &SpaceId, id: &str) -> Result<PanelView, PanelApiError> {
        self.call(|reply| PanelCall::Close {
            space: space.clone(),
            id: id.to_owned(),
            reply,
        })
    }

    pub fn pin(&self, space: &SpaceId, id: &str) -> Result<PanelView, PanelApiError> {
        self.call(|reply| PanelCall::Pin {
            space: space.clone(),
            id: id.to_owned(),
            reply,
        })
    }

    pub fn list(&self, space: &SpaceId) -> Result<PanelList, PanelApiError> {
        self.call(|reply| PanelCall::List {
            space: space.clone(),
            reply,
        })
    }

    /// The panel as it is now, without its source: a read that changes nothing.
    pub fn status(&self, space: &SpaceId, id: &str) -> Result<PanelView, PanelApiError> {
        self.call(|reply| PanelCall::Status {
            space: space.clone(),
            id: id.to_owned(),
            reply,
        })
    }

    pub fn get(&self, space: &SpaceId, id: &str) -> Result<(PanelView, String), PanelApiError> {
        self.call(|reply| PanelCall::Get {
            space: space.clone(),
            id: id.to_owned(),
            reply,
        })
    }
}
