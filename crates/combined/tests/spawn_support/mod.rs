#![allow(dead_code)]
//! Test support for the spawn tests: panel files, a running server on the fake backend, and
//! readers for the replies.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use combine_protocol::{
    ClientMessage, DaemonMessage, ErrorCode, ErrorMessage, RequestId, SpawnSource, SpawnTerminal,
    Spawned, Templates, Terminals, WatchTerminals,
};
use combined::model::SpaceId;
use combined::ports::FileStore;
use combined::rpc::ServerHandle;
use combined::testing::{FakeBackend, FakeConfig, FakeControl, MemoryFileStore};
use serde_json::{json, Value};

use crate::daemon_support::{
    refuse_default_herdr_core, seed_project_terminal, server_on, work_dir, DaemonPaths,
    ShortTempDir, WireClient,
};

/// A server on the fake backend with one terminal in the project, so the project is a space.
pub struct Rig {
    pub server: ServerHandle,
    pub backend: Arc<FakeBackend>,
    pub control: FakeControl,
    pub store: Arc<dyn FileStore>,
    pub space: SpaceId,
    pub socket: std::path::PathBuf,
    pub scratch: ShortTempDir,
}

impl Rig {
    pub fn new(store: Arc<dyn FileStore>) -> Rig {
        let scratch = ShortTempDir::new();
        let (backend, control) = FakeBackend::new(FakeConfig::default());
        let work = work_dir(&scratch);
        fs::create_dir_all(work.join(".git")).unwrap();
        seed_project_terminal(&backend, &scratch);
        let server = server_on(&scratch, "d", Arc::clone(&backend), Arc::clone(&store));
        let socket = DaemonPaths::in_scratch(&scratch, "d").socket;
        refuse_default_herdr_core(&socket, &scratch);
        Rig {
            server,
            backend,
            control,
            store,
            space: SpaceId(work.to_str().unwrap().to_string()),
            socket,
            scratch,
        }
    }

    pub fn in_memory() -> (Rig, Arc<MemoryFileStore>) {
        let store = Arc::new(MemoryFileStore::new());
        (Rig::new(store.clone()), store)
    }

    pub fn client(&self) -> WireClient {
        WireClient::open(&self.socket).0
    }

    pub fn name(&self) -> String {
        self.space.0.clone()
    }

    pub fn list(&self, client: &mut WireClient, request: u64) -> Templates {
        client.send(&ClientMessage::ListTemplates(
            combine_protocol::ListTemplates {
                request: RequestId(request),
                space: self.name(),
            },
        ));
        expect_templates(client, request)
    }

    pub fn spawn(&self, client: &mut WireClient, request: u64, source: SpawnSource) {
        client.send(&ClientMessage::SpawnTerminal(SpawnTerminal {
            request: RequestId(request),
            space: self.name(),
            source,
        }));
    }

    pub fn spawn_template(&self, client: &mut WireClient, request: u64, panel: &str) {
        self.spawn(
            client,
            request,
            SpawnSource::Template {
                panel: panel.to_string(),
            },
        );
    }
}

pub fn watch_terminals(client: &mut WireClient, request: u64) -> Terminals {
    client.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(request),
    }));
    loop {
        if let DaemonMessage::Terminals(terminals) = client.recv() {
            break terminals;
        }
    }
}

/// The JSON of a terminal panel running `argv` in `cwd` (the project root when `None`).
pub fn terminal_panel_json(argv: &[&str], cwd: Option<&str>) -> Vec<u8> {
    let mut template = json!({ "argv": argv });
    if let Some(cwd) = cwd {
        template["cwd"] = json!(cwd);
    }
    panel_json_of(json!({
        "version": 1,
        "content": { "type": "terminal", "slot": "main", "template": template }
    }))
}

pub fn panel_json_of(value: Value) -> Vec<u8> {
    value.to_string().into_bytes()
}

pub fn write_panel_fs(work: &Path, panel: &str, bytes: &[u8]) {
    let dir = work.join(".workspace/panels").join(panel);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("panel.json"), bytes).unwrap();
}

pub fn write_panel_memory(store: &MemoryFileStore, space: &SpaceId, panel: &str, bytes: &[u8]) {
    store.write_outside(space, &format!("panels/{panel}/panel.json"), bytes.to_vec());
}

/// How many times the fake backend was asked to start a terminal, refused attempts included.
pub fn spawn_attempts(control: &FakeControl) -> usize {
    control.spawn_specs().len()
}

/// The next `templates` message, which must answer `request`. Pushes are skipped.
pub fn expect_templates(client: &WireClient, request: u64) -> Templates {
    loop {
        match client.recv() {
            DaemonMessage::Templates(templates) => {
                assert_eq!(templates.request, RequestId(request));
                return templates;
            }
            DaemonMessage::Terminals(_) => {}
            other => panic!("expected templates, got {other:?}"),
        }
    }
}

/// The next `spawned` message, which must answer `request`. Pushes are skipped.
pub fn expect_spawned(client: &WireClient, request: u64) -> Spawned {
    loop {
        match client.recv() {
            DaemonMessage::Spawned(spawned) => {
                assert_eq!(spawned.request, RequestId(request));
                return spawned;
            }
            DaemonMessage::Terminals(_) => {}
            other => panic!("expected spawned, got {other:?}"),
        }
    }
}

/// The next error, which must carry `code` and answer `request`. Pushes are skipped.
pub fn expect_error(client: &WireClient, request: u64, code: ErrorCode) -> ErrorMessage {
    loop {
        match client.recv() {
            DaemonMessage::Error(error) => {
                assert_eq!(error.code, code, "{}", error.message);
                assert_eq!(error.request, Some(RequestId(request)));
                return error;
            }
            DaemonMessage::Terminals(_) => {}
            other => panic!("expected an error, got {other:?}"),
        }
    }
}
