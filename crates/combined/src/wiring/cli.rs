//! The command line: `serve`, `connect`, `--version` and `--help`.

use std::ffi::OsString;
use std::path::PathBuf;

use super::StartError;

pub const USAGE: &str = "usage:
  combined serve [--config <path>]
  combined connect [--socket <path>] [--config <path>]
  combined --version";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Serve {
        config: Option<PathBuf>,
    },
    Connect {
        socket: Option<PathBuf>,
        config: Option<PathBuf>,
    },
    Version,
    Help,
}

/// Parses the arguments after the program name.
pub fn parse<I>(args: I) -> Result<Command, StartError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let first = args.next().ok_or_else(|| usage("missing command"))?;
    match first.to_str() {
        Some("serve") => {
            let (config, _) = flags(args, false)?;
            Ok(Command::Serve { config })
        }
        Some("connect") => {
            let (config, socket) = flags(args, true)?;
            Ok(Command::Connect { socket, config })
        }
        Some("--version" | "-V") => no_more(args, Command::Version),
        Some("--help" | "-h" | "help") => no_more(args, Command::Help),
        _ => Err(usage(&format!(
            "unknown command {}",
            first.to_string_lossy()
        ))),
    }
}

fn usage(problem: &str) -> StartError {
    StartError::Usage(format!("{problem}\n{USAGE}"))
}

fn no_more(
    mut args: impl Iterator<Item = OsString>,
    command: Command,
) -> Result<Command, StartError> {
    match args.next() {
        None => Ok(command),
        Some(extra) => Err(usage(&format!(
            "unexpected argument {}",
            extra.to_string_lossy()
        ))),
    }
}

/// Reads `--config <path>` and, when `allow_socket`, `--socket <path>`. Returns
/// `(config, socket)`.
fn flags(
    mut args: impl Iterator<Item = OsString>,
    allow_socket: bool,
) -> Result<(Option<PathBuf>, Option<PathBuf>), StartError> {
    let mut config = None;
    let mut socket = None;
    while let Some(flag) = args.next() {
        let slot = match flag.to_str() {
            Some("--config") => &mut config,
            Some("--socket") if allow_socket => &mut socket,
            _ => {
                return Err(usage(&format!(
                    "unexpected argument {}",
                    flag.to_string_lossy()
                )))
            }
        };
        let value = args
            .next()
            .ok_or_else(|| usage(&format!("{} needs a value", flag.to_string_lossy())))?;
        if slot.replace(PathBuf::from(value)).is_some() {
            return Err(usage(&format!("{} given twice", flag.to_string_lossy())));
        }
    }
    Ok((config, socket))
}
