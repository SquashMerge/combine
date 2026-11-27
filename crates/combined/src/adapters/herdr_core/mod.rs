//! The herdr-core adapter: [`TerminalBackend`] over herdr-core's framed protocol on a Unix
//! socket the caller names. It is the only code that speaks that protocol (SPEC.md, "Terminal
//! backends").
//!
//! A `TermId` is `hc:<address>`. Each attachment is a pair of viewers (see `pair.rs`), and the
//! adapter keeps the pair alive across a dropped half or a restarted core.

mod attachment;
mod caller;
mod connection;
#[cfg(feature = "test-support")]
mod control;
mod events;
mod fencing;
mod frame;
mod jsonrpc;
mod pair;
mod watch;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use combine_protocol::TermId;
use futures::stream::BoxStream;
use serde_json::{json, Value};

use self::attachment::Fidelities;
use self::connection::{CallError, Connection, Opened};
use self::jsonrpc::{address_of, is_remote, term_id, Hello};
use self::pair::{OpenError, OpenParams};
use crate::model::{SpawnSpec, TermEvent, TermInfo};
use crate::ports::{
    AttachOptions, Attachment, BackendError, BackendResult, Capabilities, TerminalBackend,
};

pub use self::caller::HerdrCoreCallerIdentity;
#[cfg(feature = "test-support")]
pub use self::control::{DeliveryWait, HerdrCoreControl, PairStatus};

/// The backend's `id()`.
pub const ID: &str = "herdr-core";
/// The prefix of every `TermId` it makes.
pub const PREFIX: &str = "hc";

/// Data bytes a new display stream may send before any credit (herdr-core's own allowance).
const INITIAL_CREDIT: u32 = 262_144;

/// Where the daemon finds herdr-core, and how hard the adapter tries to keep a pair.
#[derive(Debug, Clone)]
pub struct HerdrCoreConfig {
    /// The core's socket. Always explicit: the adapter never looks one up.
    pub socket: PathBuf,
    /// Reopen attempts before an attachment, or the watcher, gives up.
    pub reopen_attempts: u32,
    pub reopen_interval: Duration,
    /// How long a control call keeps retrying to connect.
    pub control_connect_window: Duration,
}

impl HerdrCoreConfig {
    /// 40 attempts 250 ms apart, and control calls retry to connect for 2 s.
    pub fn new(socket: PathBuf) -> Self {
        HerdrCoreConfig {
            socket,
            reopen_attempts: 40,
            reopen_interval: Duration::from_millis(250),
            control_connect_window: Duration::from_secs(2),
        }
    }
}

/// What every task of one backend shares.
struct Env {
    pub(super) config: HerdrCoreConfig,
    /// The highest ownership generation any request for each terminal has carried.
    pub(super) highest_ownership: Mutex<HashMap<String, u64>>,
    pub(super) fidelities: Mutex<Fidelities>,
    /// The core lists `automatic_input` among its features.
    pub(super) automatic_feature: bool,
    /// The core's id when the backend started, if `hello` carried a usable one.
    pub(super) core_id: Option<String>,
    #[cfg(feature = "test-support")]
    pub(super) control: Arc<control::Shared>,
}

/// Why a connection to the core didn't open.
pub(super) enum OpenFailure {
    Backend(BackendError),
    /// The core answered, but it isn't the core this backend started with.
    CoreChanged,
}

impl Env {
    /// Opens a connection and checks that the core behind it is the one the backend started
    /// with. A core whose state was wiped has a new id, and would hand out terminal IDs again.
    async fn open(&self) -> Result<Opened, OpenFailure> {
        let opened = Connection::open(&self.config.socket)
            .await
            .map_err(OpenFailure::Backend)?;
        if opened.0.hello().core_id != self.core_id {
            return Err(OpenFailure::CoreChanged);
        }
        Ok(opened)
    }

    /// The failure a caller of the backend sees. Nothing matches on its text.
    fn core_changed(&self) -> BackendError {
        BackendError::Failed(format!(
            "herdr-core on {} is another core now",
            self.config.socket.display()
        ))
    }

    fn refusal(&self, failure: OpenFailure) -> BackendError {
        match failure {
            OpenFailure::Backend(error) => error,
            OpenFailure::CoreChanged => self.core_changed(),
        }
    }

    fn watch_credit_stalled(&self) -> bool {
        #[cfg(feature = "test-support")]
        return self.control.watch_credit_stalled();
        #[cfg(not(feature = "test-support"))]
        false
    }

    fn record_subscribe(&self, result: &Value) {
        #[cfg(feature = "test-support")]
        self.control.record_subscribe(result);
        #[cfg(not(feature = "test-support"))]
        let _ = result;
    }

