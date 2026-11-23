//! Entry-point wiring: turns settings into a running daemon.

use std::ffi::OsString;
use std::fmt;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use crate::adapters::fs::config as config_file;
use crate::adapters::fs::config::DaemonConfigFile;
use crate::adapters::fs::data_dir::DataDir;
use crate::adapters::fs::file_store::FsFileStore;
use crate::adapters::fs::git_probe::FsGitProbe;
use crate::adapters::fs::host_id;
use crate::adapters::fs::project_dirs::FsProjectDirs;
use crate::core::{Core, CoreConfig, Driver, LayoutTiming, TerminalList};
use crate::ports::{FileStore, TerminalBackend};
use crate::rpc::socket::{self, BindError};
use crate::rpc::{self, ServeError, ServeOptions, ServerParts, ServerStats};
use cli::Command;
use clock::MonotonicClock;
use combine_protocol::HostId;
use config::{DaemonConfigError, DaemonEnv};
use ids::IdReservation;
use tokio::signal::unix::{signal, SignalKind};

pub mod backend;
pub mod cli;
pub mod clock;
pub mod config;
pub mod ids;

/// Why the daemon did not start or stopped.
#[derive(Debug)]
pub enum StartError {
    /// A runtime failure: exit 1.
    Runtime(String),
    /// Bad command-line usage: exit 2.
    Usage(String),
    /// A bad config file or setting: exit 2.
    Config(String),
    /// A bad or unusable data directory or file in it: exit 2.
    Data(String),
    /// The terminal backend is unavailable: exit 3.
    Backend(String),
}

