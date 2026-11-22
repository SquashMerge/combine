//! `combined connect`: relays stdin and stdout to the daemon's socket, so a client can reach it
//! through `ssh host combined connect`.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

const CHUNK: usize = 64 * 1024;

/// Tests set `COMBINED_CONNECT_TRACE` to a file and read which daemon-closed branch the input
/// thread took, so a test can fail when the race it targets never happened. Without the
/// `test-support` feature this does nothing.
#[cfg(feature = "test-support")]
fn trace(event: &str) {
    let Some(path) = std::env::var_os("COMBINED_CONNECT_TRACE") else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{event}");
    }
}

#[cfg(not(feature = "test-support"))]
fn trace(_event: &str) {}

/// Copies stdin to the socket until stdin ends, then closes the socket's write side. The main
/// thread copies the socket to stdout, flushing after every read, and exits 0 when the socket
/// ends or stdout is closed. It exits 1 if it cannot connect, if stdin fails to read, or on any
/// other I/O error. A socket error that says the daemon has gone (a broken pipe, a reset, or a
/// half-close of a connection the daemon already closed) is the daemon closing, so it exits 0.
/// Only bytes from the socket go to stdout.
pub fn run(socket: &Path) -> ExitCode {
    let stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("combined: cannot connect to {}: {error}", socket.display());
            return ExitCode::from(1);
        }
    };
    let upstream = match stream.try_clone() {
        Ok(clone) => clone,
        Err(error) => {
            eprintln!("combined: {error}");
            return ExitCode::from(1);
        }
    };
    let failed = Arc::new(AtomicBool::new(false));
    let stdin_failed = Arc::clone(&failed);
    thread::spawn(move || {
        let mut upstream = upstream;
        match relay(&mut io::stdin().lock(), &mut upstream) {
            Ok(()) => {}
            Err(RelayError::Socket(error)) if daemon_closed(&error) => {}
            Err(error) => {
                eprintln!("combined: {error}");
                stdin_failed.store(true, Ordering::SeqCst);
                let _ = upstream.shutdown(Shutdown::Both);
            }
        }
    });
    match copy_to_stdout(stream) {
        Ok(()) if failed.load(Ordering::SeqCst) => ExitCode::from(1),
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if daemon_closed(&error) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("combined: {error}");
            ExitCode::from(1)
        }
    }
}

/// Which side of the relay failed.
#[derive(Debug)]
enum RelayError {
    Stdin(io::Error),
    Socket(io::Error),
}

impl fmt::Display for RelayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RelayError::Stdin(error) | RelayError::Socket(error) => error.fmt(f),
        }
    }
}

/// Whether a socket error means the daemon has already closed the connection.
fn daemon_closed(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotConnected
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}

/// Copies `input` to the socket until it ends, then closes the socket's write side. A read or
/// write error is returned and leaves the socket open.
pub fn relay_input<R: Read>(input: &mut R, socket: &mut UnixStream) -> io::Result<()> {
    relay(input, socket).map_err(|error| match error {
        RelayError::Stdin(error) | RelayError::Socket(error) => error,
    })
}

fn relay<R: Read>(input: &mut R, socket: &mut UnixStream) -> Result<(), RelayError> {
    let mut buf = vec![0; CHUNK];
    loop {
        match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => socket.write_all(&buf[..n]).map_err(|error| {
                trace(&format!("input write failed: {:?}", error.kind()));
                RelayError::Socket(error)
            })?,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(RelayError::Stdin(error)),
        }
    }
    socket.shutdown(Shutdown::Write).map_err(|error| {
        trace(&format!("half-close failed: {:?}", error.kind()));
        RelayError::Socket(error)
    })
}

fn copy_to_stdout(mut socket: UnixStream) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    let mut buf = vec![0; CHUNK];
    loop {
        let n = match socket.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        match stdout.write_all(&buf[..n]).and_then(|()| stdout.flush()) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}
