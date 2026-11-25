//! The real `combine mcp` binary over stdio, against an in-process daemon.

mod shim_support;

use std::time::{Duration, Instant};

use combine_protocol::CreatedBy;
use combined::core::parse_view_panel;
use combined::ports::{DiagramFailure, DiagramVerdict, FailureCode, ScratchDir};
use combined::rpc::ServeOptions;
use combined::testing::{attach, read_until_tail, wait_until, WireClient};
use serde_json::{json, Value};
use shim_support::{PanelServer, PanelServerConfig, Shim, ShimEnv, TestDiagramCheck};

const MERMAID: &str = "graph TD; A-->B";
const OTHER_MERMAID: &str = "graph TD; B-->C";
const BROKEN: &str = "graph TD; A-->";
/// The source of the `syntax` fixture that `combined`'s `tests/diagram_check.rs` asserts the
/// position 6:3 for.
const PELE_BROKEN: &str =
    "classDiagram\n  class Animal {\n    +String name\n  }\n  Animal <|-- Dog\n  @@@ not a statement\n";
const PELE_VALID: &str = "classDiagram\n  class Animal\n  class Dog\n  Animal <|-- Dog\n";
const EXIT_WAIT: Duration = Duration::from_secs(10);

fn options(mcp_wait: Duration) -> ServeOptions {
    ServeOptions {
        mcp_wait,
        mcp_poll: Duration::from_millis(10),
        ..ServeOptions::default()
    }
}

fn server_with(options: ServeOptions) -> PanelServer {
    PanelServer::start(PanelServerConfig {
        options,
        diagram: TestDiagramCheck::Fake,
    })
}

/// A server with a terminal in its project folder, whose space the daemon already lists.
fn server() -> PanelServer {
    server_ready(server_with(options(Duration::from_millis(200))))
}

fn server_ready(server: PanelServer) -> PanelServer {
    server.spawn_terminal();
    server.wait_listed(std::path::Path::new(&server.space.0));
    server
}

/// A shim for the project folder, once its space has read its panels.
fn shim_for(server: &PanelServer) -> Shim {
    let mut shim = Shim::spawn(server, ShimEnv::new(server));
    shim.wait_loaded();
    shim
}

fn failure(line: u32, column: u32) -> DiagramVerdict {
    DiagramVerdict::Failed(DiagramFailure {
        code: FailureCode::Syntax,
        message: "unexpected end of input".to_string(),
        line: Some(line),
        column: Some(column),
        snippet: None,
    })
}

fn mermaid(source: &str) -> Value {
    json!({"renderer": "mermaid", "content": source})
}

fn status_of(shim: &mut Shim, panel: &str) -> Value {
    let status = shim.call_tool("panel_status", json!({"panel": panel}));
    assert!(!status.is_error, "{status:?}");
    status.body
}

// AC1

#[test]
fn tools_list_through_the_shim_is_exactly_six() {
    let server = server();
    let mut shim = shim_for(&server);
    let reply = shim.request("tools/list", json!({}));
    let names: Vec<&str> = reply["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "create_panel",
            "update_panel",
            "show_diagram",
            "panel_status",
            "list_panels",
            "remove_panel"
        ]
    );
}

// AC2

fn assert_a_broken_diagram_fails_with_a_position(
    server: &PanelServer,
    source: &str,
    line: u64,
    column: u64,
) {
    let mut shim = shim_for(server);
    let started = Instant::now();
    let created = shim.call_tool("create_panel", mermaid(source));
    let elapsed = started.elapsed();
    assert!(!created.is_error, "{created:?}");
    assert_eq!(created.body["state"], "failed", "{}", created.body);
    assert_eq!(created.body["error"]["line"], line, "{}", created.body);
    assert_eq!(created.body["error"]["column"], column, "{}", created.body);
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
}

