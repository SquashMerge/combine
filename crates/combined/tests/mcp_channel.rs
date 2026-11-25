#![cfg(feature = "test-support")]
//! The MCP channel in the server loop: `mcp_open`, the cap, the frames before it, the closed
//! channel's slot and the order of the replies.

mod common;
mod daemon_support;
mod mcp_support;
mod panel_support;

use std::io::{BufReader, Write};
use std::time::{Duration, Instant};

use combine_protocol::{
    ClientMessage, DaemonMessage, ErrorCode, JsonRpcMessage, Mcp, McpCaller, McpOpen, RequestId,
    WatchTerminals,
};
use combined::core::PanelApiError;
use combined::rpc::ServeOptions;
use daemon_support::{wait_until, WireClient};
use mcp_support::{cwd_caller, wire_options, McpWire};
use panel_support::{PanelServer, PanelServerConfig};
use serde_json::json;
use std::os::unix::net::UnixStream;

fn server_with(options: ServeOptions) -> PanelServer {
    PanelServer::start(PanelServerConfig {
        options,
        ..PanelServerConfig::default()
    })
}

/// A server with a terminal in its project folder, which makes the folder a space.
fn server() -> PanelServer {
    let server = server_with(wire_options());
    server.spawn_terminal();
    server
}

fn ping() -> serde_json::Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})
}

#[test]
fn mcp_open_names_the_terminals_space() {
    let server = server_with(wire_options());
    let other = server.dir.path().join("other");
    std::fs::create_dir_all(other.join(".git")).unwrap();
    let term = server.spawn_terminal_in(&other);
    let socket = server.socket.to_str().unwrap().to_string();
    server.callers.set_socket(Some(&socket));

    // The terminal names `other`, the working directory names the project folder: the terminal
    // wins.
    let caller = McpCaller {
        terminal_id: Some(term.0.strip_prefix("fake:").unwrap().to_string()),
        terminal_socket: Some(socket),
        cwd: Some(server.space.0.clone()),
    };
    let wire = McpWire::open(&server, caller);
    assert_eq!(wire.space, other.to_str().unwrap());
}

#[test]
fn a_second_mcp_open_is_refused() {
    let server = server();
    let mut wire = McpWire::open(&server, cwd_caller(&server));
    let error = wire.try_open(cwd_caller(&server)).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert_eq!(
        error.message,
        "the MCP channel is already open on this connection"
    );
    let reply = wire.call("ping", json!({}));
    assert_eq!(reply["result"], json!({}));
}

#[test]
fn mcp_frames_before_mcp_open_are_ignored() {
    let server = server();
    let mut wire = McpWire::unopened(WireClient::open(&server.socket).0);
    wire.send(&ping());
    assert!(wire.recv_within(Duration::from_millis(300)).is_none());

    let mut client = wire.into_client();
    client.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(7),
    }));
    assert!(matches!(client.recv(), DaemonMessage::Terminals(_)));
}

#[test]
fn mcp_open_without_a_match_is_unknown_caller() {
    let server = server();
    let mut wire = McpWire::unopened(WireClient::open(&server.socket).0);
    let nowhere = McpCaller {
        terminal_id: None,
        terminal_socket: None,
        cwd: Some("/nowhere".to_string()),
    };
    let error = wire.try_open(nowhere).unwrap_err();
    assert_eq!(error.code, ErrorCode::UnknownCaller);
    assert_eq!(error.message, "no terminal or space matches the caller");
    // A refused open may be retried on the same connection.
    assert!(wire.try_open(cwd_caller(&server)).is_ok());
}

#[test]
fn a_terminal_with_an_empty_cwd_does_not_make_every_caller_match() {
    let server = server();
    server.spawn_terminal_in(std::path::Path::new(""));
    let mut wire = McpWire::unopened(WireClient::open(&server.socket).0);
    let nowhere = McpCaller {
        terminal_id: None,
        terminal_socket: None,
        cwd: Some("/nowhere".to_string()),
    };
    let error = wire.try_open(nowhere).unwrap_err();
    assert_eq!(error.code, ErrorCode::UnknownCaller);
    assert!(wire.try_open(cwd_caller(&server)).is_ok());
}

