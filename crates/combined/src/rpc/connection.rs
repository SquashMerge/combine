//! One client connection: a reader thread that does the handshake and then feeds the server's
//! inbox, and a writer thread that writes the batches the server hands it.

use std::io::{BufWriter, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::Duration;

use combine_protocol::{
    daemon_handshake, read_message, write_message, ClientMessage, DaemonMessage, DecodeError,
    EncodeError, ErrorCode, ErrorMessage, HandshakeError, HostId, ReadError, WriteError,
};
use tokio::sync::mpsc;

use crate::core::ConnectionId;

const WRITE_BUFFER: usize = 64 * 1024;

/// What a connection's threads tell the server.
#[derive(Debug)]
pub(super) enum ConnEvent {
    Message(ConnectionId, ClientMessage),
    /// A frame that could not be decoded, after the handshake. The reader has stopped.
    Malformed(ConnectionId, String),
    /// The connection ended or failed.
    Closed(ConnectionId),
}

/// What the server tells a connection's writer.
#[derive(Debug)]
pub(super) enum WriterCmd {
    Batch(Vec<DaemonMessage>),
    /// Writes the message if there is one, then closes the socket.
    Close(Option<DaemonMessage>),
}

pub(super) struct Spawn {
    pub conn: ConnectionId,
    pub stream: UnixStream,
    pub commands: mpsc::Receiver<WriterCmd>,
    pub inbox: mpsc::Sender<ConnEvent>,
    pub host_id: HostId,
    pub handshake_timeout: Duration,
}

pub(super) fn spawn(parts: Spawn) -> std::io::Result<()> {
    let name = format!("combined-conn-{}", parts.conn.0);
    thread::Builder::new()
        .name(name)
        .spawn(move || read_side(parts))
        .map(drop)
}

fn error_message(code: ErrorCode, message: String) -> DaemonMessage {
    DaemonMessage::Error(ErrorMessage {
        code,
        message,
        request: None,
        attachment: None,
    })
}

/// Writes `message` and flushes, for the replies that go out of turn during the handshake.
fn reply(stream: &mut UnixStream, message: DaemonMessage) {
    let _ = write_message(stream, &message).and_then(|()| stream.flush().map_err(WriteError::from));
}

fn read_side(parts: Spawn) {
    let Spawn {
        conn,
        mut stream,
        commands,
        inbox,
        host_id,
        handshake_timeout,
    } = parts;
    let Ok(mut write_half) = stream.try_clone() else {
        let _ = inbox.blocking_send(ConnEvent::Closed(conn));
        return;
    };
    let handshake = stream
        .set_read_timeout(Some(handshake_timeout))
        .map_err(|_| HandshakeError::Closed)
        .and_then(|()| daemon_handshake(&mut stream, &mut write_half, &host_id));
    match handshake {
        Ok(_) => {}
        Err(failure) => {
            match failure {
                HandshakeError::Read(ReadError::Decode(error)) => {
                    reply(&mut write_half, malformed(&error));
                }
                HandshakeError::UnexpectedMessage { type_name } => reply(
                    &mut write_half,
                    error_message(
                        ErrorCode::InvalidRequest,
                        format!("expected hello, got {type_name}"),
                    ),
                ),
                _ => {}
            }
            let _ = stream.shutdown(Shutdown::Both);
            let _ = inbox.blocking_send(ConnEvent::Closed(conn));
            return;
        }
    }
    if stream.set_read_timeout(None).is_err() {
        let _ = stream.shutdown(Shutdown::Both);
        let _ = inbox.blocking_send(ConnEvent::Closed(conn));
        return;
    }
    let writer_inbox = inbox.clone();
    let spawned = thread::Builder::new()
        .name(format!("combined-write-{}", conn.0))
        .spawn(move || write_side(conn, write_half, commands, writer_inbox));
    if spawned.is_err() {
        let _ = stream.shutdown(Shutdown::Both);
        let _ = inbox.blocking_send(ConnEvent::Closed(conn));
        return;
    }
    loop {
        let event = match read_message::<ClientMessage, _>(&mut stream) {
            Ok(Some(message)) => ConnEvent::Message(conn, message),
            Ok(None) | Err(ReadError::Io(_)) => {
                let _ = inbox.blocking_send(ConnEvent::Closed(conn));
                return;
            }
            Err(ReadError::Decode(error)) => {
                let _ = inbox.blocking_send(ConnEvent::Malformed(conn, error.to_string()));
                return;
            }
        };
        if inbox.blocking_send(event).is_err() {
            return;
        }
    }
}

fn malformed(error: &DecodeError) -> DaemonMessage {
    error_message(ErrorCode::Malformed, format!("malformed frame: {error}"))
}

/// Writes one message. A terminal list too large for a frame becomes an `internal` error.
fn write_one<W: Write>(out: &mut W, message: &DaemonMessage) -> Result<(), WriteError> {
    match write_message(out, message) {
        Err(WriteError::Encode(EncodeError::FrameTooLarge { .. })) => {
            let request = match message {
                DaemonMessage::Terminals(terminals) => terminals.request,
                _ => None,
            };
            write_message(
                out,
                &DaemonMessage::Error(ErrorMessage {
                    code: ErrorCode::Internal,
                    message: "the terminal list is larger than one frame".to_string(),
                    request,
                    attachment: None,
                }),
            )
        }
        other => other,
    }
}

fn write_side(
    conn: ConnectionId,
    stream: UnixStream,
    mut commands: mpsc::Receiver<WriterCmd>,
    inbox: mpsc::Sender<ConnEvent>,
) {
    let Ok(socket) = stream.try_clone() else {
        let _ = inbox.blocking_send(ConnEvent::Closed(conn));
        return;
    };
    let mut out = BufWriter::with_capacity(WRITE_BUFFER, stream);
    loop {
        let result = match commands.blocking_recv() {
            Some(WriterCmd::Batch(messages)) => messages
                .iter()
                .try_for_each(|message| write_one(&mut out, message))
                .and_then(|()| out.flush().map_err(WriteError::from)),
            Some(WriterCmd::Close(message)) => {
                if let Some(message) = message {
                    let _ = write_one(&mut out, &message);
                }
                let _ = out.flush();
                let _ = socket.shutdown(Shutdown::Both);
                return;
            }
            None => {
                let _ = out.flush();
                let _ = socket.shutdown(Shutdown::Both);
                return;
            }
        };
        if result.is_err() {
            let _ = socket.shutdown(Shutdown::Both);
            let _ = inbox.blocking_send(ConnEvent::Closed(conn));
            return;
        }
    }
}