#[test]
fn broken_mermaid_fails_with_line_and_column_within_5s() {
    let server = server_ready(server_with(ServeOptions::default()));
    server.check.answer(BROKEN, Ok(failure(2, 5)));
    assert_a_broken_diagram_fails_with_a_position(&server, BROKEN, 2, 5);
}

#[test]
fn broken_mermaid_fails_with_line_and_column_within_5s_with_pele() {
    let server = server_ready(PanelServer::start(PanelServerConfig {
        options: ServeOptions::default(),
        diagram: TestDiagramCheck::Pele,
    }));
    // Pele starts its engine beside the server: the timed call is the second, on a warm engine.
    let mut warm = shim_for(&server);
    let shown = warm.call_tool("show_diagram", json!({"source": PELE_VALID}));
    assert!(!shown.is_error, "{shown:?}");
    let id = shown.body["panel"].as_str().unwrap().to_string();
    assert!(
        wait_until(Duration::from_secs(30), || {
            let status = status_of(&mut warm, &id);
            status["state"] == "validated"
        }),
        "the engine never validated a valid diagram"
    );
    drop(warm);
    assert_a_broken_diagram_fails_with_a_position(&server, PELE_BROKEN, 6, 3);
}

// AC3 and AC16

#[test]
fn an_update_validates_at_a_higher_revision() {
    let server = server();
    let mut shim = shim_for(&server);
    let created = shim.call_tool("create_panel", mermaid(MERMAID));
    assert!(!created.is_error, "{created:?}");
    assert_eq!(created.body["state"], "validated");
    let id = created.body["panel"].as_str().unwrap().to_string();
    let r1 = created.body["revision"].as_u64().unwrap();

    let updated = shim.call_tool(
        "update_panel",
        json!({"panel": id, "content": OTHER_MERMAID}),
    );
    assert!(!updated.is_error, "{updated:?}");
    let r2 = updated.body["revision"].as_u64().unwrap();
    assert!(r2 > r1, "{r2} is not above {r1}");
    assert_eq!(updated.body["state"], "validated");
    assert_eq!(updated.body["superseded"], false);

    let old = shim.call_tool("panel_status", json!({"panel": id, "revision": r1}));
    assert_eq!(old.body["superseded"], true, "{:?}", old.body);
    assert_eq!(old.body["revision"], r2);
    let current = shim.call_tool("panel_status", json!({"panel": id, "revision": r2}));
    assert_eq!(current.body["superseded"], false);
}

// AC4

#[test]
fn idempotency_holds_across_a_daemon_restart() {
    let mut server = server();
    let args = json!({"renderer": "mermaid", "content": MERMAID, "idempotency_key": "K"});
    let mut first = shim_for(&server);
    let created = first.call_tool("create_panel", args.clone());
    assert!(!created.is_error, "{created:?}");
    let id = created.body["panel"].as_str().unwrap().to_string();
    first.close_stdin();
    assert!(first.wait_exit(EXIT_WAIT).success());

    server.restart();
    server.wait_listed(std::path::Path::new(&server.space.0));
    let mut second = shim_for(&server);
    let again = second.call_tool("create_panel", args.clone());
    assert!(!again.is_error, "{again:?}");
    assert_eq!(again.body["panel"], id.as_str());

    let removed = second.call_tool("remove_panel", json!({"panel": id}));
    assert_eq!(removed.body["state"], "removed", "{removed:?}");
    let renewed = second.call_tool("create_panel", args);
    assert!(!renewed.is_error, "{renewed:?}");
    assert_ne!(renewed.body["panel"], id.as_str());
}

// AC5

