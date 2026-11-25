//! Finds the daemon's socket the way `combined connect` does.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

pub const MAX_SOCKET_PATH: usize = 103;
pub const DATA_DIR_UNDER_HOME: &str = "Library/Application Support/com.squashmerge.combined";
pub const SOCKET_NAME: &str = "combined.sock";

/// The two `daemon.json` fields the shim reads. Every other field is ignored, `version` included.
#[derive(Debug, Default, serde::Deserialize)]
pub struct ShimConfig {
    pub socket: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
}

#[derive(Debug)]
pub enum SocketError {
    ConfigMissing(PathBuf),
    ConfigUnreadable(PathBuf, String),
    ConfigInvalid(PathBuf, String),
    RelativePath { what: &'static str, path: PathBuf },
    TooLong { what: &'static str, path: PathBuf },
    NoHome,
}

impl fmt::Display for SocketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfigMissing(path) => {
                write!(f, "the config file {} doesn't exist", path.display())
            }
            Self::ConfigUnreadable(path, why) => {
                write!(f, "cannot read the config file {}: {why}", path.display())
            }
            Self::ConfigInvalid(path, why) => {
                write!(f, "the config file {} is invalid: {why}", path.display())
            }
            Self::RelativePath { what, path } => {
                write!(f, "{what} {} must be an absolute path", path.display())
            }
            Self::TooLong { what, path } => write!(
                f,
                "{what} {} is {} bytes; the limit is {MAX_SOCKET_PATH}",
                path.display(),
                path.as_os_str().len()
            ),
            Self::NoHome => f.write_str(
                "HOME is not set, so the default data directory cannot be found; \
                 set it in daemon.json",
            ),
        }
    }
}

impl std::error::Error for SocketError {}

/// `$HOME/.config/combine/daemon.json`, or `None` without a home.
pub fn default_config_path(home: Option<&Path>) -> Option<PathBuf> {
    home.map(|home| home.join(".config/combine/daemon.json"))
}

/// Reads the config. A missing file is `Ok(None)` unless `required`.
pub fn load_config(path: &Path, required: bool) -> Result<Option<ShimConfig>, SocketError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return if required {
                Err(SocketError::ConfigMissing(path.to_path_buf()))
            } else {
                Ok(None)
            };
        }
        Err(e) => {
            return Err(SocketError::ConfigUnreadable(
                path.to_path_buf(),
                e.to_string(),
            ))
        }
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| SocketError::ConfigInvalid(path.to_path_buf(), e.to_string()))
}

fn absolute(what: &'static str, path: PathBuf) -> Result<PathBuf, SocketError> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(SocketError::RelativePath { what, path })
    }
}

fn short_socket(what: &'static str, path: PathBuf) -> Result<PathBuf, SocketError> {
    let path = absolute(what, path)?;
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(SocketError::TooLong { what, path });
    }
    Ok(path)
}

