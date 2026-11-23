//! Turns `daemon.json`, the environment and the defaults into the paths the daemon uses.

use std::fmt;
use std::path::{Path, PathBuf};

use super::backend::BackendChoice;
use crate::adapters::fs::config::{BackendName, ConfigFileError, DaemonConfigFile};
use crate::rpc::socket::MAX_SOCKET_PATH;

const DATA_DIR_UNDER_HOME: &str = "Library/Application Support/com.squashmerge.combined";
const SOCKET_NAME: &str = "combined.sock";

/// The environment variables the daemon reads. An empty value counts as unset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DaemonEnv {
    pub home: Option<PathBuf>,
    pub hc_socket: Option<PathBuf>,
    pub hc_session: Option<String>,
    pub xdg_runtime_dir: Option<PathBuf>,
    pub xdg_state_home: Option<PathBuf>,
}

impl DaemonEnv {
    pub fn from_process() -> DaemonEnv {
        fn var(name: &str) -> Option<std::ffi::OsString> {
            std::env::var_os(name).filter(|value| !value.is_empty())
        }
        DaemonEnv {
            home: var("HOME").map(PathBuf::from),
            hc_socket: var("HC_SOCKET").map(PathBuf::from),
            hc_session: var("HC_SESSION").and_then(|value| value.into_string().ok()),
            xdg_runtime_dir: var("XDG_RUNTIME_DIR").map(PathBuf::from),
            xdg_state_home: var("XDG_STATE_HOME").map(PathBuf::from),
        }
    }

    fn home(&self, needed_for: &'static str) -> Result<&Path, DaemonConfigError> {
        self.home
            .as_deref()
            .ok_or(DaemonConfigError::NoHome { needed_for })
    }
}

/// Where the herdr-core socket path came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HerdrCoreSource {
    ConfigFile,
    HcSocket,
    XdgRuntimeDir { session: String },
    XdgStateHome { session: String },
    DefaultStateDir { session: String },
}

impl fmt::Display for HerdrCoreSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HerdrCoreSource::ConfigFile => f.write_str("herdr_core_socket in daemon.json"),
            HerdrCoreSource::HcSocket => f.write_str("HC_SOCKET"),
            HerdrCoreSource::XdgRuntimeDir { session } => {
                write!(f, "XDG_RUNTIME_DIR, session {session}")
            }
            HerdrCoreSource::XdgStateHome { session } => {
                write!(f, "XDG_STATE_HOME, session {session}")
            }
            HerdrCoreSource::DefaultStateDir { session } => {
                write!(f, "the default state directory, session {session}")
            }
        }
    }
}

#[derive(Debug)]
pub enum DaemonConfigError {
    File(ConfigFileError),
    NoHome { needed_for: &'static str },
    RelativePath { what: &'static str, path: PathBuf },
    SocketTooLong { what: &'static str, path: PathBuf },
    FakeBackendRefused,
}

impl DaemonConfigError {
    pub const FAKE_REFUSED: &'static str = "backend \"fake\" exists only in test builds";
}

impl fmt::Display for DaemonConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DaemonConfigError::File(error) => error.fmt(f),
            DaemonConfigError::NoHome { needed_for } => write!(
                f,
                "HOME is not set, so the default {needed_for} cannot be found; set it in daemon.json"
            ),
            DaemonConfigError::RelativePath { what, path } => {
                write!(f, "{what} {} must be an absolute path", path.display())
            }
            DaemonConfigError::SocketTooLong { what, path } => write!(
                f,
                "{what} {} is {} bytes; the limit is {MAX_SOCKET_PATH}",
                path.display(),
                path.as_os_str().len()
            ),
            DaemonConfigError::FakeBackendRefused => f.write_str(Self::FAKE_REFUSED),
        }
    }
}

impl std::error::Error for DaemonConfigError {}

impl From<ConfigFileError> for DaemonConfigError {
    fn from(error: ConfigFileError) -> Self {
        DaemonConfigError::File(error)
    }
}

/// What the daemon runs with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    pub data_dir: PathBuf,
    pub socket: PathBuf,
    pub backend: BackendChoice,
}

/// `$HOME/.config/combine/daemon.json`, or `None` without `HOME`.
pub fn default_config_path(env: &DaemonEnv) -> Option<PathBuf> {
    env.home
        .as_ref()
        .map(|home| home.join(".config/combine/daemon.json"))
}

fn absolute(what: &'static str, path: PathBuf) -> Result<PathBuf, DaemonConfigError> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(DaemonConfigError::RelativePath { what, path })
    }
}