#[test]
fn the_caller_resolves_by_terminal_only_on_the_daemons_socket() {
    let server = server();
    let work = std::path::PathBuf::from(&server.space.0);
    let w2 = server.dir.path().join("w2");
    std::fs::create_dir_all(w2.join(".git")).unwrap();
    let term = server.spawn_terminal_in(&w2);
    server.wait_listed(&w2);
    let id = term.0.strip_prefix("fake:").unwrap().to_string();
    let daemon_socket = server.socket.to_str().unwrap().to_string();
    server.callers.set_socket(Some(&daemon_socket));
    let elsewhere = server.dir.path().join("elsewhere");
    let wrong_socket = server
        .dir
        .sock("not-the-daemon.sock")
        .to_str()
        .unwrap()
        .to_string();

    let space_of = |env: ShimEnv| -> String {
        let mut shim = Shim::spawn(&server, env);
        let listed = shim.wait_loaded();
        listed.body["space"].as_str().unwrap().to_string()
    };
    let terminal = |socket: &str| ShimEnv {
        terminal_id: Some(id.clone()),
        terminal_socket: Some(socket.to_string()),
        cwd: elsewhere.clone(),
        ..ShimEnv::new(&server)
    };

    // (a) the id and the daemon's socket, with a working directory in no space: the terminal's.
    assert_eq!(space_of(terminal(&daemon_socket)), w2.to_str().unwrap());

    // (b) another socket and a working directory in no space: nobody.
    let mut refused = Shim::spawn(&server, terminal(&wrong_socket));
    let status = refused.wait_exit(EXIT_WAIT);
    assert_eq!(status.code(), Some(1), "{}", refused.stderr());
    assert!(
        refused
            .stderr()
            .contains("no terminal or space matches the caller"),
        "{}",
        refused.stderr()
    );

    // (c) another socket, a working directory below the project: the project.
    let sub = ShimEnv {
        cwd: work.join("sub"),
        ..terminal(&wrong_socket)
    };
    assert_eq!(space_of(sub), server.space.0);

    // (d) no HC_* at all, the project as the working directory: the project.
    let plain = ShimEnv {
        cwd: work.clone(),
        ..ShimEnv::new(&server)
    };
    assert_eq!(space_of(plain), server.space.0);

    // (e) a terminal in W2 on the daemon's socket, a working directory in the project: W2.
    let both = ShimEnv {
        cwd: work.clone(),
        ..terminal(&daemon_socket)
    };
    assert_eq!(space_of(both), w2.to_str().unwrap());
}

// AC6

#[test]
fn a_slow_validator_answers_validating_then_status_validated() {
    let server = server_ready(server_with(ServeOptions::default()));
    let mut shim = shim_for(&server);
    let hold = server.check.hold(MERMAID);
    let started = Instant::now();
    let created = shim.call_tool("create_panel", mermaid(MERMAID));
    let elapsed = started.elapsed();
    assert!(!created.is_error, "{created:?}");
    assert!(
        elapsed >= Duration::from_secs(5) && elapsed < Duration::from_secs(9),
        "took {elapsed:?}"
    );
    assert_eq!(created.body["state"], "validating");
    let id = created.body["panel"].as_str().unwrap().to_string();

    hold.release();
    assert!(
        wait_until(Duration::from_secs(5), || {
            status_of(&mut shim, &id)["state"] == "validated"
        }),
        "the panel never validated"
    );
}

// AC7

#[test]
fn terminal_output_flows_while_a_tool_waits() {
    let server = server_ready(server_with(ServeOptions::default()));
    let term = server.spawn_terminal();
    let mut shim = shim_for(&server);
    let (mut viewer, _) = WireClient::open(&server.socket);
    let attachment = attach(&mut viewer, &term);
    let _hold = server.check.hold(MERMAID);

    let started = Instant::now();
    let id = shim.start_call("create_panel", mermaid(MERMAID));
    for (n, at) in [(1, Duration::from_secs(1)), (2, Duration::from_secs(3))] {
        std::thread::sleep(at.saturating_sub(started.elapsed()));
        let marker = format!("marker_{n}");
        server.control.emit(&term, marker.as_bytes());
        let read = read_until_tail(
            &mut viewer,
            attachment,
            marker.as_bytes(),
            Duration::from_secs(2),
        );
        assert!(read.is_some(), "{marker} didn't reach the viewer in 2 s");
        assert!(
            shim.try_reply(id).is_none(),
            "the tool answered before the wait ended, at {:?}",
            started.elapsed()
        );
    }
    let reply = shim
        .reply(id, Duration::from_secs(20))
        .expect("the create never answered");
    assert_eq!(reply["result"]["isError"], false, "{reply}");
}

