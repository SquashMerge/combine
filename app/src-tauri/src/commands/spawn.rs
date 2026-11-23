//! Tauri commands and the event sink for starting terminals: the templates a space offers, and
//! spawning a shell or a template. Request IDs cross as `DecimalU64`.
//!
//! What a template starts (argv, env, cwd and the panel it belongs to) crosses unchanged,
//! because it is exactly what the user's click approves. Only labels are trimmed.

use std::collections::BTreeMap;
use std::sync::Arc;

use combine_protocol::{
    ClientMessage, ErrorCode, HostId, ListTemplates, SkippedPanel, SpawnSource, SpawnTerminal,
    TemplateInfo, Templates,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::hosts::model::DecimalU64;
use crate::hosts::outbound::{Outbound, RequestKind};
use crate::hosts::service::HostsService;
use crate::hosts::spawn::{SpawnReply, SpawnSink};
use crate::hosts::text::{display_text, is_blank};

/// The event the webview listens on for every template list, started terminal, refusal and
/// disconnect.
pub const SPAWN_EVENT: &str = "spawn-reply";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcTemplate {
    pub panel: String,
    pub title: Option<String>,
    pub slot: String,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: String,
}

impl From<TemplateInfo> for IpcTemplate {
    fn from(template: TemplateInfo) -> Self {
        IpcTemplate {
            panel: template.panel,
            title: template
                .title
                .filter(|title| !is_blank(title))
                .map(|title| display_text(&title)),
            slot: display_text(&template.slot),
            argv: template.argv,
            env: template.env,
            cwd: template.cwd,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcSkippedPanel {
    pub panel: String,
    pub reason: String,
}

impl From<SkippedPanel> for IpcSkippedPanel {
    fn from(skipped: SkippedPanel) -> Self {
        IpcSkippedPanel {
            panel: display_text(&skipped.panel),
            reason: display_text(&skipped.reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcTemplateList {
    pub space: String,
    pub templates: Vec<IpcTemplate>,
    pub skipped: Vec<IpcSkippedPanel>,
    pub error: Option<String>,
}

impl From<Templates> for IpcTemplateList {
    fn from(templates: Templates) -> Self {
        IpcTemplateList {
            space: templates.space,
            templates: templates.templates.into_iter().map(Into::into).collect(),
            skipped: templates.skipped.into_iter().map(Into::into).collect(),
            error: templates.error.map(|error| display_text(&error)),
        }
    }
}

/// What `spawn-reply` carries, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum IpcSpawnEvent {
    Templates {
        host_id: String,
        request: DecimalU64,
        list: IpcTemplateList,
    },
    Spawned {
        host_id: String,
        request: DecimalU64,
        space: String,
        terminal: String,
    },
    Error {
        host_id: String,
        request: DecimalU64,
        code: ErrorCode,
        message: String,
    },
    Disconnected {
        host_id: String,
    },
}

fn spawn_event(host_id: &HostId, reply: SpawnReply) -> IpcSpawnEvent {
    let host_id = host_id.0.clone();
    match reply {
        SpawnReply::Templates(templates) => IpcSpawnEvent::Templates {
            host_id,
            request: templates.request.into(),
            list: templates.into(),
        },
        SpawnReply::Spawned(spawned) => IpcSpawnEvent::Spawned {
            host_id,
            request: spawned.request.into(),
            space: spawned.space,
            terminal: spawned.terminal.0,
        },
        SpawnReply::Error {
            request,
            code,
            message,
        } => IpcSpawnEvent::Error {
            host_id,
            request: request.into(),
            code,
            message,
        },
        SpawnReply::Disconnected => IpcSpawnEvent::Disconnected { host_id },
    }
}

/// What a terminal starts, as the webview names it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IpcSpawnSource {
    Shell,
    Template { panel: String },
}

impl From<IpcSpawnSource> for SpawnSource {
    fn from(source: IpcSpawnSource) -> Self {
        match source {
            IpcSpawnSource::Shell => SpawnSource::Shell,
            IpcSpawnSource::Template { panel } => SpawnSource::Template { panel },
        }
    }
}

/// Publishes spawn traffic to the webview as `spawn-reply` events.
pub struct TauriSpawnSink(pub AppHandle);

impl SpawnSink for TauriSpawnSink {
    fn spawn_reply(&self, host_id: &HostId, reply: SpawnReply) {
        if let Err(err) = self.0.emit(SPAWN_EVENT, spawn_event(host_id, reply)) {
            eprintln!("could not emit {SPAWN_EVENT}: {err}");
        }
    }
}

pub fn spawn_sink(handle: AppHandle) -> Arc<dyn SpawnSink> {
    Arc::new(TauriSpawnSink(handle))
}

fn list_request(outbound: &Outbound, host_id: String, space: String) -> Result<DecimalU64, String> {
    outbound
        .send(&HostId(host_id), RequestKind::Spawn, |request| {
            ClientMessage::ListTemplates(ListTemplates { request, space })
        })
        .map(Into::into)
        .map_err(|err| err.to_string())
}

fn spawn_request(
    outbound: &Outbound,
    host_id: String,
    space: String,
    source: IpcSpawnSource,
) -> Result<DecimalU64, String> {
    outbound
        .send(&HostId(host_id), RequestKind::Spawn, |request| {
            ClientMessage::SpawnTerminal(SpawnTerminal {
                request,
                space,
                source: source.into(),
            })
        })
        .map(Into::into)
        .map_err(|err| err.to_string())
}

/// Asks a host for the terminal templates of a space's panels. The list arrives as a
/// `spawn-reply` event; the result is the request ID it carries.
#[tauri::command]
pub fn list_templates(
    service: State<'_, HostsService>,
    host_id: String,
    space: String,
) -> Result<DecimalU64, String> {
    list_request(&service.outbound(), host_id, space)
}

/// Starts a shell or a template's terminal in a space. Only the webview's invoke, which the
/// user's click makes, reaches this. The result is the request ID of the reply.
#[tauri::command]
pub fn spawn_terminal(
    service: State<'_, HostsService>,
    host_id: String,
    space: String,
    source: IpcSpawnSource,
) -> Result<DecimalU64, String> {
    spawn_request(&service.outbound(), host_id, space, source)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use combine_protocol::{RequestId, Spawned, TermId};
    use serde_json::{json, Value};

    use super::*;
    use crate::hosts::outbound::Outgoing;

    fn fixture(name: &str) -> Value {
        let path = format!("{}/../web/test/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(path).expect("fixture is readable");
        serde_json::from_str(&text).expect("fixture is JSON")
    }

    #[test]
    fn ipc_contract_names_match_fixture() {
        let contract = fixture("ipc-contract.json");
        assert_eq!(contract["spawnEvent"], SPAWN_EVENT);
        assert_eq!(
            contract["commands"]["listTemplates"],
            stringify!(list_templates)
        );
        assert_eq!(
            contract["commands"]["spawnTerminal"],
            stringify!(spawn_terminal)
        );
    }

    #[test]
    fn spawn_event_shapes_match_fixture() {
        let fixture = fixture("spawn-event.json");
        let host = HostId("host-1".to_string());
        let templates = Templates {
            request: RequestId(9_007_199_254_740_993),
            space: "/work".to_string(),
            templates: vec![TemplateInfo {
                panel: "claude".to_string(),
                title: Some("Claude".to_string()),
                slot: "claude".to_string(),
                argv: vec!["claude".to_string(), "--resume".to_string()],
                env: BTreeMap::from([("FOO".to_string(), "bar".to_string())]),
                cwd: "src".to_string(),
            }],
            skipped: vec![SkippedPanel {
                panel: "notes".to_string(),
                reason: "not a terminal panel".to_string(),
            }],
            error: None,
        };
        let events = [
            ("templates", SpawnReply::Templates(templates)),
            (
                "spawned",
                SpawnReply::Spawned(Spawned {
                    request: RequestId(8),
                    space: "/work".to_string(),
                    terminal: TermId("hc:t12".to_string()),
                }),
            ),
            (
                "error",
                SpawnReply::Error {
                    request: RequestId(9),
                    code: ErrorCode::CwdRefused,
                    message: "the template's cwd \"../x\" has a .. segment".to_string(),
                },
            ),
            ("disconnected", SpawnReply::Disconnected),
        ];
        for (name, reply) in events {
            assert_eq!(
                serde_json::to_value(spawn_event(&host, reply)).expect("json"),
                fixture[name],
                "{name}"
            );
        }
        let sources = fixture["sources"].as_array().expect("sources");
        let decoded: Vec<IpcSpawnSource> = sources
            .iter()
            .map(|source| serde_json::from_value(source.clone()).expect("source decodes"))
            .collect();
        assert_eq!(
            decoded,
            vec![
                IpcSpawnSource::Shell,
                IpcSpawnSource::Template {
                    panel: "claude".to_string()
                }
            ]
        );
    }

    #[test]
    fn sources_decode_and_unknown_kind_is_refused() {
        assert_eq!(
            SpawnSource::from(
                serde_json::from_value::<IpcSpawnSource>(json!({"kind": "shell"})).expect("shell")
            ),
            SpawnSource::Shell
        );
        assert_eq!(
            SpawnSource::from(
                serde_json::from_value::<IpcSpawnSource>(
                    json!({"kind": "template", "panel": "claude"})
                )
                .expect("template")
            ),
            SpawnSource::Template {
                panel: "claude".to_string()
            }
        );
        assert!(serde_json::from_value::<IpcSpawnSource>(json!({"kind": "explode"})).is_err());
        assert!(serde_json::from_value::<IpcSpawnSource>(json!({"kind": "template"})).is_err());
    }

    #[test]
    fn template_command_fields_are_exact_and_labels_are_trimmed() {
        let mut argv: Vec<String> = ["sh", "-c", "a\nb", "", "x\u{202E}y"]
            .iter()
            .map(|arg| arg.to_string())
            .collect();
        argv.push("z".repeat(300));
        let env = BTreeMap::from([("K".to_string(), "v\nw".to_string())]);
        let template = |title: Option<&str>| TemplateInfo {
            panel: "pa\nnel".to_string(),
            title: title.map(str::to_string),
            slot: "sl\not".to_string(),
            argv: argv.clone(),
            env: env.clone(),
            cwd: "dir with space".to_string(),
        };
        let list: IpcTemplateList = Templates {
            request: RequestId(1),
            space: "/work".to_string(),
            templates: vec![
                template(Some("T\nitle")),
                template(Some(" \n ")),
                template(None),
            ],
            skipped: vec![SkippedPanel {
                panel: "notes".to_string(),
                reason: "bad\nreason".to_string(),
            }],
            error: Some("broke\nit".to_string()),
        }
        .into();
        let first = &list.templates[0];
        assert_eq!(first.argv, argv);
        assert_eq!(first.env, env);
        assert_eq!(first.cwd, "dir with space");
        assert_eq!(first.panel, "pa\nnel");
        assert_eq!(first.slot, "sl ot");
        assert_eq!(first.title.as_deref(), Some("T itle"));
        assert_eq!(list.templates[1].title, None);
        assert_eq!(list.templates[2].title, None);
        assert_eq!(list.skipped[0].reason, "bad reason");
        assert_eq!(list.error.as_deref(), Some("broke it"));
    }

    #[test]
    fn commands_without_a_session_are_not_connected() {
        let outbound = Outbound::default();
        let not_connected = Err("not connected".to_string());
        assert_eq!(
            list_request(&outbound, "h".to_string(), "/work".to_string()),
            not_connected
        );
        assert_eq!(
            spawn_request(
                &outbound,
                "h".to_string(),
                "/work".to_string(),
                IpcSpawnSource::Shell
            ),
            not_connected
        );
    }

    #[test]
    fn spawn_sends_the_source_and_returns_the_request() {
        let outbound = Arc::new(Outbound::default());
        let host = HostId("h".to_string());
        let (tx, rx) = mpsc::channel();
        let _registration = outbound.register(&host, tx);
        let listed = list_request(&outbound, "h".to_string(), "/work".to_string()).expect("sent");
        let spawned = spawn_request(
            &outbound,
            "h".to_string(),
            "/work".to_string(),
            IpcSpawnSource::Template {
                panel: "claude".to_string(),
            },
        )
        .expect("sent");
        assert_eq!((listed, spawned), (DecimalU64(1), DecimalU64(2)));
        assert_eq!(
            rx.try_recv().ok(),
            Some(Outgoing::Message(ClientMessage::ListTemplates(
                ListTemplates {
                    request: RequestId(1),
                    space: "/work".to_string(),
                }
            )))
        );
        assert_eq!(
            rx.try_recv().ok(),
            Some(Outgoing::Message(ClientMessage::SpawnTerminal(
                SpawnTerminal {
                    request: RequestId(2),
                    space: "/work".to_string(),
                    source: SpawnSource::Template {
                        panel: "claude".to_string()
                    },
                }
            )))
        );
        assert_eq!(
            outbound.kind_of(&host, RequestId(2)),
            Some(RequestKind::Spawn)
        );
    }
}