impl StartError {
    pub fn exit_code(&self) -> u8 {
        match self {
            StartError::Runtime(_) => 1,
            StartError::Usage(_) | StartError::Config(_) | StartError::Data(_) => 2,
            StartError::Backend(_) => 3,
        }
    }
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::Runtime(text)
            | StartError::Usage(text)
            | StartError::Config(text)
            | StartError::Data(text)
            | StartError::Backend(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for StartError {}

impl From<DaemonConfigError> for StartError {
    fn from(error: DaemonConfigError) -> Self {
        StartError::Config(error.to_string())
    }
}

impl From<BindError> for StartError {
    fn from(error: BindError) -> Self {
        match error.exit_code() {
            2 => StartError::Data(error.to_string()),
            _ => StartError::Runtime(error.to_string()),
        }
    }
}

impl From<ServeError> for StartError {
    fn from(error: ServeError) -> Self {
        match error {
            ServeError::Backend(_) => StartError::Backend(error.to_string()),
            ServeError::State(_) | ServeError::Io(_) => StartError::Runtime(error.to_string()),
        }
    }
}

/// The daemon's entry point: runs the command named on the command line.
pub fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let result = cli::parse(args).and_then(|command| match command {
        Command::Serve { config } => run_serve(config.as_deref()).map(|()| ExitCode::SUCCESS),
        Command::Connect { socket, config } => run_connect(socket, config.as_deref()),
        Command::Version => {
            println!(
                "combined {} (protocol {})",
                env!("CARGO_PKG_VERSION"),
                combine_protocol::PROTOCOL_VERSION
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Help => {
            println!("{}", cli::USAGE);
            Ok(ExitCode::SUCCESS)
        }
    });
    match result {
        Ok(code) => code,
        Err(error) => {
            for line in error.to_string().lines() {
                eprintln!("combined: {line}");
            }
            ExitCode::from(error.exit_code())
        }
    }
}

/// Reads the config named by `--config`, which must exist, or else the default one, which may
/// be missing.
fn load_config(
    flag: Option<&Path>,
    env: &DaemonEnv,
) -> Result<Option<DaemonConfigFile>, StartError> {
    let loaded = match (flag, config::default_config_path(env)) {
        (Some(path), _) => config_file::load(path, true),
        (None, Some(path)) => config_file::load(&path, false),
        (None, None) => Ok(None),
    };
    loaded.map_err(|error| StartError::Config(error.to_string()))
}

fn run_connect(
    socket: Option<std::path::PathBuf>,
    config: Option<&Path>,
) -> Result<ExitCode, StartError> {
    let env = DaemonEnv::from_process();
    let file = match socket {
        Some(_) => None,
        None => load_config(config, &env)?,
    };
    let socket = config::connect_socket(socket, file.as_ref(), &env)?;
    Ok(rpc::connect::run(&socket))
}

/// [`server_parts_with`] over a store without a fallback and the default layout timing.
pub fn server_parts(
    backend: Arc<dyn TerminalBackend>,
    id_base: u64,
    host_id: HostId,
    ids: IdReservation,
) -> Result<ServerParts, StartError> {
    server_parts_with(
        backend,
        id_base,
        host_id,
        ids,
        Arc::new(FsFileStore::new()),
        LayoutTiming::default(),
    )
}

/// Builds what the server runs on from a backend. `run_serve` and the in-process test servers
/// both come through here, so a test exercises the wiring the daemon ships.
pub fn server_parts_with(
    backend: Arc<dyn TerminalBackend>,
    id_base: u64,
    host_id: HostId,
    ids: IdReservation,
    file_store: Arc<dyn FileStore>,
    layout_timing: LayoutTiming,
) -> Result<ServerParts, StartError> {
    let core = Core::new(
        CoreConfig {
            id_base,
            ..CoreConfig::default()
        },
        backend.capabilities(),
    )
    .map_err(|error| StartError::Runtime(format!("invalid core configuration: {error}")))?;
    Ok(ServerParts {
        driver: Driver::new(core, backend),
        terminals: TerminalList::new(Box::new(FsGitProbe)),
        host_id,
        ids,
        clock: MonotonicClock::new(),
        file_store,
        layout_timing,
        project_dirs: Box::new(FsProjectDirs),
    })
}

/// Runs the daemon until it fails or gets SIGTERM or SIGINT, which write every pending layout
/// before it returns.
fn run_serve(config_flag: Option<&Path>) -> Result<(), StartError> {
    let env = DaemonEnv::from_process();
    let file = load_config(config_flag, &env)?;
    let resolved = config::resolve(file, &env)?;
    eprintln!(
        "combined: data dir {}, socket {}, backend {}",
        resolved.data_dir.display(),
        resolved.socket.display(),
        match &resolved.backend {
            backend::BackendChoice::HerdrCore { socket, source } =>
                format!("herdr-core at {} (from {source})", socket.display()),
            backend::BackendChoice::Fake => "fake".to_string(),
        }
    );

    // Held until this function returns: it is the data directory's lock.
    let data_dir =
        DataDir::open(&resolved.data_dir).map_err(|error| StartError::Data(error.to_string()))?;
    let host_id = host_id::load_or_create(&data_dir).map_err(|error| match error {
        host_id::HostIdError::Write { .. } => StartError::Runtime(error.to_string()),
        _ => StartError::Data(error.to_string()),
    })?;
    let (ids, id_base) = IdReservation::start(&data_dir.ids_json())?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| StartError::Runtime(format!("cannot start the runtime: {error}")))?;

    let layouts_dir = data_dir.path().join("spaces");
    let served = runtime.block_on(async {
        let signals = |kind| {
            signal(kind)
                .map_err(|error| StartError::Runtime(format!("cannot listen for signals: {error}")))
        };
        let mut terminate = signals(SignalKind::terminate())?;
        let mut interrupt = signals(SignalKind::interrupt())?;
        let backend = backend::build(&resolved.backend).await?;
        let parts = server_parts_with(
            backend,
            id_base,
            host_id,
            ids,
            Arc::new(FsFileStore::with_fallback(layouts_dir)),
            LayoutTiming::default(),
        )?;
        // Last, so a start that fails earlier leaves no socket behind.
        let bound = socket::bind(&resolved.socket)?;
        rpc::serve(
            bound,
            parts,
            ServeOptions::default(),
            async {
                tokio::select! {
                    _ = terminate.recv() => {}
                    _ = interrupt.recv() => {}
                }
            },
            Arc::new(ServerStats::new()),
        )
        .await
        .map_err(StartError::from)
    });
    drop(data_dir);
    served
}