/// The daemon's socket: `--socket` as given, else the config's `socket`, else the config's
/// `data_dir` (or the default data directory under `home`) plus `combined.sock`.
pub fn daemon_socket(
    flag: Option<PathBuf>,
    config: Option<&ShimConfig>,
    home: Option<&Path>,
) -> Result<PathBuf, SocketError> {
    if let Some(socket) = flag {
        return Ok(socket);
    }
    if let Some(socket) = config.and_then(|config| config.socket.clone()) {
        return short_socket("socket", socket);
    }
    let data_dir = match config.and_then(|config| config.data_dir.clone()) {
        Some(dir) => absolute("data dir", dir)?,
        None => absolute(
            "data dir",
            home.ok_or(SocketError::NoHome)?.join(DATA_DIR_UNDER_HOME),
        )?,
    };
    short_socket("socket", data_dir.join(SOCKET_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(json: &str) -> ShimConfig {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn the_flag_wins_and_is_unchecked() {
        let found = daemon_socket(Some("relative".into()), None, None).unwrap();
        assert_eq!(found, PathBuf::from("relative"));
        let long = PathBuf::from(format!("/{}", "a".repeat(200)));
        assert_eq!(daemon_socket(Some(long.clone()), None, None).unwrap(), long);
    }

    #[test]
    fn the_config_socket_comes_before_the_data_dir() {
        let config = config(r#"{"socket": "/s/x.sock", "data_dir": "/d"}"#);
        let found = daemon_socket(None, Some(&config), None).unwrap();
        assert_eq!(found, PathBuf::from("/s/x.sock"));
    }

    #[test]
    fn the_data_dir_gets_the_socket_name() {
        let config = config(r#"{"data_dir": "/d"}"#);
        let found = daemon_socket(None, Some(&config), None).unwrap();
        assert_eq!(found, PathBuf::from("/d/combined.sock"));
    }

    #[test]
    fn the_default_data_dir_is_under_home() {
        let found = daemon_socket(None, None, Some(Path::new("/Users/a"))).unwrap();
        assert_eq!(
            found,
            PathBuf::from(
                "/Users/a/Library/Application Support/com.squashmerge.combined/combined.sock"
            )
        );
    }

    #[test]
    fn the_error_texts() {
        let relative = config(r#"{"socket": "x.sock"}"#);
        assert_eq!(
            daemon_socket(None, Some(&relative), None)
                .unwrap_err()
                .to_string(),
            "socket x.sock must be an absolute path"
        );
        let relative_dir = config(r#"{"data_dir": "d"}"#);
        assert_eq!(
            daemon_socket(None, Some(&relative_dir), None)
                .unwrap_err()
                .to_string(),
            "data dir d must be an absolute path"
        );
        let long_path = format!("/{}", "a".repeat(103));
        let long = ShimConfig {
            socket: Some(long_path.clone().into()),
            data_dir: None,
        };
        assert_eq!(
            daemon_socket(None, Some(&long), None)
                .unwrap_err()
                .to_string(),
            format!("socket {long_path} is 104 bytes; the limit is 103")
        );
        let edge = ShimConfig {
            socket: Some(format!("/{}", "a".repeat(102)).into()),
            data_dir: None,
        };
        assert!(daemon_socket(None, Some(&edge), None).is_ok());
        assert_eq!(
            daemon_socket(None, None, None).unwrap_err().to_string(),
            "HOME is not set, so the default data directory cannot be found; set it in daemon.json"
        );
        let path = Path::new("/c/daemon.json");
        assert_eq!(
            SocketError::ConfigMissing(path.into()).to_string(),
            "the config file /c/daemon.json doesn't exist"
        );
    }

    #[test]
    fn the_config_is_lenient_about_other_fields() {
        let config = config(r#"{"version": 2, "backend": "fake", "extra": [1], "socket": "/s"}"#);
        assert_eq!(config.socket, Some(PathBuf::from("/s")));
    }

    #[test]
    fn loading_a_config() {
        let dir = std::env::temp_dir().join(format!("combine-socket-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("missing.json");
        assert!(load_config(&missing, false).unwrap().is_none());
        assert!(matches!(
            load_config(&missing, true),
            Err(SocketError::ConfigMissing(_))
        ));
        let bad = dir.join("bad.json");
        std::fs::write(&bad, "{").unwrap();
        assert!(matches!(
            load_config(&bad, false),
            Err(SocketError::ConfigInvalid(..))
        ));
        let wrong_type = dir.join("wrong.json");
        std::fs::write(&wrong_type, r#"{"socket": 3}"#).unwrap();
        assert!(matches!(
            load_config(&wrong_type, false),
            Err(SocketError::ConfigInvalid(..))
        ));
        let good = dir.join("good.json");
        std::fs::write(&good, r#"{"data_dir": "/d"}"#).unwrap();
        let loaded = load_config(&good, true).unwrap().unwrap();
        assert_eq!(loaded.data_dir, Some(PathBuf::from("/d")));
        assert!(matches!(
            load_config(&dir, false),
            Err(SocketError::ConfigUnreadable(..))
        ));
        assert_eq!(
            default_config_path(Some(Path::new("/h"))),
            Some(PathBuf::from("/h/.config/combine/daemon.json"))
        );
        assert_eq!(default_config_path(None), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
