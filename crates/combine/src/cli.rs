//! The command line: argument parsing and the `main` that runs it.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use combine_protocol::PROTOCOL_VERSION;

use crate::{relay, socket};

pub const USAGE: &str =
    "usage: combine mcp [--socket <path>] [--config <path>]\n       combine --version";

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Mcp {
        socket: Option<PathBuf>,
        config: Option<PathBuf>,
    },
    Version,
    Help,
}

/// Parses the arguments after the program name. The error is the reason, without the usage.
pub fn parse(args: Vec<OsString>) -> Result<Invocation, String> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Err("no subcommand".to_string());
    };
    match first.to_str() {
        Some("--version") => no_more(args, Invocation::Version),
        Some("help" | "--help") => no_more(args, Invocation::Help),
        Some("mcp") => parse_mcp(args),
        _ => Err(format!("unknown subcommand {}", first.to_string_lossy())),
    }
}

fn no_more(
    mut rest: impl Iterator<Item = OsString>,
    invocation: Invocation,
) -> Result<Invocation, String> {
    match rest.next() {
        None => Ok(invocation),
        Some(extra) => Err(format!("unexpected argument {}", extra.to_string_lossy())),
    }
}

fn parse_mcp(mut args: impl Iterator<Item = OsString>) -> Result<Invocation, String> {
    let mut socket = None;
    let mut config = None;
    while let Some(flag) = args.next() {
        let (slot, name) = match flag.to_str() {
            Some("--socket") => (&mut socket, "--socket"),
            Some("--config") => (&mut config, "--config"),
            _ => return Err(format!("unknown argument {}", flag.to_string_lossy())),
        };
        if slot.is_some() {
            return Err(format!("{name} was given twice"));
        }
        match args.next() {
            Some(value) => *slot = Some(PathBuf::from(value)),
            None => return Err(format!("{name} needs a value")),
        }
    }
    Ok(Invocation::Mcp { socket, config })
}

fn usage_error(reason: &str) -> ExitCode {
    eprintln!("combine: {reason}");
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// Runs the command line. `combine mcp` never returns: it ends the process itself.
pub fn main() -> ExitCode {
    let invocation = match parse(std::env::args_os().skip(1).collect()) {
        Ok(invocation) => invocation,
        Err(reason) => return usage_error(&reason),
    };
    match invocation {
        Invocation::Version => {
            println!(
                "combine {} (protocol {PROTOCOL_VERSION})",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        Invocation::Help => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Invocation::Mcp {
            socket: flag,
            config,
        } => {
            let home = home();
            let socket = match find_socket(flag, config, home.as_deref()) {
                Ok(socket) => socket,
                Err(e) => return usage_error(&e.to_string()),
            };
            relay::run(&socket, relay::caller_from_env())
        }
    }
}

fn find_socket(
    flag: Option<PathBuf>,
    config: Option<PathBuf>,
    home: Option<&std::path::Path>,
) -> Result<PathBuf, socket::SocketError> {
    let config = if flag.is_some() {
        None
    } else if let Some(path) = config {
        socket::load_config(&path, true)?
    } else if let Some(path) = socket::default_config_path(home) {
        socket::load_config(&path, false)?
    } else {
        None
    };
    socket::daemon_socket(flag, config.as_ref(), home)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn mcp(socket: Option<&str>, config: Option<&str>) -> Result<Invocation, String> {
        Ok(Invocation::Mcp {
            socket: socket.map(PathBuf::from),
            config: config.map(PathBuf::from),
        })
    }

    #[test]
    fn the_parse_table() {
        let table: Vec<(Vec<&str>, Result<Invocation, String>)> = vec![
            (vec!["mcp"], mcp(None, None)),
            (vec!["mcp", "--socket", "/a"], mcp(Some("/a"), None)),
            (vec!["mcp", "--config", "/c"], mcp(None, Some("/c"))),
            (
                vec!["mcp", "--config", "/c", "--socket", "/a"],
                mcp(Some("/a"), Some("/c")),
            ),
            (vec!["--version"], Ok(Invocation::Version)),
            (vec!["help"], Ok(Invocation::Help)),
            (vec!["--help"], Ok(Invocation::Help)),
            (vec![], Err("no subcommand".into())),
            (vec!["serve"], Err("unknown subcommand serve".into())),
            (
                vec!["mcp", "--socket", "/a", "--socket", "/b"],
                Err("--socket was given twice".into()),
            ),
            (
                vec!["mcp", "--config", "/a", "--config", "/b"],
                Err("--config was given twice".into()),
            ),
            (
                vec!["mcp", "--socket"],
                Err("--socket needs a value".into()),
            ),
            (
                vec!["mcp", "--bogus"],
                Err("unknown argument --bogus".into()),
            ),
            (vec!["mcp", "/a"], Err("unknown argument /a".into())),
            (vec!["--version", "x"], Err("unexpected argument x".into())),
        ];
        for (input, expected) in table {
            assert_eq!(parse(args(&input)), expected, "{input:?}");
        }
    }

    #[test]
    fn the_usage_names_both_forms() {
        assert!(USAGE.contains("combine mcp [--socket <path>] [--config <path>]"));
        assert!(USAGE.contains("combine --version"));
    }
}