// AC8

#[test]
fn the_shim_finds_the_socket_through_daemon_json() {
    let server = server();
    let home = server.dir.path().join("home");
    let config = home.join(".config/combine");
    std::fs::create_dir_all(&config).unwrap();
    let json = json!({"version": 1, "socket": server.socket});
    std::fs::write(config.join("daemon.json"), json.to_string()).unwrap();
    let env = ShimEnv {
        socket: None,
        ..ShimEnv::new(&server)
    };
    let mut shim = Shim::spawn(&server, env);
    let listed = shim.wait_loaded();
    assert_eq!(listed.body["space"], server.space.0.as_str());
}

// AC10

#[test]
fn created_by_is_agent_with_null_slot_and_no_terminal_id_is_written() {
    let server = server();
    let term = server.spawn_terminal();
    let id = term.0.strip_prefix("fake:").unwrap().to_string();
    let socket = server.socket.to_str().unwrap().to_string();
    server.callers.set_socket(Some(&socket));
    let mut shim = Shim::spawn(
        &server,
        ShimEnv {
            terminal_id: Some(id.clone()),
            terminal_socket: Some(socket),
            ..ShimEnv::new(&server)
        },
    );
    shim.wait_loaded();

    let persistent = shim.call_tool(
        "create_panel",
        json!({"id": "kept", "renderer": "markdown", "content": "a note"}),
    );
    assert!(!persistent.is_error, "{persistent:?}");
    let scratch = shim.call_tool("show_diagram", json!({"id": "shown", "source": MERMAID}));
    assert!(!scratch.is_error, "{scratch:?}");
    assert!(
        wait_until(Duration::from_secs(5), || {
            server
                .store
                .bytes(&server.space, "panels/kept/panel.json")
                .is_some()
        }),
        "the persistent panel was never written"
    );

    let dir = ScratchDir {
        key: server.space.sha256_hex(),
        panel: "shown".to_string(),
    };
    let stored = [
        server.store.bytes(&server.space, "panels/kept/panel.json"),
        server.store.bytes(&server.space, "panels/kept/content.md"),
        server.scratch.bytes(&dir, "panel.json"),
        server.scratch.bytes(&dir, "diagram.mmd"),
        server.scratch.bytes(&dir, "scratch.json"),
    ];
    for (panel, bytes) in [("kept", &stored[0]), ("shown", &stored[2])] {
        let bytes = bytes
            .as_ref()
            .unwrap_or_else(|| panic!("{panel} has no panel.json"));
        let file = parse_view_panel(panel, bytes).unwrap();
        assert_eq!(
            file.created_by,
            Some(CreatedBy::Agent { slot: None }),
            "{panel}"
        );
    }
    for bytes in stored.iter().flatten() {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("fake:"), "{text}");
        assert!(!text.contains(&id), "{text}");
    }
}

// AC12

#[test]
fn the_mcp_cap_refuses_the_third_shim_and_frees_slots() {
    let server = server_ready(server_with(ServeOptions {
        max_mcp_connections: 2,
        ..options(Duration::from_millis(200))
    }));
    let mut first = shim_for(&server);
    let mut second = shim_for(&server);
    let stats = server.server.stats();
    assert_eq!(stats.mcp_channels(), 2);

    let mut third = Shim::spawn(&server, ShimEnv::new(&server));
    let status = third.wait_exit(EXIT_WAIT);
    assert_eq!(status.code(), Some(1), "{}", third.stderr());
    assert!(
        third
            .stderr()
            .contains("too many MCP connections; the limit is 2"),
        "{}",
        third.stderr()
    );

    first.close_stdin();
    assert!(first.wait_exit(EXIT_WAIT).success());
    assert!(
        wait_until(Duration::from_secs(5), || stats.mcp_channels() == 1),
        "the closed shim's slot was never freed"
    );
    let mut fourth = shim_for(&server);
    assert_eq!(fourth.request("ping", json!({}))["result"], json!({}));
    assert_eq!(second.request("ping", json!({}))["result"], json!({}));
}

