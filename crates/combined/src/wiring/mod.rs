//! Entry-point wiring: turns settings into a running daemon.

use std::fmt;

pub mod clock;
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
