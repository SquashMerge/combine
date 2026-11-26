//! Tauri commands and the event for a space's panels: the list that rides the layout watch,
//! fetched panel sources, closing and pinning a panel, and the render status the webview reports back. Request IDs and
//! revisions cross as `DecimalU64`.
//!
//! Panel IDs, space IDs and panel sources cross unchanged, because the webview names them back
//! to the daemon and draws the source as written. An error's snippet is a piece of that source,
//! so it keeps its indentation too. Every other text from a daemon is trimmed.

use combine_protocol::{
    ClientMessage, ClosePanel, CreatedBy, ErrorCode, GetPanel, HostId, Panel, PanelError,
    PanelLifetime, PanelState, PanelSummary, Panels, PinPanel, RenderStatus, Renderer,
    ReportRender, SkippedPanel,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use super::spawn::IpcSkippedPanel;
use crate::hosts::model::DecimalU64;
use crate::hosts::outbound::{Outbound, RequestKind};
use crate::hosts::panels::PanelReply;
use crate::hosts::service::HostsService;
use crate::hosts::text::{display_text, is_blank};

/// The event the webview listens on for every panel list, fetched panel, refusal and
/// disconnect.
pub const PANELS_EVENT: &str = "panels-changed";

/// How much of a render error crosses to the daemon, so a report always fits one frame.
const REPORT_CODE_MAX_BYTES: usize = 64;
const REPORT_TEXT_MAX_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcPanelError {
    pub code: String,
    pub message: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub snippet: Option<String>,
}

impl From<PanelError> for IpcPanelError {
    fn from(error: PanelError) -> Self {
        IpcPanelError {
            code: display_text(&error.code),
            message: display_text(&error.message),
            line: error.line,
            column: error.column,
            snippet: error.snippet,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcCreatedBy {
    User,
    Agent { slot: Option<String> },
}

impl From<CreatedBy> for IpcCreatedBy {
    fn from(created_by: CreatedBy) -> Self {
        match created_by {
            CreatedBy::User => IpcCreatedBy::User,
            CreatedBy::Agent { slot } => IpcCreatedBy::Agent {
                slot: slot.map(|slot| display_text(&slot)),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcPanelSummary {
    pub id: String,
    pub title: Option<String>,
    pub renderer: Renderer,
    pub revision: DecimalU64,
    pub state: PanelState,
    pub error: Option<IpcPanelError>,
    pub created_by: Option<IpcCreatedBy>,
    pub lifetime: Option<PanelLifetime>,
}

impl From<PanelSummary> for IpcPanelSummary {
    fn from(summary: PanelSummary) -> Self {
        IpcPanelSummary {
            id: summary.id,
            title: summary
                .title
                .filter(|title| !is_blank(title))
                .map(|title| display_text(&title)),
            renderer: summary.renderer,
            revision: DecimalU64(summary.revision),
            state: summary.state,
            error: summary.error.map(Into::into),
            created_by: summary.created_by.map(Into::into),
            lifetime: summary.lifetime,
        }
    }
}

/// A skipped panel keeps its ID as the daemon wrote it, unlike the spawn list's copy.
fn skipped_raw(skipped: SkippedPanel) -> IpcSkippedPanel {
    IpcSkippedPanel {
        panel: skipped.panel,
        reason: display_text(&skipped.reason),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcPanelList {
    pub space: String,
    pub panels: Vec<IpcPanelSummary>,
    pub skipped: Vec<IpcSkippedPanel>,
    pub error: Option<String>,
}

impl From<Panels> for IpcPanelList {
    fn from(panels: Panels) -> Self {
        IpcPanelList {
            space: panels.space,
            panels: panels.panels.into_iter().map(Into::into).collect(),
            skipped: panels.skipped.into_iter().map(skipped_raw).collect(),
            error: panels.error.map(|error| display_text(&error)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcPanelContent {
    pub space: String,
    pub id: String,
    pub revision: DecimalU64,
    pub renderer: Renderer,
    pub source: String,
    pub state: PanelState,
    pub error: Option<IpcPanelError>,
}

impl From<Panel> for IpcPanelContent {
    fn from(panel: Panel) -> Self {
        IpcPanelContent {
            space: panel.space,
            id: panel.id,
            revision: DecimalU64(panel.revision),
            renderer: panel.renderer,
            source: panel.source,
            state: panel.state,
            error: panel.error.map(Into::into),
        }
    }
}

/// What `panels-changed` carries, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum IpcPanelsEvent {
    List {
        host_id: String,
        request: Option<DecimalU64>,
        list: IpcPanelList,
    },
    Panel {
        host_id: String,
        request: DecimalU64,
        panel: IpcPanelContent,
    },
    Closed {
        host_id: String,
        request: DecimalU64,
        space: String,
        panel: String,
    },
    Pinned {
        host_id: String,
        request: DecimalU64,
        space: String,
        panel: String,
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

pub fn list_event(host_id: &HostId, panels: Panels) -> IpcPanelsEvent {
    IpcPanelsEvent::List {
        host_id: host_id.0.clone(),
        request: panels.request.map(Into::into),
        list: panels.into(),
    }
}

pub fn reply_event(host_id: &HostId, reply: PanelReply) -> IpcPanelsEvent {
    let host_id = host_id.0.clone();
    match reply {
        PanelReply::Panel(panel) => IpcPanelsEvent::Panel {
            host_id,
            request: panel.request.into(),
            panel: panel.into(),
        },
        PanelReply::Closed(closed) => IpcPanelsEvent::Closed {
            host_id,
            request: closed.request.into(),
            space: closed.space,
            panel: closed.panel,
        },
        PanelReply::Pinned(pinned) => IpcPanelsEvent::Pinned {
            host_id,
            request: pinned.request.into(),
            space: pinned.space,
            panel: pinned.panel,
        },
        PanelReply::Error {
            request,
            code,
            message,
        } => IpcPanelsEvent::Error {
            host_id,
            request: request.into(),
            code,
            message,
        },
        PanelReply::Disconnected => IpcPanelsEvent::Disconnected { host_id },
    }
}

pub(super) fn emit(handle: &AppHandle, event: IpcPanelsEvent) {
    if let Err(err) = handle.emit(PANELS_EVENT, event) {
        eprintln!("could not emit {PANELS_EVENT}: {err}");
    }
}

/// What the webview reports about drawing one revision.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IpcRenderReport {
    pub space: String,
    pub panel: String,
    pub revision: DecimalU64,
    pub status: RenderStatus,
    pub error: Option<IpcPanelError>,
}

fn clip(text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let end = (0..=max_bytes)
        .rev()
        .find(|&index| text.is_char_boundary(index))
        .unwrap_or(0);
    text[..end].to_string()
}

impl From<IpcPanelError> for PanelError {
    fn from(error: IpcPanelError) -> Self {
        PanelError {
            code: clip(error.code, REPORT_CODE_MAX_BYTES),
            message: clip(error.message, REPORT_TEXT_MAX_BYTES),
            line: error.line,
            column: error.column,
            snippet: error
                .snippet
                .map(|snippet| clip(snippet, REPORT_TEXT_MAX_BYTES)),
        }
    }
}

fn get(
    outbound: &Outbound,
    host_id: String,
    space: String,
    panel: String,
) -> Result<DecimalU64, String> {
    outbound
        .send(&HostId(host_id), RequestKind::Panel, |request| {
            ClientMessage::GetPanel(GetPanel {
                request,
                space,
                panel,
            })
        })
        .map(Into::into)
        .map_err(|err| err.to_string())
}

fn close(
    outbound: &Outbound,
    host_id: String,
    space: String,
    panel: String,
) -> Result<DecimalU64, String> {
    outbound
        .send(&HostId(host_id), RequestKind::Panel, |request| {
            ClientMessage::ClosePanel(ClosePanel {
                request,
                space,
                panel,
            })
        })
        .map(Into::into)
        .map_err(|err| err.to_string())
}

fn pin(
    outbound: &Outbound,
    host_id: String,
    space: String,
    panel: String,
) -> Result<DecimalU64, String> {
    outbound
        .send(&HostId(host_id), RequestKind::Panel, |request| {
            ClientMessage::PinPanel(PinPanel {
                request,
                space,
                panel,
            })
        })
        .map(Into::into)
        .map_err(|err| err.to_string())
}

fn report(outbound: &Outbound, host_id: String, report: IpcRenderReport) -> Result<(), String> {
    let error = match report.status {
        RenderStatus::Rendered => None,
        RenderStatus::RenderFailed => report.error.map(Into::into),
    };
    outbound
        .send_untracked(
            &HostId(host_id),
            ClientMessage::ReportRender(ReportRender {
                space: report.space,
                panel: report.panel,
                revision: report.revision.0,
                status: report.status,
                error,
            }),
        )
        .map_err(|err| err.to_string())
}

/// Asks a host for one panel's source. The panel arrives as a `panels-changed` event; the
/// result is the request ID it carries.
#[tauri::command]
pub fn get_panel(
    service: State<'_, HostsService>,
    host_id: String,
    space: String,
    panel: String,
) -> Result<DecimalU64, String> {
    get(&service.outbound(), host_id, space, panel)
}

/// Asks a host to close a panel. The answer arrives as a `panels-changed` event; the result is
/// the request ID it carries.
#[tauri::command]
pub fn close_panel(
    service: State<'_, HostsService>,
    host_id: String,
    space: String,
    panel: String,
) -> Result<DecimalU64, String> {
    close(&service.outbound(), host_id, space, panel)
}

/// Asks a host to keep a scratch panel in the project. The answer arrives as a `panels-changed`
/// event; the result is the request ID it carries.
#[tauri::command]
pub fn pin_panel(
    service: State<'_, HostsService>,
    host_id: String,
    space: String,
    panel: String,
) -> Result<DecimalU64, String> {
    pin(&service.outbound(), host_id, space, panel)
}

/// Tells the host whether this client drew a panel's revision. The daemon doesn't answer.
#[tauri::command]
pub fn report_render(
    service: State<'_, HostsService>,
    host_id: String,
    report: IpcRenderReport,
) -> Result<(), String> {
    self::report(&service.outbound(), host_id, report)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::sync::Arc;

    use combine_protocol::{PanelClosed, PanelPinned, RequestId};
    use serde_json::{json, Value};

    use super::*;
    use crate::hosts::outbound::{Outgoing, Registration};

    fn fixture(name: &str) -> Value {
        let path = format!("{}/../web/test/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(path).expect("fixture is readable");
        serde_json::from_str(&text).expect("fixture is JSON")
    }

    fn summaries() -> Vec<PanelSummary> {
        vec![
            PanelSummary {
                id: "classes".to_string(),
                title: Some("Class diagram".to_string()),
                renderer: Renderer::Mermaid,
                revision: 9_007_199_254_740_993,
                state: PanelState::Failed,
                error: Some(PanelError {
                    code: "syntax".to_string(),
                    message: "unexpected token".to_string(),
                    line: Some(2),
                    column: Some(11),
                    snippet: Some("  A -> B".to_string()),
                }),
                created_by: Some(CreatedBy::Agent {
                    slot: Some("claude".to_string()),
                }),
                lifetime: Some(PanelLifetime::Persistent),
            },
            PanelSummary {
                id: "notes".to_string(),
                title: None,
                renderer: Renderer::Markdown,
                revision: 12,
                state: PanelState::Validated,
                error: None,
                created_by: Some(CreatedBy::User),
                lifetime: Some(PanelLifetime::Scratch),
            },
        ]
    }

    fn list(request: Option<u64>) -> Panels {
        Panels {
            request: request.map(RequestId),
            space: "/work".to_string(),
            panels: summaries(),
            skipped: vec![SkippedPanel {
                panel: "broken".to_string(),
                reason: "panel.json can't be used".to_string(),
            }],
            error: None,
        }
    }

    fn notes(request: u64) -> Panel {
        Panel {
            request: RequestId(request),
            space: "/work".to_string(),
            id: "notes".to_string(),
            revision: 12,
            renderer: Renderer::Markdown,
            source: "# Notes\n".to_string(),
            state: PanelState::Validated,
            error: None,
        }
    }

    #[test]
    fn ipc_contract_names_match_fixture() {
        let contract = fixture("ipc-contract.json");
        assert_eq!(contract["panelsEvent"], PANELS_EVENT);
        assert_eq!(contract["commands"]["getPanel"], stringify!(get_panel));
        assert_eq!(
            contract["commands"]["reportRender"],
            stringify!(report_render)
        );
        assert_eq!(contract["commands"]["closePanel"], stringify!(close_panel));
        assert_eq!(contract["commands"]["pinPanel"], stringify!(pin_panel));
    }

    #[test]
    fn panels_event_shapes_match_fixture() {
        let fixture = fixture("panels-event.json");
        let host = HostId("host-1".to_string());
        let loading = Panels {
            request: Some(RequestId(8)),
            space: "/work".to_string(),
            panels: Vec::new(),
            skipped: Vec::new(),
            error: Some("the space's panels are loading".to_string()),
        };
        let events = [
            ("list", list_event(&host, list(Some(7)))),
            ("push", list_event(&host, list(None))),
            ("loading", list_event(&host, loading)),
            ("panel", reply_event(&host, PanelReply::Panel(notes(9)))),
            (
                "error",
                reply_event(
                    &host,
                    PanelReply::Error {
                        request: RequestId(10),
                        code: ErrorCode::UnknownPanel,
                        message: "the space has no panel gone".to_string(),
                    },
                ),
            ),
            (
                "closed",
                reply_event(
                    &host,
                    PanelReply::Closed(PanelClosed {
                        request: RequestId(11),
                        space: "/work".to_string(),
                        panel: "notes".to_string(),
                    }),
                ),
            ),
            (
                "pinned",
                reply_event(
                    &host,
                    PanelReply::Pinned(PanelPinned {
                        request: RequestId(12),
                        space: "/work".to_string(),
                        panel: "notes".to_string(),
                    }),
                ),
            ),
            ("disconnected", reply_event(&host, PanelReply::Disconnected)),
        ];
        for (name, event) in events {
            assert_eq!(
                serde_json::to_value(event).expect("json"),
                fixture[name],
                "{name}"
            );
        }
        let mut names: Vec<&String> = fixture.as_object().expect("object").keys().collect();
        names.sort();
        assert_eq!(
            names,
            [
                "closed",
                "disconnected",
                "error",
                "list",
                "loading",
                "panel",
                "pinned",
                "push"
            ]
        );
    }

    #[test]
    fn titles_and_texts_are_trimmed_ids_and_sources_are_not() {
        let error = |text: &str| PanelError {
            code: format!("co\nde{text}"),
            message: format!("mes\nsage{text}"),
            line: None,
            column: None,
            snippet: Some(format!("sni\npet{text}")),
        };
        let summary = |title: Option<&str>| PanelSummary {
            id: "pa\nnel".to_string(),
            title: title.map(str::to_string),
            renderer: Renderer::Markdown,
            revision: 1,
            state: PanelState::Failed,
            error: Some(error("")),
            created_by: Some(CreatedBy::Agent {
                slot: Some("sl\not".to_string()),
            }),
            lifetime: None,
        };
        let panels: IpcPanelList = Panels {
            request: None,
            space: "/wo\nrk".to_string(),
            panels: vec![
                summary(Some("T\nitle")),
                summary(Some(" \n ")),
                summary(None),
            ],
            skipped: vec![SkippedPanel {
                panel: " bro\nken ".to_string(),
                reason: "bad\nreason".to_string(),
            }],
            error: Some("broke\nit".to_string()),
        }
        .into();
        assert_eq!(panels.space, "/wo\nrk");
        let first = &panels.panels[0];
        assert_eq!(first.id, "pa\nnel");
        assert_eq!(first.title.as_deref(), Some("T itle"));
        assert_eq!(panels.panels[1].title, None);
        assert_eq!(panels.panels[2].title, None);
        let error = first.error.as_ref().expect("error");
        assert_eq!(
            (error.code.as_str(), error.message.as_str()),
            ("co de", "mes sage")
        );
        assert_eq!(error.snippet.as_deref(), Some("sni\npet"));
        assert_eq!(
            first.created_by,
            Some(IpcCreatedBy::Agent {
                slot: Some("sl ot".to_string())
            })
        );
        assert_eq!(panels.skipped[0].panel, " bro\nken ");
        assert_eq!(panels.skipped[0].reason, "bad reason");
        assert_eq!(panels.error.as_deref(), Some("broke it"));
        let content: IpcPanelContent = Panel {
            source: "  # a\n\n\tb ".to_string(),
            id: "no\nte".to_string(),
            error: Some(error_of("x")),
            ..notes(1)
        }
        .into();
        assert_eq!(content.source, "  # a\n\n\tb ");
        assert_eq!(content.id, "no\nte");
    }

    fn error_of(text: &str) -> PanelError {
        PanelError {
            code: text.to_string(),
            message: text.to_string(),
            line: None,
            column: None,
            snippet: None,
        }
    }

    fn registered(outbound: &Arc<Outbound>) -> (mpsc::Receiver<Outgoing>, Registration) {
        let (tx, rx) = mpsc::channel();
        let registration = outbound.register(&HostId("h".to_string()), tx);
        (rx, registration)
    }

    #[test]
    fn commands_without_a_session_are_not_connected() {
        let outbound = Outbound::default();
        assert_eq!(
            get(
                &outbound,
                "h".to_string(),
                "/work".to_string(),
                "notes".to_string()
            ),
            Err("not connected".to_string())
        );
        let render = IpcRenderReport {
            space: "/work".to_string(),
            panel: "notes".to_string(),
            revision: DecimalU64(1),
            status: RenderStatus::Rendered,
            error: None,
        };
        assert_eq!(
            report(&outbound, "h".to_string(), render),
            Err("not connected".to_string())
        );
    }

    #[test]
    fn get_sends_the_panel_request_and_returns_its_id() {
        let outbound = Arc::new(Outbound::default());
        let (rx, _registration) = registered(&outbound);
        let id = get(
            &outbound,
            "h".to_string(),
            "/work".to_string(),
            "notes".to_string(),
        )
        .expect("sent");
        assert_eq!(id, DecimalU64(1));
        assert_eq!(
            rx.try_recv().ok(),
            Some(Outgoing::Message(ClientMessage::GetPanel(GetPanel {
                request: RequestId(1),
                space: "/work".to_string(),
                panel: "notes".to_string(),
            })))
        );
        assert_eq!(
            outbound.kind_of(&HostId("h".to_string()), RequestId(1)),
            Some(RequestKind::Panel)
        );
    }

    #[test]
    fn close_and_pin_send_their_requests() {
        let outbound = Arc::new(Outbound::default());
        let (rx, _registration) = registered(&outbound);
        let closed = close(
            &outbound,
            "h".to_string(),
            "/work".to_string(),
            "notes".to_string(),
        )
        .expect("sent");
        let pinned = pin(
            &outbound,
            "h".to_string(),
            "/work".to_string(),
            "notes".to_string(),
        )
        .expect("sent");
        assert_eq!((closed, pinned), (DecimalU64(1), DecimalU64(2)));
        assert_eq!(
            rx.try_recv().ok(),
            Some(Outgoing::Message(ClientMessage::ClosePanel(ClosePanel {
                request: RequestId(1),
                space: "/work".to_string(),
                panel: "notes".to_string(),
            })))
        );
        assert_eq!(
            rx.try_recv().ok(),
            Some(Outgoing::Message(ClientMessage::PinPanel(PinPanel {
                request: RequestId(2),
                space: "/work".to_string(),
                panel: "notes".to_string(),
            })))
        );
        for request in [RequestId(1), RequestId(2)] {
            assert_eq!(
                outbound.kind_of(&HostId("h".to_string()), request),
                Some(RequestKind::Panel)
            );
        }
    }

    fn failed(error: IpcPanelError) -> IpcRenderReport {
        IpcRenderReport {
            space: "/work".to_string(),
            panel: "classes".to_string(),
            revision: DecimalU64(9_007_199_254_740_993),
            status: RenderStatus::RenderFailed,
            error: Some(error),
        }
    }

    #[test]
    fn report_sends_untracked_and_drops_the_error_of_a_rendered_panel() {
        let outbound = Arc::new(Outbound::default());
        let (rx, _registration) = registered(&outbound);
        let mut rendered = failed(IpcPanelError {
            code: "x".to_string(),
            message: "y".to_string(),
            line: None,
            column: None,
            snippet: None,
        });
        rendered.status = RenderStatus::Rendered;
        report(&outbound, "h".to_string(), rendered).expect("sent");
        assert_eq!(
            rx.try_recv().ok(),
            Some(Outgoing::Message(ClientMessage::ReportRender(
                ReportRender {
                    space: "/work".to_string(),
                    panel: "classes".to_string(),
                    revision: 9_007_199_254_740_993,
                    status: RenderStatus::Rendered,
                    error: None,
                }
            )))
        );
        assert_eq!(
            outbound.kind_of(&HostId("h".to_string()), RequestId(1)),
            None
        );
    }

    #[test]
    fn report_clips_the_error_on_char_boundaries() {
        let outbound = Arc::new(Outbound::default());
        let (rx, _registration) = registered(&outbound);
        // A 3-byte char straddles each limit, so a byte cut would split it.
        let code = format!("{}\u{20AC}", "c".repeat(63));
        let text = format!("{}\u{20AC}", "m".repeat(1023));
        let short = IpcPanelError {
            code: "ok".to_string(),
            message: "fine".to_string(),
            line: Some(3),
            column: Some(4),
            snippet: None,
        };
        report(&outbound, "h".to_string(), failed(short)).expect("sent");
        let long = IpcPanelError {
            code,
            message: text.clone(),
            line: None,
            column: None,
            snippet: Some(text),
        };
        report(&outbound, "h".to_string(), failed(long)).expect("sent");
        let sent = |rx: &mpsc::Receiver<Outgoing>| match rx.try_recv() {
            Ok(Outgoing::Message(ClientMessage::ReportRender(report))) => {
                report.error.expect("error")
            }
            other => panic!("expected a report, got {other:?}"),
        };
        let first = sent(&rx);
        assert_eq!(
            (first.code.as_str(), first.message.as_str(), first.line),
            ("ok", "fine", Some(3))
        );
        let second = sent(&rx);
        assert_eq!(second.code, "c".repeat(63));
        assert_eq!(second.message, "m".repeat(1023));
        assert_eq!(second.snippet, Some("m".repeat(1023)));
    }

    #[test]
    fn render_reports_refuse_unknown_fields() {
        let ok = json!({
            "space": "/work", "panel": "notes", "revision": "12", "status": "rendered",
            "error": null
        });
        let decoded: IpcRenderReport = serde_json::from_value(ok.clone()).expect("decodes");
        assert_eq!(decoded.revision, DecimalU64(12));
        let mut extra = ok;
        extra["extra"] = json!(1);
        assert!(serde_json::from_value::<IpcRenderReport>(extra).is_err());
    }
}
