//! The herdr-core adapter: [`TerminalBackend`] over herdr-core's framed protocol on a Unix
//! socket the caller names. It is the only code that speaks that protocol (SPEC.md, "Terminal
//! backends").
//!
//! A `TermId` is `hc:<address>`. Each attachment is a pair of viewers (see `pair.rs`), and the
//! adapter keeps the pair alive across a dropped half or a restarted core.

mod attachment;
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
use self::connection::{CallError, Connection};
use self::jsonrpc::{address_of, is_remote, term_id, Hello};
use self::pair::{OpenError, OpenParams};
use crate::model::{SpawnSpec, TermEvent, TermInfo};
use crate::ports::{
    AttachOptions, Attachment, BackendError, BackendResult, Capabilities, TerminalBackend,
};

#[cfg(feature = "test-support")]
pub use self::control::{DeliveryWait, HerdrCoreControl, PairHalf, PairStatus};

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
    pub(super) fence: Mutex<HashMap<String, u64>>,
    pub(super) fidelities: Mutex<Fidelities>,
    /// The core lists `automatic_input` among its features.
    pub(super) automatic_feature: bool,
    #[cfg(feature = "test-support")]
    pub(super) control: Arc<control::Shared>,
}

impl Env {
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
                fence: Mutex::new(HashMap::new()),
                fidelities: Mutex::new(Fidelities::default()),
                automatic_feature: hello.has_feature("automatic_input"),
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
            match Connection::open(&config.socket).await {
                Ok(opened) => break opened,
                Err(error) => {
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
    use super::*;

    fn backend(features: &[&str]) -> HerdrCoreBackend {
        let hello = Hello {
            version: "0.1.21".to_string(),
            features: features.iter().map(|f| f.to_string()).collect(),
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
}
