//! Tauri commands and the event sink for a space's layout. Request IDs and layout revisions
//! cross as `DecimalU64`.

use std::sync::Arc;

use combine_protocol::{
    ClientMessage, DroppedNotice, ErrorCode, HostId, LayoutNode, LayoutOp, LayoutSnapshot,
    MutateLayout, Panels, Ratio, RequestId, SplitDirection, UnwatchLayout, WatchLayout,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use super::panels;
use crate::hosts::model::DecimalU64;
use crate::hosts::outbound::{Outbound, RequestKind};
use crate::hosts::panels::PanelReply;
use crate::hosts::service::HostsService;
use crate::hosts::session::LayoutSink;

/// The event the webview listens on for every layout reply, push, error and disconnect.
pub const LAYOUT_EVENT: &str = "layout-changed";

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcLayoutNode {
    Split {
        dir: SplitDirection,
        ratio: f64,
        a: Box<IpcLayoutNode>,
        b: Box<IpcLayoutNode>,
    },
    Tile {
        id: String,
        panels: Vec<String>,
        active: Option<String>,
    },
}

impl From<LayoutNode> for IpcLayoutNode {
    fn from(node: LayoutNode) -> Self {
        match node {
            LayoutNode::Split { dir, ratio, a, b } => IpcLayoutNode::Split {
                dir,
                ratio: ratio.get(),
                a: Box::new((*a).into()),
                b: Box::new((*b).into()),
            },
            LayoutNode::Tile { id, panels, active } => IpcLayoutNode::Tile { id, panels, active },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcDroppedNotice {
    pub op_count: u32,
    pub message: String,
}

impl From<DroppedNotice> for IpcDroppedNotice {
    fn from(notice: DroppedNotice) -> Self {
        IpcDroppedNotice {
            op_count: notice.op_count,
            message: notice.message,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcLayoutSnapshot {
    pub space: String,
    pub layout_revision: DecimalU64,
    pub root: IpcLayoutNode,
    pub read_only: bool,
    pub error: Option<String>,
    pub dropped: Option<IpcDroppedNotice>,
}

impl From<LayoutSnapshot> for IpcLayoutSnapshot {
    fn from(snapshot: LayoutSnapshot) -> Self {
        IpcLayoutSnapshot {
            space: snapshot.space,
            layout_revision: DecimalU64(snapshot.layout_revision),
            root: snapshot.root.into(),
            read_only: snapshot.read_only,
            error: snapshot.error,
            dropped: snapshot.dropped.map(Into::into),
        }
    }
}

/// One layout change the webview asks for, tagged by `op`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum IpcLayoutOp {
    SetActive {
        tile: String,
        panel: String,
    },
    SetRatio {
        leaves: Vec<String>,
        ratio: f64,
    },
    MovePanel {
        panel: String,
        to_tile: String,
        index: u32,
    },
    SplitTile {
        tile: String,
        dir: SplitDirection,
        new_tile: String,
        ratio: f64,
        panel: Option<String>,
    },
    CloseTile {
        tile: String,
    },
}

fn ratio(value: f64) -> Result<Ratio, String> {
    Ratio::new(value).ok_or_else(|| "a ratio must be finite".to_string())
}

impl TryFrom<IpcLayoutOp> for LayoutOp {
    type Error = String;

    fn try_from(op: IpcLayoutOp) -> Result<LayoutOp, String> {
        Ok(match op {
            IpcLayoutOp::SetActive { tile, panel } => LayoutOp::SetActive { tile, panel },
            IpcLayoutOp::SetRatio {
                leaves,
                ratio: value,
            } => LayoutOp::SetRatio {
                leaves,
                ratio: ratio(value)?,
            },
            IpcLayoutOp::MovePanel {
                panel,
                to_tile,
                index,
            } => LayoutOp::MovePanel {
                panel,
                to_tile,
                index,
            },
            IpcLayoutOp::SplitTile {
                tile,
                dir,
                new_tile,
                ratio: value,
                panel,
            } => LayoutOp::SplitTile {
                tile,
                dir,
                new_tile,
                ratio: ratio(value)?,
                panel,
            },
            IpcLayoutOp::CloseTile { tile } => LayoutOp::CloseTile { tile },
        })
    }
}

/// What `layout-changed` carries, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum IpcLayoutEvent {
    Snapshot {
        host_id: String,
        request: Option<DecimalU64>,
        snapshot: IpcLayoutSnapshot,
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

/// Publishes layout traffic to the webview as `layout-changed` events.
pub struct TauriLayoutSink(pub AppHandle);

fn snapshot_event(
    host_id: &HostId,
    request: Option<RequestId>,
    snapshot: LayoutSnapshot,
) -> IpcLayoutEvent {
    IpcLayoutEvent::Snapshot {
        host_id: host_id.0.clone(),
        request: request.map(Into::into),
        snapshot: snapshot.into(),
    }
}

fn error_event(
    host_id: &HostId,
    request: RequestId,
    code: ErrorCode,
    message: String,
) -> IpcLayoutEvent {
    IpcLayoutEvent::Error {
        host_id: host_id.0.clone(),
        request: request.into(),
        code,
        message,
    }
}

fn disconnected_event(host_id: &HostId) -> IpcLayoutEvent {
    IpcLayoutEvent::Disconnected {
        host_id: host_id.0.clone(),
    }
}

impl TauriLayoutSink {
    fn emit(&self, event: IpcLayoutEvent) {
        if let Err(err) = self.0.emit(LAYOUT_EVENT, event) {
            eprintln!("could not emit {LAYOUT_EVENT}: {err}");
        }
    }
}

impl LayoutSink for TauriLayoutSink {
    fn layout(&self, host_id: &HostId, request: Option<RequestId>, snapshot: LayoutSnapshot) {
        self.emit(snapshot_event(host_id, request, snapshot));
    }

    fn layout_error(&self, host_id: &HostId, request: RequestId, code: ErrorCode, message: String) {
        self.emit(error_event(host_id, request, code, message));
    }

    fn layout_disconnected(&self, host_id: &HostId) {
        self.emit(disconnected_event(host_id));
    }

    fn panels(&self, host_id: &HostId, panels: Panels) {
        panels::emit(&self.0, panels::list_event(host_id, panels));
    }

    fn panel_reply(&self, host_id: &HostId, reply: PanelReply) {
        panels::emit(&self.0, panels::reply_event(host_id, reply));
    }
}

pub fn layout_sink(handle: AppHandle) -> Arc<dyn LayoutSink> {
    Arc::new(TauriLayoutSink(handle))
}

fn watch(outbound: &Outbound, host_id: String, space: String) -> Result<DecimalU64, String> {
    outbound
        .send(&HostId(host_id), RequestKind::Layout, |request| {
            ClientMessage::WatchLayout(WatchLayout { request, space })
        })
        .map(Into::into)
        .map_err(|err| err.to_string())
}

fn unwatch(outbound: &Outbound, host_id: String) -> Result<(), String> {
    outbound
        .send_untracked(
            &HostId(host_id),
            ClientMessage::UnwatchLayout(UnwatchLayout {}),
        )
        .map_err(|err| err.to_string())
}

fn mutate(
    outbound: &Outbound,
    host_id: String,
    space: String,
    base_layout_revision: u64,
    ops: Vec<IpcLayoutOp>,
) -> Result<DecimalU64, String> {
    let ops = ops
        .into_iter()
        .map(LayoutOp::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    outbound
        .send(&HostId(host_id), RequestKind::Layout, |request| {
            ClientMessage::MutateLayout(MutateLayout {
                request,
                space,
                base_layout_revision,
                ops,
            })
        })
        .map(Into::into)
        .map_err(|err| err.to_string())
}

/// Watches a space's layout on a host. The reply and later pushes arrive as `layout-changed`;
/// the result is the request ID the reply carries.
#[tauri::command]
pub fn watch_layout(
    service: State<'_, HostsService>,
    host_id: String,
    space: String,
) -> Result<DecimalU64, String> {
    watch(&service.outbound(), host_id, space)
}

/// Stops the host's layout watch.
#[tauri::command]
pub fn unwatch_layout(service: State<'_, HostsService>, host_id: String) -> Result<(), String> {
    unwatch(&service.outbound(), host_id)
}

/// Sends a batch of ops made against `base_layout_revision`; the result is the request ID of
/// the reply.
#[tauri::command]
pub fn mutate_layout(
    service: State<'_, HostsService>,
    host_id: String,
    space: String,
    base_layout_revision: DecimalU64,
    ops: Vec<IpcLayoutOp>,
) -> Result<DecimalU64, String> {
    mutate(
        &service.outbound(),
        host_id,
        space,
        base_layout_revision.0,
        ops,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

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
        assert_eq!(contract["layoutEvent"], LAYOUT_EVENT);
        assert_eq!(
            contract["commands"]["watchLayout"],
            stringify!(watch_layout)
        );
        assert_eq!(
            contract["commands"]["unwatchLayout"],
            stringify!(unwatch_layout)
        );
        assert_eq!(
            contract["commands"]["mutateLayout"],
            stringify!(mutate_layout)
        );
    }

    fn sample_snapshot() -> LayoutSnapshot {
        let tile = |id: &str, panels: &[&str], active: Option<&str>| LayoutNode::Tile {
            id: id.to_string(),
            panels: panels.iter().map(|p| p.to_string()).collect(),
            active: active.map(str::to_string),
        };
        LayoutSnapshot {
            space: "/work".to_string(),
            layout_revision: 9_007_199_254_740_993,
            root: LayoutNode::Split {
                dir: SplitDirection::H,
                ratio: Ratio::new(0.25).expect("finite"),
                a: Box::new(tile("t1", &["p1", "p2"], Some("p2"))),
                b: Box::new(tile("t2", &[], None)),
            },
            read_only: true,
            error: Some("layout.json is invalid".to_string()),
            dropped: Some(DroppedNotice {
                op_count: 2,
                message: "dropped".to_string(),
            }),
        }
    }

    #[test]
    fn layout_event_shapes_match_fixture() {
        let fixture = fixture("layout-event.json");
        let host = HostId("host-1".to_string());
        let events = [
            (
                "snapshot",
                snapshot_event(&host, Some(RequestId(7)), sample_snapshot()),
            ),
            ("push", snapshot_event(&host, None, sample_snapshot())),
            (
                "error",
                error_event(
                    &host,
                    RequestId(8),
                    ErrorCode::StaleLayout,
                    "stale".to_string(),
                ),
            ),
            ("disconnected", disconnected_event(&host)),
        ];
        for (name, event) in events {
            assert_eq!(
                serde_json::to_value(event).expect("json"),
                fixture[name],
                "{name}"
            );
        }
    }

    #[test]
    fn op_shapes_match_fixture() {
        let fixture = fixture("layout-event.json");
        let ops = fixture["ops"].as_array().expect("ops array");
        assert_eq!(ops.len(), 5);
        let wire: Vec<LayoutOp> = ops
            .iter()
            .map(|op| {
                let op: IpcLayoutOp = serde_json::from_value(op.clone()).expect("op decodes");
                LayoutOp::try_from(op).expect("converts")
            })
            .collect();
        assert_eq!(
            wire,
            vec![
                LayoutOp::SetActive {
                    tile: "t1".to_string(),
                    panel: "p2".to_string()
                },
                LayoutOp::SetRatio {
                    leaves: vec!["t1".to_string(), "t2".to_string()],
                    ratio: Ratio::new(0.4).expect("finite")
                },
                LayoutOp::MovePanel {
                    panel: "p1".to_string(),
                    to_tile: "t2".to_string(),
                    index: 1
                },
                LayoutOp::SplitTile {
                    tile: "t1".to_string(),
                    dir: SplitDirection::V,
                    new_tile: "t3".to_string(),
                    ratio: Ratio::new(0.5).expect("finite"),
                    panel: Some("p1".to_string())
                },
                LayoutOp::CloseTile {
                    tile: "t3".to_string()
                },
            ]
        );
    }

    #[test]
    fn malformed_ops_and_revisions_are_refused() {
        assert!(
            serde_json::from_value::<IpcLayoutOp>(json!({"op": "explode", "tile": "t1"})).is_err()
        );
        assert!(serde_json::from_value::<IpcLayoutOp>(
            json!({"op": "move_panel", "panel": "p1", "to_tile": "t2", "index": 1})
        )
        .is_err());
        assert!(serde_json::from_value::<DecimalU64>(json!(5)).is_err());
    }

    #[test]
    fn ops_decode_from_camel_case_and_convert() {
        let ops: Vec<IpcLayoutOp> = serde_json::from_value(json!([
            {"op": "move_panel", "panel": "p1", "toTile": "t2", "index": 1},
            {"op": "split_tile", "tile": "t1", "dir": "v", "newTile": "t3", "ratio": 0.5, "panel": null},
            {"op": "close_tile", "tile": "t3"}
        ]))
        .expect("ops decode");
        let wire: Vec<LayoutOp> = ops
            .into_iter()
            .map(|op| LayoutOp::try_from(op).expect("converts"))
            .collect();
        assert_eq!(
            wire[0],
            LayoutOp::MovePanel {
                panel: "p1".to_string(),
                to_tile: "t2".to_string(),
                index: 1
            }
        );
        assert!(matches!(wire[1], LayoutOp::SplitTile { .. }));
    }

    #[test]
    fn commands_without_a_session_are_not_connected() {
        let outbound = Outbound::default();
        let not_connected = Err("not connected".to_string());
        assert_eq!(
            watch(&outbound, "h".to_string(), "/work".to_string()),
            not_connected
        );
        assert_eq!(
            unwatch(&outbound, "h".to_string()),
            not_connected.map(|_: DecimalU64| ())
        );
        assert_eq!(
            mutate(
                &outbound,
                "h".to_string(),
                "/work".to_string(),
                1,
                Vec::new()
            ),
            Err("not connected".to_string())
        );
    }

    #[test]
    fn mutate_sends_the_base_revision_and_returns_the_request() {
        let outbound = Arc::new(Outbound::default());
        let (tx, rx) = mpsc::channel();
        let _registration = outbound.register(&HostId("h".to_string()), tx);
        let request = mutate(
            &outbound,
            "h".to_string(),
            "/work".to_string(),
            5,
            vec![IpcLayoutOp::CloseTile {
                tile: "t1".to_string(),
            }],
        )
        .expect("sent");
        assert_eq!(request, DecimalU64(1));
        assert_eq!(
            rx.try_recv().ok(),
            Some(Outgoing::Message(ClientMessage::MutateLayout(
                MutateLayout {
                    request: RequestId(1),
                    space: "/work".to_string(),
                    base_layout_revision: 5,
                    ops: vec![LayoutOp::CloseTile {
                        tile: "t1".to_string()
                    }],
                }
            )))
        );
    }
}
