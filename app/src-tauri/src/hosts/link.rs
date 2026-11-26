//! The link to a host: connector, failure text and exit info.
//!
//! A [`Link`] is the byte stream of one connect attempt: an `ssh` child's stdio or a Unix
//! socket. Whoever holds a link ends it with [`Link::finish`] or [`abort_link`], so an `ssh`
//! child is always reaped.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

/// The longest one read of a socket link blocks before it looks at the socket again.
pub const READ_TICK: Duration = Duration::from_millis(500);

/// A socket whose reads can be given a timeout.
pub trait TimedSocket: Read + Send {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
}

impl TimedSocket for UnixStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        UnixStream::set_read_timeout(self, timeout)
    }
}

/// The read end of a socket link: a read that a shutdown always ends.
///
/// macOS can lose the wake-up of a read that is going to sleep just as its socket, or the peer,
/// is shut down: `shutdown` returns `Ok`, and the read sleeps on (#82). The closer's shutdown
/// would then never end the read, and stopping the supervisor would wait for ever. So each read
/// blocks for at most [`READ_TICK`] and is then made again. The new read sees the shut-down
/// socket and returns end of file. The caller never sees a tick.
pub fn ticking_reader<S: TimedSocket + 'static>(socket: S) -> io::Result<Box<dyn Read + Send>> {
    socket.set_read_timeout(Some(READ_TICK))?;
    Ok(Box::new(TickingReader(socket)))
}

struct TickingReader<S>(S);

impl<S: Read> Read for TickingReader<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.0.read(buf) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                result => return result,
            }
        }
    }
}

/// Combine's wording for a failure, plus the external text that explains it.
///
/// `text` is Combine text and is published whole. `detail` is raw external text, such as an OS
/// error, `ssh` stderr or a daemon's message, and is trimmed where snapshots are made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureText {
    pub text: String,
    pub detail: Option<String>,
}

/// How a link ended: the exit status of an `ssh` child and the tail of its stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub stderr: String,
}

/// Unblocks a blocked read: kills the child, or shuts the socket down.
pub type Closer = Arc<dyn Fn() + Send + Sync>;

/// Reports how the other end of a link ended, after the link's streams are dropped.
pub trait ExitReport: Send {
    fn finish(self: Box<Self>) -> Option<ExitInfo>;
}

pub struct Link {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    closer: Closer,
    exit: Box<dyn ExitReport>,
}

impl Link {
    pub fn new(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        closer: Closer,
        exit: Box<dyn ExitReport>,
    ) -> Link {
        Link {
            reader,
            writer,
            closer,
            exit,
        }
    }

    pub fn closer(&self) -> Closer {
        Arc::clone(&self.closer)
    }

    /// Drops the reader and the writer, then asks how the link ended.
    pub fn finish(self) -> Option<ExitInfo> {
        let Link {
            reader,
            writer,
            closer: _,
            exit,
        } = self;
        drop(reader);
        drop(writer);
        exit.finish()
    }
}

/// Kills an `ssh` child or shuts the socket, then reaps.
pub fn abort_link(link: Link) {
    (link.closer())();
    let _ = link.finish();
}

pub trait Connector: Send + Sync {
    fn connect(&self) -> Result<Link, ConnectError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// Retrying cannot help until the user acts.
    Permanent(FailureText),
    /// Worth retrying after a delay.
    Transient(FailureText),
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use super::super::testing::{LostWakeupSocket, WAIT};
    use super::*;

    #[test]
    fn a_read_tick_is_not_seen_by_the_caller() {
        let socket = LostWakeupSocket::new(Vec::new());
        let mut reader = ticking_reader(socket.clone()).expect("set the read timeout");
        let (done, read) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0u8; 8];
            let _ = done.send(reader.read(&mut buf).map(|n| buf[..n].to_vec()));
        });
        socket.wait_blocked(2);
        socket.push(b"frame");
        let got = read.recv_timeout(WAIT).expect("the read returns");
        assert_eq!(got.expect("no error from a tick"), b"frame");
    }

    #[test]
    fn a_socket_link_reads_with_the_tick_as_its_timeout() {
        let (end, _peer) = UnixStream::pair().expect("socket pair");
        let _reader = ticking_reader(end.try_clone().expect("clone")).expect("set the timeout");
        assert_eq!(end.read_timeout().expect("read timeout"), Some(READ_TICK));
    }
}
