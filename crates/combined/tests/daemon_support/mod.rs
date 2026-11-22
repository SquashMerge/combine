#![allow(dead_code)]
//! Scratch paths and safety checks shared by the daemon's test crates. Every test passes
//! explicit paths under its own scratch directory and never touches a default location.

use std::fs;
use std::io::Write;
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use combine_protocol::{
    client_handshake, read_message, write_message, Ack, ClientMessage, Cursor, DaemonMessage,
    HostId, Welcome,
};
use combined::adapters::fs::data_dir::DataDir;
use combined::adapters::fs::file_store::FsFileStore;
use combined::adapters::fs::git_probe::FsGitProbe;
use combined::core::{Core, CoreConfig, Driver, TerminalList};
use combined::ports::TerminalBackend;
use combined::rpc::{self, ServeOptions, ServerHandle, ServerParts};
use combined::testing::FakeBackend;
use combined::wiring::clock::MonotonicClock;
use combined::wiring::config::{self, DaemonEnv};
use combined::wiring::ids::IdReservation;

/// The longest socket path macOS accepts.
const MAX_SOCKET_PATH: usize = 103;

static NEXT: AtomicU32 = AtomicU32::new(0);

/// `/tmp/cs4-<pid>-<n>`, mode 0700, removed on drop.
pub struct ShortTempDir {
    path: PathBuf,
}

impl ShortTempDir {
    pub fn new() -> ShortTempDir {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let path = PathBuf::from(format!("/tmp/cs4-{}-{n}", std::process::id()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        ShortTempDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A socket path in the scratch dir, checked against the length limit.
    pub fn sock(&self, name: &str) -> PathBuf {
        let path = self.path.join(name);
        assert!(
            path.as_os_str().len() <= MAX_SOCKET_PATH,
            "socket path {} is over {MAX_SOCKET_PATH} bytes",
            path.display()
        );
        path
    }
}

impl Drop for ShortTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Panics unless `path` is inside the scratch dir and has no `..` component.
pub fn check_in_scratch(path: &Path, scratch: &ShortTempDir) {
    assert!(
        !path.components().any(|part| part == Component::ParentDir),
        "{} has a `..` component",
        path.display()
    );
    assert!(
        path.starts_with(scratch.path()),
        "{} is outside the scratch dir {}",
        path.display(),
        scratch.path().display()
    );
}

/// Panics on a path outside the scratch dir and on herdr-core's default socket for the
/// process's own environment.
pub fn refuse_default_herdr_core(path: &Path, scratch: &ShortTempDir) {
    refuse_default_herdr_core_in(path, scratch, &DaemonEnv::from_process());
}

/// As [`refuse_default_herdr_core`], against the environment `env` describes.
pub fn refuse_default_herdr_core_in(path: &Path, scratch: &ShortTempDir, env: &DaemonEnv) {
    check_in_scratch(path, scratch);
    if let Ok((default, _)) = config::herdr_core_socket(None, env) {
        assert!(
            path != default,
            "{} is herdr-core's default socket",
            path.display()
        );
    }
}

/// The three paths a daemon needs, all inside the scratch dir.
#[derive(Debug, Clone)]
pub struct DaemonPaths {
    /// The directory under the scratch dir that holds this daemon's config, log and data.
    pub name: String,
    pub data_dir: PathBuf,
    pub socket: PathBuf,
    pub herdr_core_socket: PathBuf,
}

impl DaemonPaths {
    pub fn in_scratch(scratch: &ShortTempDir, name: &str) -> DaemonPaths {
        let base = scratch.path().join(name);
        let data_dir = base.join("data");
        DaemonPaths {
            name: name.to_string(),
            socket: data_dir.join("combined.sock"),
            data_dir,
            herdr_core_socket: base.join("no-hc.sock"),
        }
    }

    pub fn with_socket(mut self, scratch: &ShortTempDir, socket: PathBuf) -> DaemonPaths {
        refuse_default_herdr_core(&socket, scratch);
        self.socket = socket;
        self
    }

    pub fn with_data_dir(mut self, scratch: &ShortTempDir, data_dir: PathBuf) -> DaemonPaths {
        check_in_scratch(&data_dir, scratch);
        self.data_dir = data_dir;
        self
    }

    pub fn with_herdr_core_socket(
        mut self,
        scratch: &ShortTempDir,
        socket: PathBuf,
    ) -> DaemonPaths {
        refuse_default_herdr_core(&socket, scratch);
        self.herdr_core_socket = socket;
        self
    }
}

/// Writes `<scratch>/<name>/daemon.json` with every field set, and returns its path. Two
/// daemons with different names get different files.
pub fn write_daemon_config(scratch: &ShortTempDir, paths: &DaemonPaths, backend: &str) -> PathBuf {
    for path in [&paths.data_dir, &paths.socket, &paths.herdr_core_socket] {
        refuse_default_herdr_core(path, scratch);
    }
    let dir = scratch.path().join(&paths.name);
    fs::create_dir_all(&dir).unwrap();
    let config = dir.join("daemon.json");
    let text = serde_json::json!({
        "version": 1,
        "backend": backend,
        "herdr_core_socket": paths.herdr_core_socket,
        "data_dir": paths.data_dir,
        "socket": paths.socket,
    });
    fs::write(&config, serde_json::to_vec_pretty(&text).unwrap()).unwrap();
    config
}

/// Polls `cond` until it holds or `timeout` passes; returns whether it held.
pub fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// How long a test waits for the daemon to send something before it fails.
pub const RECV_TIMEOUT: Duration = Duration::from_secs(5);

/// A client of the daemon's socket. A thread decodes the daemon's frames as they arrive.
pub struct WireClient {
    stream: UnixStream,
    frames: mpsc::Receiver<DaemonMessage>,
}

impl WireClient {
    /// Connects without a handshake.
    pub fn connect(socket: &Path) -> WireClient {
        let stream = UnixStream::connect(socket).unwrap();
        let mut reader = stream.try_clone().unwrap();
        let (sender, frames) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(message)) = read_message::<DaemonMessage, _>(&mut reader) {
                if sender.send(message).is_err() {
                    break;
                }
            }
        });
        WireClient { stream, frames }
    }

