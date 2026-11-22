//! The client state files of every host, loaded on first use.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use super::model::{ClientStateFile, LastSpace, SpaceState};
use super::path::{client_state_path, InvalidHostId};
use super::store::{ClientStateStore, LoadOutcome, StateError};

/// A state file's contents and, when it was invalid, the error to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientStateView {
    pub file: ClientStateFile,
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum ServiceError {
    HostId(InvalidHostId),
    State(StateError),
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceError::HostId(e) => e.fmt(f),
            ServiceError::State(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for ServiceError {}

struct Entry {
    store: Arc<ClientStateStore>,
    error: Option<String>,
}

pub struct ClientStates {
    data_dir: PathBuf,
    entries: Mutex<HashMap<String, Arc<Entry>>>,
}

impl ClientStates {
    pub fn new(data_dir: PathBuf) -> ClientStates {
        ClientStates {
            data_dir,
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn entry(&self, host_id: &str) -> Result<Arc<Entry>, ServiceError> {
        let path = client_state_path(&self.data_dir, host_id).map_err(ServiceError::HostId)?;
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = entries.get(host_id) {
            return Ok(Arc::clone(entry));
        }
        let (store, outcome) = ClientStateStore::load(path);
        let error = match outcome {
            LoadOutcome::Invalid(message) => Some(message),
            LoadOutcome::Missing | LoadOutcome::Loaded => None,
        };
        let entry = Arc::new(Entry {
            store: Arc::new(store),
            error,
        });
        entries.insert(host_id.to_string(), Arc::clone(&entry));
        Ok(entry)
    }

    pub fn client_state(&self, host_id: &str) -> Result<ClientStateView, ServiceError> {
        let entry = self.entry(host_id)?;
        Ok(ClientStateView {
            file: entry.store.snapshot(),
            error: entry.error.clone(),
        })
    }

    pub fn set_space_state(
        &self,
        host_id: &str,
        key: &str,
        state: SpaceState,
    ) -> Result<(), ServiceError> {
        let entry = self.entry(host_id)?;
        entry
            .store
            .set_space_state(key, state)
            .map_err(ServiceError::State)
    }

    pub fn set_last_space(&self, host_id: &str, last: LastSpace) -> Result<(), ServiceError> {
        let entry = self.entry(host_id)?;
        entry
            .store
            .set_last_space(last)
            .map_err(ServiceError::State)
    }
}
