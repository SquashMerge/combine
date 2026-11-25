#![allow(dead_code)]
//! Test support for the shim: an in-process daemon, and the real `combine mcp` binary run against
//! it over stdio.

mod daemon;

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use combined::testing::check_in_scratch;
use serde_json::{json, Value};

pub use daemon::{PanelServer, PanelServerConfig, TestDiagramCheck};

/// How long a test waits for a reply the daemon gives by itself, past the tool wait.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// What a shim runs with. Nothing else is inherited: the environment is cleared.
pub struct ShimEnv {
    /// The `--socket` flag. `None` leaves the lookup to `daemon.json`.
    pub socket: Option<PathBuf>,
    pub home: PathBuf,
    /// The working directory, also given as `PWD`.
    pub cwd: PathBuf,
    pub terminal_id: Option<String>,
    pub terminal_socket: Option<String>,
}

impl ShimEnv {
    /// The server's socket by flag, its project folder as the working directory, no terminal.
    pub fn new(server: &PanelServer) -> ShimEnv {
        ShimEnv {
            socket: Some(server.socket.clone()),
            home: server.dir.path().join("home"),
            cwd: combined::testing::work_dir(&server.dir),
            terminal_id: None,
            terminal_socket: None,
        }
    }
}

/// What a `tools/call` answered.
#[derive(Debug)]
pub struct ToolReply {
    pub is_error: bool,
    pub body: Value,
}

impl ToolReply {
    /// The error's code, for an error result.
    pub fn code(&self) -> &str {
        assert!(self.is_error, "not an error: {}", self.body);
        self.body["error"]["code"].as_str().unwrap()
    }
}

/// Reads stdout lines, parsing each: one that isn't a whole JSON line is reported as corruption.
fn read_stdout(
    stdout: impl Read + Send + 'static,
    lines: mpsc::Sender<Result<Value, String>>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        loop {
            let mut line = Vec::new();
            match stdout.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let parsed = match line.strip_suffix(b"\n") {
                None => Err(format!(
                    "a stdout line has no newline: {}",
                    String::from_utf8_lossy(&line)
                )),
                Some(text) => serde_json::from_slice(text).map_err(|e| {
                    format!(
                        "a stdout line isn't JSON ({e}): {}",
                        String::from_utf8_lossy(text)
                    )
                }),
            };
            if lines.send(parsed).is_err() {
                return;
            }
        }
    })
}

fn read_stderr(stderr: impl Read + Send + 'static, text: Arc<Mutex<String>>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut stderr = stderr;
        let mut buffer = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buffer) {
            if n == 0 {
                return;
            }
            text.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push_str(&String::from_utf8_lossy(&buffer[..n]));
        }
    })
}

/// A running `combine mcp`.
pub struct Shim {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Result<Value, String>>,
    /// Lines read from stdout and not yet taken by a test.
    unread: Vec<Value>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
    next_id: u64,
}

impl Shim {
    /// Starts the binary with a cleared environment and only the values `env` names. Every path
    /// it is given must be inside the server's scratch dir.
    pub fn spawn(server: &PanelServer, env: ShimEnv) -> Shim {
        std::fs::create_dir_all(&env.home).unwrap();
        std::fs::create_dir_all(&env.cwd).unwrap();
        check_in_scratch(&env.home, &server.dir);
        check_in_scratch(&env.cwd, &server.dir);
        let mut command = Command::new(env!("CARGO_BIN_EXE_combine"));
        command.arg("mcp");
        if let Some(socket) = &env.socket {
            check_in_scratch(socket, &server.dir);
            command.arg("--socket").arg(socket);
        }
        command
            .env_clear()
            .env("HOME", &env.home)
            .env("PWD", &env.cwd)
            .current_dir(&env.cwd);
        if let Some(id) = &env.terminal_id {
            command.env("HC_TERMINAL_ID", id);
        }
        if let Some(socket) = &env.terminal_socket {
            check_in_scratch(std::path::Path::new(socket), &server.dir);
            command.env("HC_TERMINAL_SOCKET", socket);
        }
        Shim::start(command)
    }

    fn start(mut command: Command) -> Shim {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (sender, lines) = mpsc::channel();
        let stderr = Arc::new(Mutex::new(String::new()));
        let readers = vec![
            read_stdout(child.stdout.take().unwrap(), sender),
            read_stderr(child.stderr.take().unwrap(), Arc::clone(&stderr)),
        ];
        Shim {
            stdin: child.stdin.take(),
            child,
            lines,
            unread: Vec::new(),
            stderr,
            readers,
            next_id: 0,
        }
    }