    /// Connects and says `hello` at the current protocol version.
    pub fn open(socket: &Path) -> (WireClient, Welcome) {
        let mut client = WireClient::connect(socket);
        client.send(&ClientMessage::Hello(combine_protocol::Hello {
            protocol_version: combine_protocol::PROTOCOL_VERSION,
        }));
        match client.recv() {
            DaemonMessage::Welcome(welcome) => (client, welcome),
            other => panic!("expected welcome, got {other:?}"),
        }
    }

    pub fn send(&mut self, message: &ClientMessage) {
        write_message(&mut self.stream, message).unwrap();
        self.stream.flush().unwrap();
    }

    pub fn send_bytes(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).unwrap();
        self.stream.flush().unwrap();
    }

    /// Closes the write side, so the daemon reads end of stream after what was sent.
    pub fn shutdown_write(&self) {
        self.stream.shutdown(Shutdown::Write).unwrap();
    }

    /// The next message; panics if none arrives within [`RECV_TIMEOUT`] or the daemon closed.
    pub fn recv(&self) -> DaemonMessage {
        match self.frames.recv_timeout(RECV_TIMEOUT) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => panic!("no message within {RECV_TIMEOUT:?}"),
            Err(RecvTimeoutError::Disconnected) => panic!("the daemon closed the connection"),
        }
    }

    /// The next message, or `None` if none arrives within `wait` or the daemon closed.
    pub fn recv_within(&self, wait: Duration) -> Option<DaemonMessage> {
        self.frames.recv_timeout(wait).ok()
    }

    /// Panics unless the daemon closes the connection, with nothing more to read, within
    /// [`RECV_TIMEOUT`].
    pub fn expect_closed(&self) {
        match self.frames.recv_timeout(RECV_TIMEOUT) {
            Err(RecvTimeoutError::Disconnected) => {}
            Err(RecvTimeoutError::Timeout) => panic!("the daemon did not close the connection"),
            Ok(message) => panic!("expected the close, got {message:?}"),
        }
    }

    /// Acknowledges everything up to `cursor`.
    pub fn ack(&mut self, cursor: Cursor) {
        self.send(&ClientMessage::Ack(Ack { cursor }));
    }
}

