//! Tauri commands and the event sink for host state.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};

use crate::hosts::model::HostsSnapshot;
use crate::hosts::service::{HostsService, SnapshotSink};

/// The event the webview listens on for every new hosts snapshot.
pub const HOSTS_EVENT: &str = "hosts-changed";

/// Publishes each snapshot to the webview as a `hosts-changed` event.
pub struct TauriSink(pub AppHandle);

impl SnapshotSink for TauriSink {
    fn publish(&self, snapshot: &HostsSnapshot) {
        if let Err(err) = self.0.emit(HOSTS_EVENT, snapshot) {
            eprintln!("could not emit {HOSTS_EVENT}: {err}");
        }
    }
}

/// Returns the current snapshot; the webview calls it once at startup, before any event.
#[tauri::command]
pub fn hosts_snapshot(service: State<'_, HostsService>) -> HostsSnapshot {
    service.snapshot()
}

/// Asks a host's supervisor to reconnect now.
#[tauri::command]
pub fn retry_host(service: State<'_, HostsService>, name: String) -> Result<(), String> {
    service.retry(&name).map_err(|err| err.to_string())
}

/// The sink the hosts service publishes through: each snapshot becomes a `hosts-changed` event.
pub fn sink(handle: AppHandle) -> Arc<dyn SnapshotSink> {
    Arc::new(TauriSink(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_name_matches_fixture() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../web/test/fixtures/ipc-contract.json"
        );
        let text = std::fs::read_to_string(path).expect("fixture is readable");
        let fixture: serde_json::Value = serde_json::from_str(&text).expect("fixture is JSON");
        assert_eq!(fixture["hostsEvent"], HOSTS_EVENT);
    }
}