#[test]
fn mcp_open_beyond_the_cap_is_refused() {
    let server = server_with(ServeOptions {
        max_mcp_connections: 2,
        ..wire_options()
    });
    server.spawn_terminal();
    let _first = McpWire::open(&server, cwd_caller(&server));
    let _second = McpWire::open(&server, cwd_caller(&server));

    let mut third = McpWire::unopened(WireClient::open(&server.socket).0);
    let error = third.try_open(cwd_caller(&server)).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert_eq!(error.message, "too many MCP connections; the limit is 2");
    assert_eq!(server.server.stats().mcp_channels(), 2);
}

#[test]
fn a_closed_channel_frees_its_slot_within_1s() {
    let server = server_with(ServeOptions {
        max_mcp_connections: 2,
        ..wire_options()
    });
    server.spawn_terminal();
    let stats = server.server.stats();
    let first = McpWire::open(&server, cwd_caller(&server));
    let _second = McpWire::open(&server, cwd_caller(&server));
    assert_eq!(stats.mcp_channels(), 2);

    drop(first.into_client());
    assert!(
        wait_until(Duration::from_secs(1), || stats.mcp_channels() == 1),
        "the closed channel kept its slot"
    );
    let mut again = McpWire::unopened(WireClient::open(&server.socket).0);
    assert!(again.try_open(cwd_caller(&server)).is_ok());
}

#[test]
fn mcp_connections_take_no_client_slot() {
    let server = server_with(ServeOptions {
        max_connections: 1,
        ..wire_options()
    });
    server.spawn_terminal();
    let _channel = McpWire::open(&server, cwd_caller(&server));

    let (mut client, _) = WireClient::open(&server.socket);
    client.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(1),
    }));
    assert!(matches!(client.recv(), DaemonMessage::Terminals(_)));

    let third = WireClient::connect(&server.socket);
    third.expect_closed();
}

#[test]
fn a_connection_that_queues_more_than_16_requests_is_disconnected() {
    let server = server_with(ServeOptions {
        mcp_wait: Duration::from_secs(2),
        ..wire_options()
    });
    server.spawn_terminal();
    let mut wire = McpWire::open(&server, cwd_caller(&server));
    wire.wait_loaded();
    let _hold = server.check.hold("graph TD; A-->B");
    wire.send_call(
        "create_panel",
        json!({"renderer": "mermaid", "content": "graph TD; A-->B"}),
    );
    assert!(
        wait_until(Duration::from_secs(5), || server.check.in_flight() == 1),
        "the first call never reached its wait"
    );

    // One request runs and 16 may wait: the 18th is one too many.
    for n in 0..17 {
        wire.send(&json!({"jsonrpc": "2.0", "id": 100 + n, "method": "ping"}));
    }
    wire.into_client().expect_closed();
}