    /// Writes `bytes` and a newline to the shim's stdin.
    pub fn send_line(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("stdin is closed");
        stdin.write_all(bytes).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    pub fn send(&mut self, message: &Value) {
        self.send_line(message.to_string().as_bytes());
    }

    /// Sends a request with a new id and returns the id.
    pub fn send_request(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    /// Sends a `tools/call` without waiting for its reply, and returns its id.
    pub fn start_call(&mut self, tool: &str, arguments: Value) -> u64 {
        self.send_request("tools/call", json!({"name": tool, "arguments": arguments}))
    }

    /// Moves what the shim has written so far into `unread`, waiting up to `wait` for the first
    /// line. Panics on a line that isn't JSON. False once stdout has closed.
    fn pump(&mut self, wait: Duration) -> bool {
        let mut first = true;
        loop {
            let line = if first && !wait.is_zero() {
                match self.lines.recv_timeout(wait) {
                    Ok(line) => line,
                    Err(RecvTimeoutError::Timeout) => return true,
                    Err(RecvTimeoutError::Disconnected) => return false,
                }
            } else {
                match self.lines.try_recv() {
                    Ok(line) => line,
                    Err(TryRecvError::Empty) => return true,
                    Err(TryRecvError::Disconnected) => return false,
                }
            };
            first = false;
            match line {
                Ok(value) => self.unread.push(value),
                Err(corrupt) => panic!("{corrupt}; stderr: {}", self.stderr()),
            }
        }
    }

    fn take_reply(&mut self, id: u64) -> Option<Value> {
        let at = self.unread.iter().position(|line| line["id"] == id)?;
        Some(self.unread.remove(at))
    }

    /// The reply to request `id` if it has already arrived.
    pub fn try_reply(&mut self, id: u64) -> Option<Value> {
        self.pump(Duration::ZERO);
        self.take_reply(id)
    }

    /// The reply to request `id`, or `None` if none comes within `timeout`.
    pub fn reply(&mut self, id: u64, timeout: Duration) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(reply) = self.try_reply(id) {
                return Some(reply);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || !self.pump(left.min(Duration::from_millis(50))) {
                return self.take_reply(id);
            }
        }
    }

    /// The oldest line the shim wrote that no test has taken, whatever its id.
    pub fn next_line(&mut self, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            self.pump(Duration::ZERO);
            if !self.unread.is_empty() {
                return self.unread.remove(0);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero() && self.pump(left.min(Duration::from_millis(50))),
                "no stdout line within {timeout:?}; stderr: {}",
                self.stderr()
            );
        }
    }

    /// Sends a request and returns its reply.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send_request(method, params);
        self.reply(id, REPLY_TIMEOUT)
            .unwrap_or_else(|| panic!("no reply to {method}; stderr: {}", self.stderr()))
    }

    /// Calls a tool and returns its result, with the text parsed.
    pub fn call_tool(&mut self, tool: &str, arguments: Value) -> ToolReply {
        let id = self.start_call(tool, arguments);
        let reply = self
            .reply(id, REPLY_TIMEOUT)
            .unwrap_or_else(|| panic!("no reply to {tool}; stderr: {}", self.stderr()));
        tool_reply(&reply)
    }

    /// Waits until the caller's space has read its panels: before that, `list_panels` answers
    /// `space_loading`. Returns the answer of the first `list_panels` that worked.
    pub fn wait_loaded(&mut self) -> ToolReply {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let listed = self.call_tool("list_panels", json!({}));
            if !listed.is_error {
                return listed;
            }
            assert!(
                Instant::now() < deadline,
                "the space never loaded: {listed:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Closes the shim's stdin.
    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Waits for the process to exit, then for both readers: afterwards `stderr` is complete and
    /// every stdout line has been checked. A shim still running at `timeout` is killed, and the
    /// test fails.
    pub fn wait_exit(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                self.child.kill().unwrap();
                self.child.wait().unwrap();
                panic!(
                    "the shim was still running after {timeout:?}; stderr: {}",
                    self.stderr()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
        self.pump(Duration::ZERO);
        status
    }

    /// Whatever the shim has written to stderr so far.
    pub fn stderr(&self) -> String {
        self.stderr
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The lines read and not yet taken, after pulling in what has arrived.
    pub fn unread(&mut self) -> &[Value] {
        self.pump(Duration::ZERO);
        &self.unread
    }
}

impl Drop for Shim {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The result of a `tools/call` reply.
pub fn tool_reply(reply: &Value) -> ToolReply {
    let result = &reply["result"];
    assert!(result.is_object(), "not a tool result: {reply}");
    let text = result["content"][0]["text"].as_str().unwrap();
    ToolReply {
        is_error: result["isError"].as_bool().unwrap(),
        body: serde_json::from_str(text).unwrap(),
    }
}
