#![cfg(feature = "test-support")]
//! A client of the daemon's socket, for tests.

use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use combine_protocol::{
    read_message, write_message, Ack, AttachRequestOptions, AttachmentId, ClientMessage, Cursor,
    DaemonMessage, Display, RequestId, TermId, Welcome,
};

/// How long a test waits for the daemon to send something before it fails.
pub const RECV_TIMEOUT: Duration = Duration::from_secs(5);

/// A client of the daemon's socket. A thread decodes the daemon's frames as they arrive.
pub struct WireClient {
    stream: UnixStream,
    frames: mpsc::Receiver<DaemonMessage>,
}

impl WireClient {
    /// Connects without a handshake.
    pub fn connect(socket: &Path) -> WireClient {
        let stream = UnixStream::connect(socket).unwrap();
        let mut reader = stream.try_clone().unwrap();
        let (sender, frames) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(message)) = read_message::<DaemonMessage, _>(&mut reader) {
                if sender.send(message).is_err() {
                    break;
                }
            }
        });
        WireClient { stream, frames }
    }

    /// Connects and says `hello` at the current protocol version.
    pub fn open(socket: &Path) -> (WireClient, Welcome) {
        let mut client = WireClient::connect(socket);
        client.send(&ClientMessage::Hello(combine_protocol::Hello {
            protocol_version: combine_protocol::PROTOCOL_VERSION,
        }));
        match client.recv() {
            DaemonMessage::Welcome(welcome) => (client, welcome),
            other => panic!("expected welcome, got {other:?}"),
        }
    }

    pub fn send(&mut self, message: &ClientMessage) {
        write_message(&mut self.stream, message).unwrap();
        self.stream.flush().unwrap();
    }

    pub fn send_bytes(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).unwrap();
        self.stream.flush().unwrap();
    }

    /// Like [`WireClient::send_bytes`], but reports a write failure, such as the daemon having
    /// closed the connection.
    pub fn try_send_bytes(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(bytes)?;
        self.stream.flush()
    }

    /// Closes the write side, so the daemon reads end of stream after what was sent.
    pub fn shutdown_write(&self) {
        self.stream.shutdown(Shutdown::Write).unwrap();
    }

    /// The next message; panics if none arrives within [`RECV_TIMEOUT`] or the daemon closed.
    pub fn recv(&self) -> DaemonMessage {
        self.recv_waiting(RECV_TIMEOUT)
    }

    /// [`WireClient::recv`] with its own bound, for a message that can take longer than
    /// [`RECV_TIMEOUT`] to come.
    pub fn recv_waiting(&self, wait: Duration) -> DaemonMessage {
        match self.frames.recv_timeout(wait) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => panic!("no message within {wait:?}"),
            Err(RecvTimeoutError::Disconnected) => panic!("the daemon closed the connection"),
        }
    }

    /// The next message, or `None` if none arrives within `wait` or the daemon closed.
    pub fn recv_within(&self, wait: Duration) -> Option<DaemonMessage> {
        self.frames.recv_timeout(wait).ok()
    }

    /// Panics unless the daemon closes the connection, with nothing more to read, within
    /// [`RECV_TIMEOUT`].
    pub fn expect_closed(&self) {
        match self.frames.recv_timeout(RECV_TIMEOUT) {
            Err(RecvTimeoutError::Disconnected) => {}
            Err(RecvTimeoutError::Timeout) => panic!("the daemon did not close the connection"),
            Ok(message) => panic!("expected the close, got {message:?}"),
        }
    }

    /// Acknowledges everything up to `cursor`.
    pub fn ack(&mut self, cursor: Cursor) {
        self.send(&ClientMessage::Ack(Ack { cursor }));
    }
}

impl Drop for WireClient {
    /// Closes the socket even though the decoder thread still holds a handle to it.
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// Attaches `client` to `term` and returns the attachment, with everything up to and including
/// `started` read.
pub fn attach(client: &mut WireClient, term: &TermId) -> AttachmentId {
    client.send(&ClientMessage::Attach(combine_protocol::Attach {
        request: RequestId(1),
        terminal: term.clone(),
        options: AttachRequestOptions {
            cols: 80,
            rows: 24,
            display: Display {
                foreground: "#d4d4d4".to_string(),
                background: "#1e1e1e".to_string(),
                cursor_color: "#ffffff".to_string(),
                cell_width: 8,
                cell_height: 16,
            },
            observe: false,
            history: 0,
        },
        instance: None,
    }));
    let mut attachment = None;
    loop {
        let message = client.recv();
        if let DaemonMessage::Attached(attached) = &message {
            attachment = Some(attached.attachment);
        }
        if matches!(message, DaemonMessage::Started(_)) {
            return attachment.expect("started before attached");
        }
    }
}

/// Reads until the bytes of `attachment`'s data end with `tail`, acking each event, or until
/// `within` passes: then `None`.
pub fn read_until_tail(
    client: &mut WireClient,
    attachment: AttachmentId,
    tail: &[u8],
    within: Duration,
) -> Option<Vec<u8>> {
    let deadline = Instant::now() + within;
    let mut bytes = Vec::new();
    while !bytes.ends_with(tail) {
        let left = deadline.saturating_duration_since(Instant::now());
        let message = client.recv_within(left)?;
        if let Some(cursor) = message
            .cursor()
            .filter(|c: &Cursor| c.attachment == attachment)
        {
            if let DaemonMessage::Data(data) = &message {
                bytes.extend_from_slice(&data.bytes);
            }
            client.ack(cursor);
        }
    }
    Some(bytes)
}