#[test]
fn mcp_replies_keep_request_order() {
    let server = server();
    let mut wire = McpWire::open(&server, cwd_caller(&server));
    wire.wait_loaded();
    let hold = server.check.hold("graph TD; A-->B");
    let slow = wire.send_call(
        "create_panel",
        json!({"renderer": "mermaid", "content": "graph TD; A-->B"}),
    );
    let second = wire.send_call("list_panels", json!({}));
    let third = wire.send_call("list_panels", json!({}));

    let ids: Vec<u64> = (0..3)
        .map(|_| wire.recv()["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, [slow, second, third]);
    drop(hold);
}

#[test]
fn a_closed_connections_queued_calls_never_run_and_its_slot_frees() {
    const SRC1: &str = "graph TD; A-->B";
    let server = server_with(ServeOptions {
        mcp_wait: Duration::from_secs(30),
        mcp_poll: Duration::from_millis(10),
        ..wire_options()
    });
    server.spawn_terminal();
    let stats = server.server.stats();
    let mut wire = McpWire::open(&server, cwd_caller(&server));
    wire.wait_loaded();
    let hold = server.check.hold(SRC1);
    server
        .panels
        .create(
            &server.space,
            combined::core::PanelCreate {
                id: Some("keep".to_string()),
                lifetime: combined::model::Lifetime::Persistent,
                renderer: combine_protocol::Renderer::Markdown,
                title: None,
                idempotency_key: None,
                created_by: None,
                placement: None,
                content: "kept".to_string(),
            },
        )
        .unwrap();

    wire.send_call(
        "create_panel",
        json!({"id": "q1", "renderer": "mermaid", "content": SRC1}),
    );
    for id in ["q2", "q3", "q4"] {
        wire.send_call(
            "create_panel",
            json!({"id": id, "renderer": "markdown", "content": id}),
        );
    }
    wire.send_call("remove_panel", json!({"panel": "keep"}));

    assert!(
        wait_until(Duration::from_secs(5), || server
            .panels
            .status(&server.space, "q1")
            .is_ok()),
        "q1 was never created"
    );
    drop(wire.into_client());

    assert!(
        wait_until(Duration::from_secs(1), || stats.mcp_channels() == 0),
        "the closed connection's tool thread kept its slot"
    );
    for id in ["q2", "q3", "q4"] {
        assert!(
            matches!(
                server.panels.status(&server.space, id),
                Err(PanelApiError::UnknownPanel { .. })
            ),
            "{id} was created"
        );
    }
    let keep = server.panels.status(&server.space, "keep").unwrap();
    assert_ne!(keep.state, combine_protocol::PanelState::Removed);
    let lines = server.audit.lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains(r#"panel="q1""#), "{}", lines[0]);
    assert!(lines[0].contains("state=validating"), "{}", lines[0]);
    drop(hold);
}

/// A connection closing after a malformed frame keeps its MCP slot until its socket is closed,
/// even when the channel's tool thread has already ended and the writer is stuck on a batch.
#[test]
fn a_closing_mcp_connection_keeps_its_slot_until_its_socket_closes() {
    let server = server_with(ServeOptions {
        max_connections: 1,
        max_mcp_connections: 1,
        mcp_wait: Duration::from_secs(30),
        mcp_poll: Duration::from_millis(10),
        ..wire_options()
    });
    server.spawn_terminal();
    let stats = server.server.stats();
    let panels = server.server.panels();
    assert!(
        wait_until(Duration::from_secs(5), || panels
            .list(&server.space)
            .is_ok()),
        "the space never loaded"
    );
    for n in 0..99 {
        server
            .panels
            .create(
                &server.space,
                combined::core::PanelCreate {
                    id: Some(format!("p{n:03}")),
                    lifetime: combined::model::Lifetime::Persistent,
                    renderer: combine_protocol::Renderer::Markdown,
                    title: Some("t".repeat(512)),
                    idempotency_key: None,
                    created_by: None,
                    placement: None,
                    content: "x".to_string(),
                },
            )
            .unwrap();
    }

    // A peer that never reads: the list reply fills the socket and the writer keeps its batch.
    let mut stream = UnixStream::connect(&server.socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    combine_protocol::client_handshake(&mut reader, &mut stream).unwrap();
    combine_protocol::write_message(
        &mut stream,
        &ClientMessage::McpOpen(McpOpen {
            request: RequestId(1),
            caller: cwd_caller(&server),
        }),
    )
    .unwrap();
    assert!(matches!(
        combine_protocol::read_message::<DaemonMessage, _>(&mut reader).unwrap(),
        Some(DaemonMessage::McpOpened(_))
    ));
    let send = |stream: &mut UnixStream, text: &str| {
        let message = JsonRpcMessage::new(text.to_string()).unwrap();
        combine_protocol::write_message(stream, &ClientMessage::Mcp(Mcp { message })).unwrap();
    };
    for id in 10..18 {
        send(
            &mut stream,
            &format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"list_panels"}}}}"#
            ),
        );
    }
    let _hold = server.check.hold("graph TD; A-->B");
    send(
        &mut stream,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"create_panel","arguments":{"renderer":"mermaid","content":"graph TD; A-->B"}}}"#,
    );
    assert!(
        wait_until(Duration::from_secs(5), || server.check.in_flight() == 1),
        "the create never reached the check"
    );

    stream.write_all(&[0, 0, 0, 1, b'!']).unwrap();

    // The tool thread ends within a 50 ms check, while the socket stays open.
    let mut replacement = McpWire::unopened(WireClient::open(&server.socket).0);
    let until = Instant::now() + Duration::from_millis(500);
    while Instant::now() < until {
        let error = replacement.try_open(cwd_caller(&server)).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert_eq!(error.message, "too many MCP connections; the limit is 1");
        assert_eq!(stats.mcp_channels(), 1);
        std::thread::sleep(Duration::from_millis(25));
    }

    // Draining the old socket lets the writer finish and close it, which frees the slot.
    while let Ok(Some(_)) = combine_protocol::read_message::<DaemonMessage, _>(&mut reader) {}
    assert!(
        wait_until(Duration::from_secs(5), || stats.mcp_channels() == 0),
        "the closed socket's slot was never freed"
    );
    replacement
        .try_open(cwd_caller(&server))
        .expect("the replacement opens once the old socket is closed");
}
