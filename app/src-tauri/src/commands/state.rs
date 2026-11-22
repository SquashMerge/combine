//! Tauri commands for the per-client state files. The DTOs carry `selected_at_ms` as a `DecimalU64`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;

use crate::hosts::model::DecimalU64;
use crate::state::model::{ClientSpaceState, ClientStateFile, LastSpace};
use crate::state::service::{ClientStateView, ClientStates};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientStateDto {
    pub version: u32,
    pub last_space: Option<LastSpaceDto>,
    pub spaces: BTreeMap<String, ClientSpaceStateDto>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastSpaceDto {
    pub space_id: String,
    pub selected_at_ms: DecimalU64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientSpaceStateDto {
    pub space_id: String,
    pub focused_tile: Option<String>,
    pub active_terminal: Option<String>,
    pub active_tabs: BTreeMap<String, String>,
    pub group_state: Value,
}

impl From<ClientSpaceState> for ClientSpaceStateDto {
    fn from(s: ClientSpaceState) -> Self {
        ClientSpaceStateDto {
            space_id: s.space_id,
            focused_tile: s.focused_tile,
            active_terminal: s.active_terminal,
            active_tabs: s.active_tabs,
            group_state: s.group_state,
        }
    }
}

impl From<ClientSpaceStateDto> for ClientSpaceState {
    fn from(s: ClientSpaceStateDto) -> Self {
        ClientSpaceState {
            space_id: s.space_id,
            focused_tile: s.focused_tile,
            active_terminal: s.active_terminal,
            active_tabs: s.active_tabs,
            group_state: s.group_state,
        }
    }
}

impl From<ClientStateView> for ClientStateDto {
    fn from(view: ClientStateView) -> Self {
        let ClientStateFile {
            version,
            last_space,
            spaces,
        } = view.file;
        ClientStateDto {
            version,
            last_space: last_space.map(|l| LastSpaceDto {
                space_id: l.space_id,
                selected_at_ms: DecimalU64(l.selected_at_ms),
            }),
            spaces: spaces.into_iter().map(|(k, v)| (k, v.into())).collect(),
            error: view.error,
        }
    }
}

/// The saved state for a host: defaults, plus the error to show, when its file is invalid.
#[tauri::command]
pub fn client_state(
    states: State<'_, ClientStates>,
    host_id: String,
) -> Result<ClientStateDto, String> {
    states
        .client_state(&host_id)
        .map(ClientStateDto::from)
        .map_err(|err| err.to_string())
}

/// Stores one space's state under its path. The write is not debounced.
#[tauri::command]
pub fn set_client_space_state(
    states: State<'_, ClientStates>,
    host_id: String,
    path: String,
    state: ClientSpaceStateDto,
) -> Result<(), String> {
    states
        .set_space_state(&host_id, &path, state.into())
        .map_err(|err| err.to_string())
}

/// Records which space the user selected last, and when.
#[tauri::command]
pub fn set_last_space(
    states: State<'_, ClientStates>,
    host_id: String,
    space_id: String,
    selected_at_ms: DecimalU64,
) -> Result<(), String> {
    states
        .set_last_space(
            &host_id,
            LastSpace {
                space_id,
                selected_at_ms: selected_at_ms.0,
            },
        )
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dto_matches_fixture() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../web/test/fixtures/client-state.json"
        );
        let text = std::fs::read_to_string(path).expect("fixture is readable");
        let fixture: Value = serde_json::from_str(&text).expect("fixture is JSON");
        let dto = ClientStateDto {
            version: 1,
            last_space: Some(LastSpaceDto {
                space_id: "space-1".to_string(),
                selected_at_ms: DecimalU64(18_446_744_073_709_551_615),
            }),
            spaces: BTreeMap::from([(
                "/work/a".to_string(),
                ClientSpaceStateDto {
                    space_id: "space-1".to_string(),
                    focused_tile: Some("t1".to_string()),
                    active_terminal: Some("7".to_string()),
                    active_tabs: BTreeMap::from([("t1".to_string(), "p2".to_string())]),
                    group_state: serde_json::json!({}),
                },
            )]),
            error: None,
        };
        assert_eq!(serde_json::to_value(&dto).expect("serialize"), fixture);
    }
}
