//! A panel made in core reaches the client through the hosts service: the watch answer carries
//! the panel list, a creation is pushed, a fetch returns the source, and a render report lands
//! in the daemon. In-process daemon on the fake backend; no ssh, no network listener and no
//! herdr-core.

mod support;

use std::path::Path;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use combine_app::hosts::attachments::{StreamDelivery, StreamId, TerminalSink};
use combine_app::hosts::config::Transport;
use combine_app::hosts::local::{LocalSocketConnector, SocketGuardEnv};
use combine_app::hosts::model::{ConnectionState, HostsSnapshot};
use combine_app::hosts::outbound::RequestKind;
use combine_app::hosts::panels::PanelReply;
use combine_app::hosts::service::{HostsService, ServiceDeps, SnapshotSink};
use combine_app::hosts::spawn::{SpawnReply, SpawnSink};
use combine_protocol::{
    ClientMessage, GetPanel, HostId, LayoutNode, PanelState, Panels, RenderStatus, Renderer,
    ReportRender, WatchLayout,
};
use combined::core::{PanelCreate, PANELS_LOADING};
use combined::model::Lifetime;
use support::{write_hosts, PanelDaemon, PanelEvent, RecordingPanels, ScratchDir};

const HOST: &str = "00000000-0000-4000-8000-000000000048";
const LAYOUT: &str = r#"{"version":1,"root":{"type":"tile","id":"main","panels":["doc"],"active":"doc"},"groups":[]}"#;
const SOURCE: &str = "# Doc\n\nA panel made in core.\n";

const POLL: Duration = Duration::from_millis(20);
const DEADLINE: Duration = Duration::from_secs(30);

struct NoSpawns;

impl SpawnSink for NoSpawns {
    fn spawn_reply(&self, _: &HostId, _: SpawnReply) {}
}

struct NoTerminals;

impl TerminalSink for NoTerminals {
    fn deliver(&self, _: StreamId, _: StreamDelivery<'_>) {}
}

struct IgnoreSnapshots;

impl SnapshotSink for IgnoreSnapshots {
    fn publish(&self, _: &HostsSnapshot) {}
}

fn start_service(client_dir: &Path, sink: Arc<RecordingPanels>) -> HostsService {
    let mut deps = ServiceDeps::system(sink, Arc::new(NoSpawns), Arc::new(NoTerminals));
    deps.connector_for = Box::new(|host| match &host.transport {
        Transport::Local { socket } => Arc::new(LocalSocketConnector::new(
            socket.clone(),
            SocketGuardEnv::from_process(),
        )),
        Transport::Ssh { .. } => panic!("this test never runs ssh"),
    });
    HostsService::start(client_dir, deps, Arc::new(IgnoreSnapshots))
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(POLL);
    }
}

