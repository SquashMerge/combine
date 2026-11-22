//! The local daemon socket, and the refusal to ever connect to a herdr-core socket.

use std::env;
use std::fs;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::link::{Closer, ConnectError, Connector, ExitInfo, ExitReport, FailureText, Link};

const HERDR_CORE_DIR: &str = "herdr-core";
const DEFAULT_SOCKET_SUFFIX: &str = "herdr-core/default/default.sock";

/// The environment that names a herdr-core socket, read once so tests can fake it.
#[derive(Debug, Clone, Default)]
pub struct SocketGuardEnv {
    pub home: Option<PathBuf>,
    pub xdg_state_home: Option<PathBuf>,
    pub hc_socket: Option<PathBuf>,
}

impl SocketGuardEnv {
    pub fn from_process() -> Self {
        SocketGuardEnv {
            home: env::var_os("HOME").map(PathBuf::from),
            xdg_state_home: env::var_os("XDG_STATE_HOME").map(PathBuf::from),
            hc_socket: env::var_os("HC_SOCKET").map(PathBuf::from),
        }
    }
}

fn has_herdr_core_component(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == HERDR_CORE_DIR)
}

/// Why `path` must not be connected to, or `None` when it may be. The message is Combine text:
/// the path was validated when hosts.json loaded.
pub fn herdr_core_socket_refusal(path: &Path, env: &SocketGuardEnv) -> Option<String> {
    let canonical = fs::canonicalize(path).ok();
    let candidates = [Some(path), canonical.as_deref()];
    let named = candidates
        .iter()
        .flatten()
        .any(|p| has_herdr_core_component(p));
    let defaults = [
        env.home
            .as_ref()
            .map(|h| h.join(".local/state").join(DEFAULT_SOCKET_SUFFIX)),
        env.xdg_state_home
            .as_ref()
            .map(|x| x.join(DEFAULT_SOCKET_SUFFIX)),
        env.hc_socket.clone(),
    ];
    let equals_known = defaults.iter().flatten().any(|known| {
        let known_canonical = fs::canonicalize(known).ok();
        [Some(known.as_path()), known_canonical.as_deref()]
            .iter()
            .flatten()
            .any(|k| candidates.iter().flatten().any(|p| p == k))
    });
    (named || equals_known).then(|| {
        format!(
            "refusing to connect to {}: it is a herdr-core socket. A local host's \"socket\" must be the Combine daemon's socket.",
            path.display()
        )
    })
}

pub struct LocalSocketConnector {
    socket: PathBuf,
    env: SocketGuardEnv,
}

impl LocalSocketConnector {
    pub fn new(socket: PathBuf, env: SocketGuardEnv) -> Self {
        LocalSocketConnector { socket, env }
    }

    fn cannot_connect(&self, e: impl ToString) -> ConnectError {
        ConnectError::Transient(FailureText {
            text: format!("cannot connect to {}", self.socket.display()),
            detail: Some(e.to_string()),
        })
    }
}

struct NoExit;

impl ExitReport for NoExit {
    fn finish(self: Box<Self>) -> Option<ExitInfo> {
        None
    }
}

