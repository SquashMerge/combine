//! One MCP channel: the plain thread that runs a connection's tool calls, and the link the
//! server loop keeps to it. The loop never blocks on a call: it hands each request over with a
//! `try_send`, and the thread's replies come back through the loop's inbox.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;

use combine_protocol::{DroppedNotice, JsonRpcMessage};
use tokio::sync::mpsc::Sender;

use super::connection::ConnEvent;
use super::panel_calls::PanelHandle;
use crate::core::{
    ClientRender, ConnectionId, PanelApiError, PanelCreate, PanelList, PanelUpdate, PanelView,
    ResolvedCaller,
};
use crate::mcp_server::{McpAudit, McpOptions, McpServer, PanelApi};
use crate::model::SpaceId;

/// The server loop's end of a channel. Dropping it closes the channel.
pub(super) struct McpLink {
    requests: SyncSender<JsonRpcMessage>,
    closed: Arc<AtomicBool>,
}

/// What became of a request offered to the tool thread.
pub(super) enum Offer {
    Queued,
    /// More requests are waiting than the channel's queue holds.
    Full,
    /// The tool thread has ended.
    Gone,
}

impl McpLink {
    /// Hands `message` to the tool thread without waiting.
    pub(super) fn offer(&self, message: JsonRpcMessage) -> Offer {
        match self.requests.try_send(message) {
            Ok(()) => Offer::Queued,
            Err(TrySendError::Full(_)) => Offer::Full,
            Err(TrySendError::Disconnected(_)) => Offer::Gone,
        }
    }

    /// Sets the closed flag: the tool thread runs nothing more.
    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

impl Drop for McpLink {
    /// Removing the connection closes its channel.
    fn drop(&mut self) {
        self.close();
    }
}

pub(super) struct ChannelStart {
    pub conn: ConnectionId,
    pub caller: ResolvedCaller,
    pub panels: PanelHandle,
    pub inbox: Sender<ConnEvent>,
    pub audit: Arc<dyn McpAudit>,
    pub options: McpOptions,
    /// How many requests may wait for the thread.
    pub queue: usize,
}

/// Spawns the tool thread `combined-mcp-<conn>`.
pub(super) fn start(start: ChannelStart) -> io::Result<McpLink> {
    let ChannelStart {
        conn,
        caller,
        panels,
        inbox,
        audit,
        options,
        queue,
    } = start;
    let (requests, queued) = mpsc::sync_channel(queue);
    let closed = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&closed);
    thread::Builder::new()
        .name(format!("combined-mcp-{}", conn.0))
        .spawn(move || {
            let _ended = Ended {
                conn,
                inbox: inbox.clone(),
            };
            let api = SpacePanels {
                space: caller.space.clone(),
                handle: panels,
            };
            let server = McpServer::new(caller, options, audit);
            serve(&queued, &flag, &server, &api, |reply| {
                inbox
                    .blocking_send(ConnEvent::McpReply(conn, reply))
                    .is_ok()
            });
        })?;
    Ok(McpLink { requests, closed })
}

/// Runs the channel's requests one at a time, in arrival order, until the channel closes or
/// `deliver` says the server is gone.
///
/// `recv` keeps handing out buffered requests after the link has dropped, so the closed flag,
/// not the end of the queue, is what stops a removed connection's calls. It is checked after
/// each `recv` and after each call: a reply that finished after the flag was set is dropped,
/// whatever the call was, so nothing is written for a connection that is gone.
fn serve(
    requests: &Receiver<JsonRpcMessage>,
    closed: &AtomicBool,
    server: &McpServer,
    api: &dyn PanelApi,
    mut deliver: impl FnMut(JsonRpcMessage) -> bool,
) {
    while let Ok(message) = requests.recv() {
        if closed.load(Ordering::SeqCst) {
            return;
        }
        let reply = server.handle(message.as_str(), api, closed);
        if closed.load(Ordering::SeqCst) {
            return;
        }
        if let Some(reply) = reply {
            if !deliver(reply) {
                return;
            }
        }
    }
}

/// Tells the server loop the thread ended, however it ended, a panic included.
struct Ended {
    conn: ConnectionId,
    inbox: Sender<ConnEvent>,
}

impl Drop for Ended {
    fn drop(&mut self) {
        let _ = self.inbox.blocking_send(ConnEvent::McpEnded(self.conn));
    }
}

/// One space's panel API over the server's panel channel. Every call blocks, so it runs only on
/// the tool thread.
struct SpacePanels {
    space: SpaceId,
    handle: PanelHandle,
}

impl PanelApi for SpacePanels {
    fn create(&self, create: PanelCreate) -> Result<PanelView, PanelApiError> {
        self.handle.create(&self.space, create)
    }

    fn update(&self, id: &str, update: PanelUpdate) -> Result<PanelView, PanelApiError> {
        self.handle.update(&self.space, id, update)
    }

    fn remove(&self, id: &str) -> Result<PanelView, PanelApiError> {
        self.handle.remove(&self.space, id)
    }

    fn status(&self, id: &str) -> Result<PanelView, PanelApiError> {
        self.handle.status(&self.space, id)
    }