fn next_event(events: &Receiver<PanelEvent>, what: &str) -> PanelEvent {
    events
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

fn is_doc(panels: &Panels) -> bool {
    panels.panels.iter().any(|panel| panel.id == "doc")
}

#[test]
fn a_panel_created_in_core_reaches_the_client() {
    let scratch = ScratchDir::new();
    let mut daemon = PanelDaemon::new(&scratch, "core", HOST);
    daemon.seed_layout(LAYOUT);
    daemon.start();
    let client_dir = scratch.path.join("client");
    write_hosts(
        &client_dir,
        serde_json::json!([{ "name": "core", "socket": daemon.checked_socket() }]),
    );
    let (sink, events) = RecordingPanels::new();
    let service = start_service(&client_dir, sink);
    let space = daemon.space();
    let host = daemon.host_id.clone();

    wait_until("the host to connect", || {
        service.snapshot().hosts.iter().any(|view| {
            view.connection == ConnectionState::Connected && view.host_id.as_ref() == Some(&host)
        })
    });
    let outbound = service.outbound();
    let watch = outbound
        .send(&host, RequestKind::Layout, |request| {
            ClientMessage::WatchLayout(WatchLayout {
                request,
                space: space.0.clone(),
            })
        })
        .expect("send the watch");

    let PanelEvent::Layout(layout_host, request, snapshot) = next_event(&events, "the layout")
    else {
        panic!("the layout must come first");
    };
    assert_eq!((layout_host, request), (host.clone(), Some(watch)));
    assert_eq!(snapshot.space, space.0);
    assert_eq!(
        snapshot.root,
        LayoutNode::Tile {
            id: "main".to_string(),
            panels: vec!["doc".to_string()],
            active: Some("doc".to_string()),
        }
    );
    let PanelEvent::Panels(panels_host, listed) = next_event(&events, "the panel list") else {
        panic!("the panel list must follow the layout");
    };
    assert_eq!((panels_host, listed.request), (host.clone(), Some(watch)));
    assert_eq!(listed.space, space.0);
    assert!(!is_doc(&listed), "doc is not made yet: {listed:?}");

    wait_until("the space's panels to be read", || {
        daemon.panels().list(&space).is_ok()
    });
    let made = daemon
        .panels()
        .create(
            &space,
            PanelCreate {
                id: Some("doc".to_string()),
                lifetime: Lifetime::Persistent,
                renderer: Renderer::Markdown,
                title: Some("Doc".to_string()),
                idempotency_key: None,
                created_by: None,
                placement: None,
                content: SOURCE.to_string(),
            },
        )
        .expect("create doc");
    assert_eq!(made.id, "doc");

    let deadline = Instant::now() + DEADLINE;
    let (pushed_with_doc, validated) = loop {
        let PanelEvent::Panels(pushed_host, pushed) = next_event(&events, "the push with doc")
        else {
            continue;
        };
        assert_eq!(pushed_host, host);
        if is_doc(&pushed) {
            break (pushed.request, pushed);
        }
        assert!(Instant::now() < deadline, "no push held doc");
    };
    assert_eq!(pushed_with_doc, None, "a push answers no request");
    let mut summary = validated
        .panels
        .into_iter()
        .find(|p| p.id == "doc")
        .unwrap();
    let validating = Instant::now() + DEADLINE;
    while summary.state != PanelState::Validated {
        assert!(Instant::now() < validating, "doc never validated");
        let PanelEvent::Panels(_, pushed) = next_event(&events, "doc to validate") else {
            continue;
        };
        if let Some(found) = pushed.panels.into_iter().find(|p| p.id == "doc") {
            summary = found;
        }
    }
    assert_eq!(summary.title.as_deref(), Some("Doc"));
    assert_eq!(summary.renderer, Renderer::Markdown);

    let fetch = outbound
        .send(&host, RequestKind::Panel, |request| {
            ClientMessage::GetPanel(GetPanel {
                request,
                space: space.0.clone(),
                panel: "doc".to_string(),
            })
        })
        .expect("send the fetch");
    let replying = Instant::now() + DEADLINE;
    let panel = loop {
        assert!(Instant::now() < replying, "no panel reply");
        if let PanelEvent::PanelReply(reply_host, PanelReply::Panel(panel)) =
            next_event(&events, "the panel reply")
        {
            assert_eq!(reply_host, host);
            break panel;
        }
    };
    assert_eq!(panel.request, fetch);
    assert_eq!(panel.id, "doc");
    assert_eq!(panel.source, SOURCE);
    assert_eq!(panel.revision, summary.revision);

    outbound
        .send_untracked(
            &host,
            ClientMessage::ReportRender(ReportRender {
                space: space.0.clone(),
                panel: "doc".to_string(),
                revision: panel.revision,
                status: RenderStatus::Rendered,
                error: None,
            }),
        )
        .expect("send the report");
    wait_until("the render status", || {
        daemon
            .panels()
            .render_status(&space, "doc")
            .expect("read the render status")
            .iter()
            .any(|render| {
                render.revision == panel.revision && render.status == RenderStatus::Rendered
            })
    });

    service.stop();
    daemon.stop();
}

#[test]
fn the_loading_text_matches_the_shared_fixture() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../web/test/fixtures/panels-event.json");
    let fixture: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read the shared fixture"))
            .expect("parse the shared fixture");
    assert_eq!(
        fixture["loading"]["list"]["error"].as_str(),
        Some(PANELS_LOADING)
    );
}