impl Connector for LocalSocketConnector {
    fn connect(&self) -> Result<Link, ConnectError> {
        if let Some(message) = herdr_core_socket_refusal(&self.socket, &self.env) {
            return Err(ConnectError::Permanent(FailureText {
                text: message,
                detail: None,
            }));
        }
        let stream = UnixStream::connect(&self.socket).map_err(|e| self.cannot_connect(e))?;
        let reader = stream.try_clone().map_err(|e| self.cannot_connect(e))?;
        let shutter = stream.try_clone().map_err(|e| self.cannot_connect(e))?;
        let closer: Closer = Arc::new(move || {
            let _ = shutter.shutdown(Shutdown::Both);
        });
        Ok(Link::new(
            Box::new(reader),
            Box::new(stream),
            closer,
            Box::new(NoExit),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::handshake::run_handshake;
    use super::super::testing::{
        host_id, joins_within, serve_on_socket, Double, ShortTempDir, HOST_A, WAIT,
    };
    use super::*;

    fn env(home: Option<&str>, xdg: Option<&str>, hc: Option<&str>) -> SocketGuardEnv {
        SocketGuardEnv {
            home: home.map(PathBuf::from),
            xdg_state_home: xdg.map(PathBuf::from),
            hc_socket: hc.map(PathBuf::from),
        }
    }

    #[test]
    fn refuses_default_herdr_core_socket() {
        let guard = env(Some("/Users/example"), None, None);
        let path = Path::new("/Users/example/.local/state/herdr-core/default/default.sock");
        let message = herdr_core_socket_refusal(path, &guard).expect("refused");
        assert_eq!(
            message,
            format!(
                "refusing to connect to {}: it is a herdr-core socket. A local host's \"socket\" must be the Combine daemon's socket.",
                path.display()
            )
        );
        assert!(
            herdr_core_socket_refusal(Path::new("/Users/example/combine/d.sock"), &guard).is_none()
        );
    }

    #[test]
    fn refuses_xdg_state_and_hc_socket_paths() {
        let guard = env(None, Some("/xdg/state"), Some("/somewhere/else/core.sock"));
        assert!(herdr_core_socket_refusal(
            Path::new("/xdg/state/herdr-core/default/default.sock"),
            &guard
        )
        .is_some());
        assert!(
            herdr_core_socket_refusal(Path::new("/somewhere/else/core.sock"), &guard).is_some()
        );
        assert!(herdr_core_socket_refusal(Path::new("/somewhere/else/d.sock"), &guard).is_none());
    }

    #[test]
    fn refuses_any_herdr_core_directory() {
        let guard = SocketGuardEnv::default();
        assert!(
            herdr_core_socket_refusal(Path::new("/tmp/x/herdr-core/other.sock"), &guard).is_some()
        );
        assert!(
            herdr_core_socket_refusal(Path::new("/tmp/x/herdr-core-not/d.sock"), &guard).is_none()
        );
        let connector = LocalSocketConnector::new(
            PathBuf::from("/tmp/x/herdr-core/other.sock"),
            SocketGuardEnv::default(),
        );
        match connector.connect() {
            Err(ConnectError::Permanent(ft)) => assert_eq!(ft.detail, None),
            other => panic!("expected a refusal, got {:?}", other.err()),
        }
    }

    #[test]
    fn refuses_a_symlink_into_a_herdr_core_directory() {
        let dir = ShortTempDir::new();
        let core = dir.path().join("herdr-core");
        fs::create_dir(&core).expect("create the directory");
        fs::write(core.join("d.sock"), b"").expect("create a regular file");
        let link = dir.path().join("combine.sock");
        std::os::unix::fs::symlink(core.join("d.sock"), &link).expect("symlink");
        let guard = SocketGuardEnv::default();
        assert_eq!(
            herdr_core_socket_refusal(&link, &guard),
            Some(format!(
                "refusing to connect to {}: it is a herdr-core socket. A local host's \"socket\" must be the Combine daemon's socket.",
                link.display()
            ))
        );
        match LocalSocketConnector::new(link, guard).connect() {
            Err(ConnectError::Permanent(ft)) => assert_eq!(ft.detail, None),
            other => panic!("expected a refusal, got {:?}", other.err()),
        }

        let real = dir.path().join("real.sock");
        fs::write(&real, b"").expect("create a regular file");
        let alias = dir.path().join("alias.sock");
        std::os::unix::fs::symlink(&real, &alias).expect("symlink");
        let guard = SocketGuardEnv {
            hc_socket: Some(real),
            ..SocketGuardEnv::default()
        };
        assert!(herdr_core_socket_refusal(&alias, &guard).is_some());
    }

    #[test]
    fn connect_error_is_transient_with_the_os_error_as_detail() {
        let dir = ShortTempDir::new();
        let socket = dir.path().join("none.sock");
        let connector = LocalSocketConnector::new(socket.clone(), SocketGuardEnv::default());
        match connector.connect() {
            Err(ConnectError::Transient(ft)) => {
                assert_eq!(ft.text, format!("cannot connect to {}", socket.display()));
                assert!(ft.detail.is_some_and(|d| !d.is_empty()));
            }
            other => panic!("expected a transient failure, got {:?}", other.err()),
        }
    }

    #[test]
    fn connects_to_short_tmp_socket_and_handshakes() {
        let dir = ShortTempDir::new();
        let socket = dir.path().join("d.sock");
        let server = serve_on_socket(&socket, Double::welcome(host_id(HOST_A)));
        let connector = LocalSocketConnector::new(socket, SocketGuardEnv::default());
        let mut link = match connector.connect() {
            Ok(link) => link,
            Err(e) => panic!("connect failed: {e:?}"),
        };
        let welcome = run_handshake(&mut *link.reader, &mut *link.writer)
            .unwrap_or_else(|e| panic!("handshake failed: {e:?}"));
        assert_eq!(welcome.host_id, host_id(HOST_A));
        assert_eq!(link.finish(), None);
        assert!(joins_within(server, WAIT));
    }
}