    fn list(&self) -> Result<PanelList, PanelApiError> {
        self.handle.list(&self.space)
    }

    fn render_status(&self, id: &str) -> Result<Vec<ClientRender>, PanelApiError> {
        self.handle.render_status(&self.space, id)
    }

    fn layout_dropped(&self) -> Result<Option<DroppedNotice>, PanelApiError> {
        self.handle.layout_dropped(&self.space)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use super::*;
    use crate::core::CallerMatch;
    use crate::mcp_server::StderrAudit;

    /// An API that sets the closed flag during its first call, as a connection closing would.
    struct ClosingApi<'a> {
        closed: &'a AtomicBool,
        calls: AtomicUsize,
    }

    impl ClosingApi<'_> {
        fn call<T>(&self) -> Result<T, PanelApiError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.closed.store(true, Ordering::SeqCst);
            Err(PanelApiError::Stopped)
        }
    }

    impl PanelApi for ClosingApi<'_> {
        fn create(&self, _: PanelCreate) -> Result<PanelView, PanelApiError> {
            self.call()
        }

        fn update(&self, _: &str, _: PanelUpdate) -> Result<PanelView, PanelApiError> {
            self.call()
        }

        fn remove(&self, _: &str) -> Result<PanelView, PanelApiError> {
            self.call()
        }

        fn status(&self, _: &str) -> Result<PanelView, PanelApiError> {
            self.call()
        }

        fn list(&self) -> Result<PanelList, PanelApiError> {
            self.call()
        }

        fn render_status(&self, _: &str) -> Result<Vec<ClientRender>, PanelApiError> {
            self.call()
        }
    }

    fn call(id: u32, tool: &str, arguments: &str) -> JsonRpcMessage {
        JsonRpcMessage::new(format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{tool}","arguments":{arguments}}}}}"#
        ))
        .unwrap()
    }

    fn server() -> McpServer {
        McpServer::new(
            ResolvedCaller {
                space: SpaceId("/work".to_string()),
                slot: None,
                by: CallerMatch::Cwd,
            },
            McpOptions {
                wait: Duration::from_millis(50),
                poll: Duration::from_millis(5),
            },
            Arc::new(StderrAudit),
        )
    }

    /// Runs `message` on a channel whose connection closes during the call, and returns how
    /// many API calls ran and what was delivered.
    fn closed_during(message: JsonRpcMessage) -> (usize, Vec<JsonRpcMessage>) {
        let closed = AtomicBool::new(false);
        let api = ClosingApi {
            closed: &closed,
            calls: AtomicUsize::new(0),
        };
        let (requests, queued) = mpsc::sync_channel(4);
        requests.send(message).unwrap();
        requests.send(call(2, "list_panels", "{}")).unwrap();
        drop(requests);
        let mut delivered = Vec::new();
        serve(&queued, &closed, &server(), &api, |reply| {
            delivered.push(reply);
            true
        });
        (api.calls.load(Ordering::SeqCst), delivered)
    }

    #[test]
    fn a_reply_finished_after_the_channel_closed_is_dropped() {
        for message in [
            call(1, "list_panels", "{}"),
            call(1, "panel_status", r#"{"panel":"p"}"#),
            call(1, "remove_panel", r#"{"panel":"p"}"#),
        ] {
            let (calls, delivered) = closed_during(message.clone());
            assert_eq!(calls, 1, "{message:?}: the queued request ran");
            assert!(delivered.is_empty(), "{message:?} replied: {delivered:?}");
        }
    }

    #[test]
    fn an_open_channel_delivers_each_reply_in_order() {
        let closed = AtomicBool::new(false);
        struct Gone;
        impl PanelApi for Gone {
            fn create(&self, _: PanelCreate) -> Result<PanelView, PanelApiError> {
                Err(PanelApiError::Stopped)
            }
            fn update(&self, _: &str, _: PanelUpdate) -> Result<PanelView, PanelApiError> {
                Err(PanelApiError::Stopped)
            }
            fn remove(&self, _: &str) -> Result<PanelView, PanelApiError> {
                Err(PanelApiError::Stopped)
            }
            fn status(&self, _: &str) -> Result<PanelView, PanelApiError> {
                Err(PanelApiError::Stopped)
            }
            fn list(&self) -> Result<PanelList, PanelApiError> {
                Err(PanelApiError::Stopped)
            }
            fn render_status(&self, _: &str) -> Result<Vec<ClientRender>, PanelApiError> {
                Err(PanelApiError::Stopped)
            }
        }
        let (requests, queued) = mpsc::sync_channel(4);
        requests.send(call(1, "list_panels", "{}")).unwrap();
        requests.send(call(2, "list_panels", "{}")).unwrap();
        drop(requests);
        let mut delivered = Vec::new();
        serve(&queued, &closed, &server(), &Gone, |reply| {
            delivered.push(reply);
            true
        });
        let ids: Vec<bool> = delivered
            .iter()
            .enumerate()
            .map(|(n, reply)| reply.as_str().contains(&format!(r#""id":{}"#, n + 1)))
            .collect();
        assert_eq!(ids, [true, true]);
    }
}