    fn record_raw(&self, line: &str) {
        #[cfg(feature = "test-support")]
        self.control.record_raw(line);
        #[cfg(not(feature = "test-support"))]
        let _ = line;
    }
}

pub struct HerdrCoreBackend {
    env: Arc<Env>,
    version: String,
}

impl HerdrCoreBackend {
    /// Checks the core with `hello` and returns the backend. The socket is not looked up and
    /// not created: a core that isn't there is an error.
    pub async fn connect(config: HerdrCoreConfig) -> BackendResult<HerdrCoreBackend> {
        let (connection, _frames) = Connection::open(&config.socket).await?;
        let hello = connection.hello().clone();
        Ok(HerdrCoreBackend::with_hello(config, hello))
    }

    /// Like `connect`, with the handle that drives and observes the backend.
    #[cfg(feature = "test-support")]
    pub async fn connect_with_control(
        config: HerdrCoreConfig,
    ) -> BackendResult<(HerdrCoreBackend, HerdrCoreControl)> {
        let backend = HerdrCoreBackend::connect(config).await?;
        let control = HerdrCoreControl::new(Arc::clone(&backend.env.control));
        Ok((backend, control))
    }

    fn with_hello(config: HerdrCoreConfig, hello: Hello) -> HerdrCoreBackend {
        HerdrCoreBackend {
            env: Arc::new(Env {
                config,
                highest_ownership: Mutex::new(HashMap::new()),
                fidelities: Mutex::new(Fidelities::default()),
                automatic_feature: hello.has_feature("automatic_input"),
                core_id: hello.core_id,
                #[cfg(feature = "test-support")]
                control: Arc::new(control::Shared::default()),
            }),
            version: hello.version,
        }
    }

    /// The core's version, from `hello`.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The address of a terminal on this machine, before any call goes out.
    fn resolve(&self, id: &TermId) -> BackendResult<String> {
        let address = address_of(id).ok_or_else(|| BackendError::UnknownTerminal(id.clone()))?;
        if is_remote(address) {
            return Err(BackendError::RemoteTerminal(id.clone()));
        }
        Ok(address.to_string())
    }