impl Drop for WireClient {
    /// Closes the socket even though the decoder thread still holds a handle to it.
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// Connects, completes the handshake and then never reads again: the daemon's writes pile up.
pub fn connect_and_never_read(socket: &Path) -> UnixStream {
    let mut stream = UnixStream::connect(socket).unwrap();
    let mut reader = stream.try_clone().unwrap();
    client_handshake(&mut reader, &mut stream).unwrap();
    stream
}

/// `<scratch>/work`: where in-process servers' terminals run.
pub fn work_dir(scratch: &ShortTempDir) -> PathBuf {
    scratch.path().join("work")
}

/// Starts a server in this process on the fake backend, with every path explicit and inside
/// the scratch dir. `<scratch>/work/.git/` is created, so a terminal spawned in `work_dir`
/// is in a repository space.
pub fn fake_server(
    scratch: &ShortTempDir,
    name: &str,
    backend: Arc<FakeBackend>,
    host: &str,
    options: ServeOptions,
) -> ServerHandle {
    let paths = DaemonPaths::in_scratch(scratch, name);
    let work = work_dir(scratch);
    for path in [
        &paths.data_dir,
        &paths.socket,
        &paths.herdr_core_socket,
        &work,
    ] {
        check_in_scratch(path, scratch);
    }
    fs::create_dir_all(work.join(".git")).unwrap();

    let data_dir = DataDir::open(&paths.data_dir).unwrap();
    let (ids, id_base) = IdReservation::start(&data_dir.ids_json()).unwrap();
    let socket = rpc::socket::bind(&paths.socket).unwrap();
    let host_id = HostId(host.to_string());
    rpc::spawn(
        socket,
        move || {
            let core = Core::new(
                CoreConfig {
                    id_base,
                    ..CoreConfig::default()
                },
                backend.capabilities(),
            )
            .unwrap();
            let backend: Arc<dyn TerminalBackend> = backend;
            // Each test serves its own data dir, so no other writer needs the lock.
            drop(data_dir);
            ServerParts {
                driver: Driver::new(core, backend),
                terminals: TerminalList::new(Box::new(FsGitProbe)),
                host_id,
                ids,
                clock: MonotonicClock::new(),
                file_store: Arc::new(FsFileStore::new()),
            }
        },
        options,
    )
    .unwrap()
}

/// `combined` with a cleared environment: only `HOME=<scratch>/home`, so nothing inherits
/// `HC_*` or `XDG_*`.
pub fn combined_command(scratch: &ShortTempDir) -> Command {
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_combined"));
    command.env_clear().env("HOME", home);
    command
}

/// Waits for `child` to exit, killing it and panicking if it takes longer than `timeout`.
fn wait_for_exit(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the process did not exit within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// A real `combined serve`, killed and reaped on drop. Its stderr goes to
/// `<scratch>/<name>/daemon.log`.
pub struct DaemonProcess {
    child: Child,
    socket: PathBuf,
    log: PathBuf,
}

impl DaemonProcess {
    /// Writes the config for `paths` and starts the daemon on it.
    pub fn spawn(scratch: &ShortTempDir, paths: &DaemonPaths, backend: &str) -> DaemonProcess {
        let config = write_daemon_config(scratch, paths, backend);
        DaemonProcess::spawn_with(scratch, paths, &["serve", "--config"], &config)
    }

    /// Starts `combined <args> <config>` and logs under `paths.name`.
    pub fn spawn_with(
        scratch: &ShortTempDir,
        paths: &DaemonPaths,
        args: &[&str],
        config: &Path,
    ) -> DaemonProcess {
        check_in_scratch(config, scratch);
        let log = scratch.path().join(&paths.name).join("daemon.log");
        let log_file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .unwrap();
        let child = combined_command(scratch)
            .args(args)
            .arg(config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log_file)
            .spawn()
            .unwrap();
        DaemonProcess {
            child,
            socket: paths.socket.clone(),
            log,
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Everything the daemon has written to stderr.
    pub fn log(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Waits until the socket accepts, and panics if the daemon exits first.
    pub fn wait_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "the daemon exited with {status} before it served:\n{}",
                    self.log()
                );
            }
            if UnixStream::connect(&self.socket).is_ok() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the daemon did not serve:\n{}",
                self.log()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn is_running(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }

    /// Waits for the daemon to exit by itself.
    pub fn wait_exit(&mut self) -> ExitStatus {
        wait_for_exit(&mut self.child, Duration::from_secs(10))
    }

    /// Kills the daemon and waits for it, as a crash would.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

static NEXT_BRIDGE: AtomicU32 = AtomicU32::new(0);

/// A `combined connect --socket` process spoken to over its stdin and stdout, killed and reaped
/// on drop. Its stderr goes to a file in the scratch dir.
pub struct BridgeClient {
    child: Child,
    stdin: Option<ChildStdin>,
    frames: mpsc::Receiver<DaemonMessage>,
    stderr: PathBuf,
}

impl BridgeClient {
    pub fn spawn(scratch: &ShortTempDir, socket: &Path) -> BridgeClient {
        BridgeClient::spawn_command(scratch, &["connect", "--socket"], socket)
    }

    fn spawn_command(scratch: &ShortTempDir, args: &[&str], socket: &Path) -> BridgeClient {
        check_in_scratch(socket, scratch);
        let n = NEXT_BRIDGE.fetch_add(1, Ordering::SeqCst);
        let stderr = scratch.path().join(format!("bridge-{n}.err"));
        let mut child = combined_command(scratch)
            .args(args)
            .arg(socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().unwrap();
        let (sender, frames) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(message)) = read_message::<DaemonMessage, _>(&mut stdout) {
                if sender.send(message).is_err() {
                    break;
                }
            }
        });
        BridgeClient {
            child,
            stdin,
            frames,
            stderr,
        }
    }

    /// Connects through the bridge and says `hello` at the current protocol version.
    pub fn open(scratch: &ShortTempDir, socket: &Path) -> (BridgeClient, Welcome) {
        let mut bridge = BridgeClient::spawn(scratch, socket);
        bridge.send(&ClientMessage::Hello(combine_protocol::Hello {
            protocol_version: combine_protocol::PROTOCOL_VERSION,
        }));
        match bridge.recv() {
            DaemonMessage::Welcome(welcome) => (bridge, welcome),
            other => panic!("expected welcome, got {other:?}"),
        }
    }

    pub fn send(&mut self, message: &ClientMessage) {
        let stdin = self.stdin.as_mut().expect("stdin is closed");
        write_message(stdin, message).unwrap();
        stdin.flush().unwrap();
    }

    pub fn send_bytes(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("stdin is closed");
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    pub fn ack(&mut self, cursor: Cursor) {
        self.send(&ClientMessage::Ack(Ack { cursor }));
    }

    /// The next message from the bridge's stdout; panics if none arrives in [`RECV_TIMEOUT`].
    pub fn recv(&self) -> DaemonMessage {
        match self.frames.recv_timeout(RECV_TIMEOUT) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => panic!("no message within {RECV_TIMEOUT:?}"),
            Err(RecvTimeoutError::Disconnected) => panic!("the bridge closed its stdout"),
        }
    }

    /// Closes the bridge's stdin: the bridge half-closes the socket.
    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Waits for the bridge to exit by itself.
    pub fn wait_exit(&mut self) -> ExitStatus {
        wait_for_exit(&mut self.child, Duration::from_secs(10))
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// What the bridge wrote to stderr.
    pub fn stderr(&self) -> String {
        fs::read_to_string(&self.stderr).unwrap_or_default()
    }
}

impl Drop for BridgeClient {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Runs a command to completion, killing it if it takes longer than `timeout`. Its stdin is the
/// caller's to set.
pub fn run_to_exit(mut command: Command, timeout: Duration) -> std::process::Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut bytes);
        bytes
    });
    let err = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr, &mut bytes);
        bytes
    });
    let status = wait_for_exit(&mut child, timeout);
    std::process::Output {
        status,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}