// AC15

fn assert_error(line: &Value, code: i64, id: &Value) {
    assert_eq!(line["jsonrpc"], "2.0", "{line}");
    assert_eq!(line["error"]["code"], code, "{line}");
    assert_eq!(&line["id"], id, "{line}");
}

/// A `ping` whose line is exactly `len` bytes, padded in `params`.
fn padded_ping(id: u64, len: usize) -> Vec<u8> {
    let head = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"ping","params":{{"pad":""#);
    let tail = r#""}}"#;
    let pad = len - head.len() - tail.len();
    let mut line = head.into_bytes();
    line.extend(std::iter::repeat(b'x').take(pad));
    line.extend_from_slice(tail.as_bytes());
    assert_eq!(line.len(), len);
    line
}

#[test]
fn the_shim_answers_malformed_lines_itself_and_keeps_going() {
    let server = server();
    let mut shim = shim_for(&server);
    let wait = Duration::from_secs(10);

    shim.send_line(b"this is not json");
    assert_error(&shim.next_line(wait), -32700, &Value::Null);

    shim.send_line(b"[1, 2]");
    assert_error(&shim.next_line(wait), -32600, &Value::Null);

    shim.send_line(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"\xff\"}");
    assert_error(&shim.next_line(wait), -32700, &Value::Null);

    shim.send_line(&padded_ping(77, 1_048_560));
    assert_error(&shim.next_line(wait), -32600, &json!(77));

    shim.send_line(&padded_ping(78, 2 * 1024 * 1024));
    assert_error(&shim.next_line(wait), -32600, &Value::Null);

    let reply = shim.request("ping", json!({}));
    assert_eq!(reply["result"], json!({}), "{reply}");
    assert!(shim.unread().is_empty());
}

// AC19

fn assert_closed_with_exit_1(shim: &mut Shim) {
    let status = shim.wait_exit(Duration::from_secs(5));
    assert_eq!(status.code(), Some(1), "stderr: {}", shim.stderr());
    assert!(
        shim.stderr().contains("closed the connection"),
        "{}",
        shim.stderr()
    );
}

#[test]
fn the_shim_exits_1_when_the_daemon_stops() {
    let mut idle = server();
    let mut shim = shim_for(&idle);
    assert!(idle.server.shutdown().is_none());
    assert_closed_with_exit_1(&mut shim);

    let mut busy = server_ready(server_with(options(Duration::from_secs(30))));
    let mut shim = shim_for(&busy);
    let hold = busy.check.hold(MERMAID);
    let call = shim.start_call("create_panel", mermaid(MERMAID));
    assert!(
        wait_until(Duration::from_secs(10), || busy.check.in_flight() == 1),
        "the check never started"
    );
    assert!(busy.server.shutdown().is_none());
    assert_closed_with_exit_1(&mut shim);
    assert!(shim.try_reply(call).is_none());
    drop(hold);
}

#[test]
fn the_shim_exits_1_after_a_restart() {
    let mut server = server();
    let mut shim = shim_for(&server);
    server.restart();
    assert_closed_with_exit_1(&mut shim);
}

#[test]
fn the_shim_exits_0_when_stdin_closes() {
    let server = server();
    let mut shim = shim_for(&server);
    shim.close_stdin();
    let status = shim.wait_exit(EXIT_WAIT);
    assert_eq!(status.code(), Some(0), "stderr: {}", shim.stderr());
    assert_eq!(shim.stderr(), "");
    assert!(shim.unread().is_empty());
}
