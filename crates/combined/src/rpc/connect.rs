//! `combined connect`: relays stdin and stdout to the daemon's socket, so a client can reach it
//! through `ssh host combined connect`.

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::thread;

const CHUNK: usize = 64 * 1024;

/// Copies stdin to the socket until stdin ends, then closes the socket's write side. The main
/// thread copies the socket to stdout, flushing after every read, and exits 0 when the socket
/// ends or stdout is closed. It exits 1 if it cannot connect or hits an I/O error. Only bytes
/// from the socket go to stdout.
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
    thread::spawn(move || copy_stdin(upstream));
    match copy_to_stdout(stream) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("combined: {error}");
            ExitCode::from(1)
        }
    }
}

fn copy_stdin(mut socket: UnixStream) {
    let mut stdin = io::stdin().lock();
    let mut buf = vec![0; CHUNK];
    loop {
        match stdin.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if socket.write_all(&buf[..n]).is_err() {
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let _ = socket.shutdown(Shutdown::Write);
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
