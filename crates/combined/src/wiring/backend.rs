//! Builds the terminal backend the daemon runs on.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::future::BoxFuture;

use super::config::HerdrCoreSource;
use super::StartError;
use crate::adapters::herdr_core::{HerdrCoreBackend, HerdrCoreConfig};
use crate::ports::TerminalBackend;
use crate::rpc::socket::MAX_SOCKET_PATH;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendChoice {
    HerdrCore {
        socket: PathBuf,
        source: HerdrCoreSource,
    },
    Fake,
}

/// The herdr-core adapter's constructor, given herdr-core's socket.
pub type HerdrCoreFactory =
    fn(PathBuf) -> BoxFuture<'static, Result<Arc<dyn TerminalBackend>, String>>;

/// The herdr-core adapter's constructor (`adapters::herdr_core`, S3), wired in by #23.
pub const HERDR_CORE: Option<HerdrCoreFactory> = Some(herdr_core_factory);

fn herdr_core_factory(
    socket: PathBuf,
) -> BoxFuture<'static, Result<Arc<dyn TerminalBackend>, String>> {
    Box::pin(async move {
        HerdrCoreBackend::connect(HerdrCoreConfig::new(socket))
            .await
            .map(|backend| Arc::new(backend) as Arc<dyn TerminalBackend>)
            .map_err(|error| error.to_string())
    })
}

pub async fn build(choice: &BackendChoice) -> Result<Arc<dyn TerminalBackend>, StartError> {
    match choice {
        BackendChoice::HerdrCore { socket, source } => {
            check_herdr_core_socket(socket, source)?;
            match HERDR_CORE {
                Some(factory) => factory(socket.clone()).await.map_err(|error| {
                    StartError::Backend(format!(
                        "cannot start the herdr-core backend on {}: {error}",
                        socket.display()
                    ))
                }),
                None => Err(StartError::Backend(
                    "combined was built without the herdr-core adapter".to_string(),
                )),
            }
        }
        BackendChoice::Fake => fake_backend(),
    }
}

/// Fails fast, without connecting, when herdr-core's socket path is unusable.
fn check_herdr_core_socket(socket: &Path, source: &HerdrCoreSource) -> Result<(), StartError> {
    use std::os::unix::fs::FileTypeExt;

    let problem = if !socket.is_absolute() {
        "the path is not absolute".to_string()
    } else if socket.as_os_str().len() > MAX_SOCKET_PATH {
        format!("the path is over {MAX_SOCKET_PATH} bytes")
    } else {
        match fs::metadata(socket) {
            Ok(meta) if meta.file_type().is_socket() => return Ok(()),
            Ok(_) => "the path exists and is not a socket".to_string(),
            Err(error) => format!("cannot stat it: {error}"),
        }
    };
    Err(StartError::Backend(format!(
        "herdr-core is unavailable at {} (from {source}): {problem}; start herdr-core, or set \
         herdr_core_socket in daemon.json (see docs/daemon.md)",
        socket.display()
    )))
}

#[cfg(feature = "test-support")]
fn fake_backend() -> Result<Arc<dyn TerminalBackend>, StartError> {
    use crate::testing::{FakeBackend, FakeConfig};

    let (backend, _control) = FakeBackend::new(FakeConfig::default());
    Ok(backend)
}

#[cfg(not(feature = "test-support"))]
fn fake_backend() -> Result<Arc<dyn TerminalBackend>, StartError> {
    Err(StartError::Config(
        super::config::DaemonConfigError::FAKE_REFUSED.to_string(),
    ))
}