    /// One call on its own connection. Connecting is retried for the configured window, so a
    /// call made while the core restarts waits for it.
    async fn call(&self, method: &str, params: Value) -> Result<Value, CallError> {
        let config = &self.env.config;
        let started = tokio::time::Instant::now();
        let (connection, _frames) = loop {
            match self.env.open().await {
                Ok(opened) => break opened,
                Err(OpenFailure::CoreChanged) => {
                    return Err(CallError::Failed(self.env.core_changed().to_string()));
                }
                Err(OpenFailure::Backend(error)) => {
                    if started.elapsed() >= config.control_connect_window {
                        return Err(CallError::Failed(error.to_string()));
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        };
        connection.call(method, params).await
    }
}

fn capabilities(automatic: bool) -> Capabilities {
    Capabilities {
        ordered_attach: true,
        byte_credit: true,
        initial_credit: INITIAL_CREDIT,
        display_queries: true,
        automatic_replies: automatic,
        resync_signal: false,
        agent_state: false,
    }
}

#[async_trait]
impl TerminalBackend for HerdrCoreBackend {
    fn id(&self) -> &'static str {
        ID
    }

    fn instance(&self) -> Option<String> {
        self.env.core_id.clone()
    }

    fn capabilities(&self) -> Capabilities {
        capabilities(self.env.automatic_feature)
    }

    async fn list(&self) -> BackendResult<Vec<TermInfo>> {
        let result = self
            .call("terminal.list", json!({}))
            .await
            .map_err(|error| error.to_failure())?;
        jsonrpc::list_result(&result)
    }

    async fn watch(&self) -> BackendResult<BoxStream<'static, TermEvent>> {
        watch::start(Arc::clone(&self.env)).await
    }

    async fn spawn(&self, spec: SpawnSpec) -> BackendResult<TermId> {
        if spec.cols == 0 || spec.rows == 0 {
            return Err(BackendError::InvalidRequest(
                "a terminal needs at least one column and one row".to_string(),
            ));
        }
        let result = self
            .call("terminal.run", jsonrpc::run_params(&spec))
            .await
            .map_err(|error| error.to_failure())?;
        Ok(term_id(&jsonrpc::run_result(&result)?))
    }

    async fn attach(&self, id: &TermId, opts: AttachOptions) -> BackendResult<Box<dyn Attachment>> {
        let address = self.resolve(id)?;
        #[cfg(feature = "test-support")]
        let _opening = self.env.control.opening();
        let params = OpenParams {
            address: &address,
            size: (opts.cols, opts.rows),
            display: &opts.display,
            history: opts.history,
            resumed: opts.resumed,
            watch_only: opts.observe,
        };
        let halves = pair::open(&self.env, &params)
            .await
            .map_err(|error| match error {
                OpenError::Finished | OpenError::NotFound => {
                    BackendError::UnknownTerminal(id.clone())
                }
                OpenError::Failed(error) => error,
            })?;
        Ok(Box::new(attachment::start(
            Arc::clone(&self.env),
            &address,
            &opts,
            halves,
        )))
    }

    async fn kill(&self, id: &TermId) -> BackendResult<()> {
        let address = self.resolve(id)?;
        match self
            .call("terminal.kill", json!({"terminal": address}))
            .await
        {
            Ok(_) => Ok(()),
            Err(CallError::Rpc(error)) if error.kind.as_deref() == Some("finished") => Ok(()),
            Err(CallError::Rpc(error)) => Err(error.to_backend_error(id)),
            Err(error) => Err(error.to_failure()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;

    use combine_protocol::Display;
    use futures::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::task::JoinHandle;

    use super::*;

    fn backend(features: &[&str]) -> HerdrCoreBackend {
        let hello = Hello {
            version: "0.1.21".to_string(),
            features: features.iter().map(|f| f.to_string()).collect(),
            core_id: Some("core-a".to_string()),
        };
        HerdrCoreBackend::with_hello(HerdrCoreConfig::new("/nonexistent.sock".into()), hello)
    }

    #[test]
    fn id_is_herdr_core() {
        let backend = backend(&[]);
        assert_eq!(backend.id(), "herdr-core");
        assert_eq!(term_id("t7"), TermId("hc:t7".to_string()));
        assert_eq!(backend.version(), "0.1.21");
    }

    #[test]
    fn capabilities_follow_hello_features() {
        let with = backend(&["automatic_input"]).capabilities();
        assert!(with.automatic_replies);
        assert!(with.ordered_attach && with.byte_credit && with.display_queries);
        assert_eq!(with.initial_credit, 262_144);
        assert!(!with.resync_signal && !with.agent_state);
        assert!(!backend(&["framed"]).capabilities().automatic_replies);
    }

    #[test]
    fn resolve_accepts_a_local_address() {
        let backend = backend(&[]);
        assert_eq!(
            backend.resolve(&TermId("hc:t7".to_string())),
            Ok("t7".to_string())
        );
    }

    #[test]
    fn resolve_rejects_a_foreign_prefix_and_an_empty_address() {
        let backend = backend(&[]);
        for id in ["fake:t7", "hc:", "t7", ""] {
            let id = TermId(id.to_string());
            assert_eq!(
                backend.resolve(&id),
                Err(BackendError::UnknownTerminal(id.clone()))
            );
        }
    }

    #[test]
    fn resolve_rejects_machine_qualified_ids() {
        let backend = backend(&[]);
        let id = TermId("hc:work/t7".to_string());
        assert_eq!(
            backend.resolve(&id),
            Err(BackendError::RemoteTerminal(id.clone()))
        );
    }

    static NEXT_DIR: AtomicU32 = AtomicU32::new(0);

    /// `/tmp/cc-core-<pid>-<n>`, short enough for a socket path, removed on drop.
    struct Dir(PathBuf);

    impl Dir {
        fn new() -> Dir {
            let n = NEXT_DIR.fetch_add(1, Ordering::SeqCst);
            let path = PathBuf::from(format!("/tmp/cc-core-{}-{n}", std::process::id()));
            std::fs::create_dir(&path).expect("scratch directory");
            Dir(path)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A core that answers `hello` with a core id the test can change, and the one call each
    /// test needs. Its connections stay open until `drop_connections`.
    struct ScriptedCore {
        _dir: Dir,
        socket: PathBuf,
        core_id: Arc<Mutex<String>>,
        connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
        accept: JoinHandle<()>,
    }

    impl ScriptedCore {
        fn start(core_id: &str) -> ScriptedCore {
            let dir = Dir::new();
            let socket = dir.0.join("core.sock");
            let listener = UnixListener::bind(&socket).expect("bind");
            let core_id = Arc::new(Mutex::new(core_id.to_string()));
            let connections = Arc::new(Mutex::new(Vec::new()));
            let accept = tokio::spawn({
                let core_id = Arc::clone(&core_id);
                let connections = Arc::clone(&connections);
                async move {
                    while let Ok((stream, _)) = listener.accept().await {
                        let id = core_id.lock().expect("core id").clone();
                        let connection = tokio::spawn(serve(stream, id));
                        connections.lock().expect("connections").push(connection);
                    }
                }
            });
            ScriptedCore {
                _dir: dir,
                socket,
                core_id,
                connections,
                accept,
            }
        }

        fn config(&self) -> HerdrCoreConfig {
            let mut config = HerdrCoreConfig::new(self.socket.clone());
            config.reopen_attempts = 3;
            config.reopen_interval = Duration::from_millis(20);
            config
        }

        fn become_core(&self, id: &str) {
            *self.core_id.lock().expect("core id") = id.to_string();
        }

        fn drop_connections(&self) {
            for connection in self.connections.lock().expect("connections").drain(..) {
                connection.abort();
            }
        }
    }

    impl Drop for ScriptedCore {
        fn drop(&mut self) {
            self.accept.abort();
            self.drop_connections();
        }
    }

    async fn serve(mut stream: UnixStream, core_id: String) {
        let mut byte = [0u8; 1];
        while stream.read_exact(&mut byte).await.is_ok() && byte[0] != b'\n' {}
        let hello = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "result": {
                "generation": 1,
                "version": "0.1.21",
                "framed": true,
                "core": {"id": core_id, "name": "scripted"},
                "methods": [
                    "terminal.attach", "terminal.list", "terminal.run", "terminal.kill",
                    "events.subscribe"
                ],
                "features": [],
            }
        });
        let mut line = hello.to_string().into_bytes();
        line.push(b'\n');
        if stream.write_all(&line).await.is_err() {
            return;
        }
        loop {
            let mut header = [0u8; frame::HEADER_LEN];
            if stream.read_exact(&mut header).await.is_err() {
                return;
            }
            let length = u32::from_le_bytes([header[5], header[6], header[7], header[8]]);
            let mut body = vec![0u8; length as usize];
            if stream.read_exact(&mut body).await.is_err() {
                return;
            }
            let Ok(request) = serde_json::from_slice::<Value>(&body) else {
                return;
            };
            let result = match request["method"].as_str() {
                Some("terminal.list") => json!({"terminals": []}),
                _ => json!({"seq": 0, "terminals": []}),
            };
            let reply = json!({"jsonrpc": "2.0", "id": request["id"], "result": result});
            if stream
                .write_all(&frame::rpc(reply.to_string().as_bytes()))
                .await
                .is_err()
            {
                return;
            }
        }
    }

    fn display() -> Display {
        Display {
            foreground: "#ffffff".to_string(),
            background: "#000000".to_string(),
            cursor_color: "#ffffff".to_string(),
            cell_width: 9,
            cell_height: 18,
        }
    }

    async fn connected(core: &ScriptedCore) -> HerdrCoreBackend {
        HerdrCoreBackend::connect(core.config())
            .await
            .expect("connect")
    }

    #[tokio::test]
    async fn instance_is_the_core_id_at_start() {
        let core = ScriptedCore::start("core-a");
        let backend = connected(&core).await;
        assert_eq!(backend.instance().as_deref(), Some("core-a"));
        assert_eq!(backend.list().await, Ok(Vec::new()));
    }

    #[tokio::test]
    async fn call_refuses_another_core_without_retrying() {
        let core = ScriptedCore::start("core-a");
        let backend = connected(&core).await;
        core.become_core("core-b");
        let started = Instant::now();
        let result = backend.list().await;
        assert!(matches!(result, Err(BackendError::Failed(_))), "{result:?}");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(backend.instance().as_deref(), Some("core-a"));
    }

    #[tokio::test]
    async fn attach_half_refuses_another_core() {
        let core = ScriptedCore::start("core-a");
        let backend = connected(&core).await;
        core.become_core("core-b");
        let display = display();
        let params = OpenParams {
            address: "t1",
            size: (80, 24),
            display: &display,
            history: 0,
            resumed: false,
            watch_only: false,
        };
        let opened = pair::open(&backend.env, &params).await;
        let Err(OpenError::Failed(BackendError::Failed(message))) = opened else {
            panic!("the open must fail");
        };
        assert!(message.contains("another core"), "{message}");
    }

    #[tokio::test]
    async fn the_watch_ends_when_it_resubscribes_to_another_core() {
        let core = ScriptedCore::start("core-a");
        let backend = connected(&core).await;
        let mut stream = backend.watch().await.expect("watch");
        core.become_core("core-b");
        core.drop_connections();
        let next = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("the stream ends");
        assert!(next.is_none(), "{next:?}");
    }

    #[tokio::test]
    async fn the_watch_survives_the_same_core_reconnecting() {
        let core = ScriptedCore::start("core-a");
        let backend = connected(&core).await;
        let mut stream = backend.watch().await.expect("watch");
        core.drop_connections();
        let next = tokio::time::timeout(Duration::from_millis(300), stream.next()).await;
        assert!(next.is_err(), "the stream stays open: {next:?}");
    }
}
