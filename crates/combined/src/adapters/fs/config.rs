//! `daemon.json`: what the daemon reads before anything else.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

const VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum BackendName {
    #[serde(rename = "herdr-core")]
    HerdrCore,
    #[serde(rename = "fake")]
    Fake,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfigFile {
    pub version: u32,
    pub backend: Option<BackendName>,
    pub herdr_core_socket: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub socket: Option<PathBuf>,
}

#[derive(Debug)]
pub enum ConfigFileError {
    /// An explicitly named config file doesn't exist.
    Missing {
        path: PathBuf,
    },
    Unreadable {
        path: PathBuf,
        source: io::Error,
    },
    BadJson {
        path: PathBuf,
        detail: String,
    },
    WrongVersion {
        path: PathBuf,
        found: u32,
    },
    RelativePath {
        path: PathBuf,
        field: &'static str,
    },
}

impl fmt::Display for ConfigFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigFileError::Missing { path } => {
                write!(f, "config file {} does not exist", path.display())
            }
            ConfigFileError::Unreadable { path, source } => {
                write!(f, "cannot read config file {}: {source}", path.display())
            }
            ConfigFileError::BadJson { path, detail } => {
                write!(f, "config file {} is invalid: {detail}", path.display())
            }
            ConfigFileError::WrongVersion { path, found } => write!(
                f,
                "config file {} has version {found}, not {VERSION}",
                path.display()
            ),
            ConfigFileError::RelativePath { path, field } => write!(
                f,
                "config file {}: {field} must be an absolute path",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ConfigFileError {}

/// Reads and checks the config file. A missing file is `None`, unless `required`.
pub fn load(path: &Path, required: bool) -> Result<Option<DaemonConfigFile>, ConfigFileError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return if required {
                Err(ConfigFileError::Missing {
                    path: path.to_path_buf(),
                })
            } else {
                Ok(None)
            };
        }
        Err(source) => {
            return Err(ConfigFileError::Unreadable {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let file: DaemonConfigFile =
        serde_json::from_slice(&bytes).map_err(|error| ConfigFileError::BadJson {
            path: path.to_path_buf(),
            detail: error.to_string(),
        })?;
    if file.version != VERSION {
        return Err(ConfigFileError::WrongVersion {
            path: path.to_path_buf(),
            found: file.version,
        });
    }
    let paths = [
        ("herdr_core_socket", &file.herdr_core_socket),
        ("data_dir", &file.data_dir),
        ("socket", &file.socket),
    ];
    for (field, value) in paths {
        if value.as_ref().is_some_and(|value| !value.is_absolute()) {
            return Err(ConfigFileError::RelativePath {
                path: path.to_path_buf(),
                field,
            });
        }
    }
    Ok(Some(file))
}