fn short_socket(what: &'static str, path: PathBuf) -> Result<PathBuf, DaemonConfigError> {
    let path = absolute(what, path)?;
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(DaemonConfigError::SocketTooLong { what, path });
    }
    Ok(path)
}

fn data_dir(
    file: Option<&DaemonConfigFile>,
    env: &DaemonEnv,
) -> Result<PathBuf, DaemonConfigError> {
    match file.and_then(|file| file.data_dir.clone()) {
        Some(path) => absolute("data dir", path),
        None => absolute(
            "data dir",
            env.home("data directory")?.join(DATA_DIR_UNDER_HOME),
        ),
    }
}

/// Settles the data directory, the socket and the backend. A relative path, a socket path over
/// 103 bytes, or `fake` in a build without `test-support` is an error.
pub fn resolve(
    file: Option<DaemonConfigFile>,
    env: &DaemonEnv,
) -> Result<ResolvedConfig, DaemonConfigError> {
    let backend = match file.as_ref().and_then(|file| file.backend) {
        None | Some(BackendName::HerdrCore) => {
            let configured = file
                .as_ref()
                .and_then(|file| file.herdr_core_socket.clone());
            let (socket, source) = herdr_core_socket(configured, env)?;
            BackendChoice::HerdrCore { socket, source }
        }
        Some(BackendName::Fake) if cfg!(feature = "test-support") => BackendChoice::Fake,
        Some(BackendName::Fake) => return Err(DaemonConfigError::FakeBackendRefused),
    };
    let data_dir = data_dir(file.as_ref(), env)?;
    let socket = match file.and_then(|file| file.socket) {
        Some(path) => path,
        None => data_dir.join(SOCKET_NAME),
    };
    Ok(ResolvedConfig {
        socket: short_socket("socket", socket)?,
        data_dir,
        backend,
    })
}

/// Finds herdr-core's socket: the config's `herdr_core_socket`, else `HC_SOCKET`, else the
/// session's socket under `XDG_RUNTIME_DIR`, else under `XDG_STATE_HOME` or `$HOME/.local/state`.
/// The daemon's resolver spells herdr-core's default path here. The client's refusal guard
/// spells the default session's socket again, in `app/src-tauri/src/hosts/local.rs`
/// (`DEFAULT_SOCKET_SUFFIX`). SPEC's layering keeps the two crates apart, so a change to
/// herdr-core's layout changes both.
pub fn herdr_core_socket(
    configured: Option<PathBuf>,
    env: &DaemonEnv,
) -> Result<(PathBuf, HerdrCoreSource), DaemonConfigError> {
    const WHAT: &str = "herdr-core socket";
    if let Some(path) = configured {
        return Ok((short_socket(WHAT, path)?, HerdrCoreSource::ConfigFile));
    }
    if let Some(path) = env.hc_socket.clone() {
        return Ok((short_socket(WHAT, path)?, HerdrCoreSource::HcSocket));
    }
    let session = env
        .hc_session
        .clone()
        .unwrap_or_else(|| "default".to_string());
    if let Some(dir) = &env.xdg_runtime_dir {
        let path = dir.join("herdr-core").join(format!("{session}.sock"));
        return Ok((
            short_socket(WHAT, path)?,
            HerdrCoreSource::XdgRuntimeDir { session },
        ));
    }
    let (state, source) = match &env.xdg_state_home {
        Some(state) => (
            state.clone(),
            HerdrCoreSource::XdgStateHome {
                session: session.clone(),
            },
        ),
        None => (
            env.home("herdr-core socket")?.join(".local/state"),
            HerdrCoreSource::DefaultStateDir {
                session: session.clone(),
            },
        ),
    };
    let path = state
        .join("herdr-core")
        .join(&session)
        .join(format!("{session}.sock"));
    Ok((short_socket(WHAT, path)?, source))
}

/// The socket `combined connect` uses: `--socket`, else the config's `socket`, else the
/// config's `data_dir`, else the default data directory. It never creates anything.
pub fn connect_socket(
    flag_socket: Option<PathBuf>,
    file: Option<&DaemonConfigFile>,
    env: &DaemonEnv,
) -> Result<PathBuf, DaemonConfigError> {
    if let Some(socket) = flag_socket {
        return Ok(socket);
    }
    if let Some(socket) = file.and_then(|file| file.socket.clone()) {
        return short_socket("socket", socket);
    }
    short_socket("socket", data_dir(file, env)?.join(SOCKET_NAME))
}
