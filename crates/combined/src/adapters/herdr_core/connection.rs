//! One framed connection to herdr-core: the hello handshake, JSON-RPC calls on channel 0 and
//! every other frame on one channel per connection.
//!
//! A reader task and a writer task own the socket halves. The writer drains a bounded queue
//! that callers fill with `try_send`, so a handle method never waits on the socket. The reader
//! sends each reply to its call by id and each other frame, in order, into one unbounded
//! channel; the channel exists before the first call, so frames that arrive in the same read as
//! an attach reply are never lost.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::frame::{self, Decoder, Frame, Kind};
use super::jsonrpc::{self, Hello, RpcError};
use crate::ports::{BackendError, BackendResult};

pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// Encoded frames waiting for the writer task.
pub(super) const WRITER_QUEUE_FRAMES: usize = 1_024;

const READ_CHUNK: usize = 64 * 1024;

/// What a call can come back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CallError {
    Rpc(RpcError),
    Failed(String),
}

impl CallError {
    pub(super) fn to_failure(&self) -> BackendError {
        match self {
            CallError::Rpc(error) => error.to_failure(),
            CallError::Failed(detail) => BackendError::Failed(detail.clone()),
        }
    }
}

#[derive(Default)]
struct Pending {
    calls: HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>,
    closed: bool,
}

type SharedPending = Arc<Mutex<Pending>>;

fn lock(pending: &SharedPending) -> std::sync::MutexGuard<'_, Pending> {
    pending.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) struct Connection {
    writer: Option<mpsc::Sender<Vec<u8>>>,
    pending: SharedPending,
    next_id: AtomicU64,
    reader: JoinHandle<()>,
    writer_task: Option<JoinHandle<()>>,
    hello: Hello,
}

/// A connection and the receiving end of its non-RPC frames.
pub(super) type Opened = (Connection, mpsc::UnboundedReceiver<Frame>);

impl Connection {
    /// Connects to the socket and runs the handshake. The caller passes a path it has checked.
    pub(super) async fn open(socket: &Path) -> BackendResult<Opened> {
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(socket))
            .await
            .map_err(|_| BackendError::Failed("connecting to herdr-core timed out".to_string()))?
            .map_err(|error| BackendError::Failed(format!("connecting to herdr-core: {error}")))?;
        Connection::from_stream(stream).await
    }

    pub(super) async fn from_stream<S>(stream: S) -> BackendResult<Opened>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (mut read, mut write) = tokio::io::split(stream);
        let (hello, leftover) = tokio::time::timeout(HELLO_TIMEOUT, async {
            write
                .write_all(jsonrpc::hello_line().as_bytes())
                .await
                .map_err(|error| BackendError::Failed(format!("sending hello: {error}")))?;
            read_hello(&mut read).await
        })
        .await
        .map_err(|_| BackendError::Failed("herdr-core did not answer hello".to_string()))??;

        let pending = SharedPending::default();
        let (frames_tx, frames_rx) = mpsc::unbounded_channel();
        let (writer_tx, writer_rx) = mpsc::channel(WRITER_QUEUE_FRAMES);
        let reader = tokio::spawn(read_loop(read, leftover, Arc::clone(&pending), frames_tx));
        let writer_task = tokio::spawn(write_loop(write, writer_rx));
        Ok((
            Connection {
                writer: Some(writer_tx),
                pending,
                next_id: AtomicU64::new(1),
                reader,
                writer_task: Some(writer_task),
                hello,
            },
            frames_rx,
        ))
    }

    pub(super) fn hello(&self) -> &Hello {
        &self.hello
    }

    /// The queue a handle sends encoded frames into.
    pub(super) fn sender(&self) -> Option<mpsc::Sender<Vec<u8>>> {
        self.writer.clone()
    }

    pub(super) async fn call(&self, method: &str, params: Value) -> Result<Value, CallError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply_tx, reply_rx) = oneshot::channel();
        {
            let mut pending = lock(&self.pending);
            if pending.closed {
                return Err(CallError::Failed("the connection is closed".to_string()));
            }
            pending.calls.insert(id, reply_tx);
        }
        let sent = match &self.writer {
            Some(writer) => writer
                .send(frame::rpc(&jsonrpc::request(id, method, params)))
                .await
                .is_ok(),
            None => false,
        };
        if !sent {
            lock(&self.pending).calls.remove(&id);
            return Err(CallError::Failed("the connection is closed".to_string()));
        }
        match tokio::time::timeout(CALL_TIMEOUT, reply_rx).await {
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(error))) => Err(CallError::Rpc(error)),
            Ok(Err(_)) => Err(CallError::Failed(
                "the connection closed before the reply".to_string(),
            )),
            Err(_) => {
                lock(&self.pending).calls.remove(&id);
                Err(CallError::Failed(format!("{method} timed out")))
            }
        }
    }

    /// Lets the writer flush what is queued, then ends the connection.
    pub(super) fn close(mut self) {
        self.writer = None;
        self.writer_task = None;
        self.reader.abort();
    }
}

impl Drop for Connection {
    /// Ends both tasks, dropping whatever the writer still held.
    fn drop(&mut self) {
        self.reader.abort();
        if let Some(task) = &self.writer_task {
            task.abort();
        }
    }
}

async fn read_hello<R: AsyncRead + Unpin>(read: &mut R) -> BackendResult<(Hello, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = vec![0u8; READ_CHUNK];
    loop {
        if let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
            let hello = jsonrpc::parse_hello(&buffer[..end])?;
            return Ok((hello, buffer[end + 1..].to_vec()));
        }
        let count = read
            .read(&mut chunk)
            .await
            .map_err(|error| BackendError::Failed(format!("reading hello: {error}")))?;
        if count == 0 {
            return Err(BackendError::Failed(
                "herdr-core closed before answering hello".to_string(),
            ));
        }
        buffer.extend_from_slice(&chunk[..count]);
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(mut write: W, mut queue: mpsc::Receiver<Vec<u8>>) {
    while let Some(bytes) = queue.recv().await {
        if write.write_all(&bytes).await.is_err() {
            return;
        }
    }
    let _ = write.shutdown().await;
}

async fn read_loop<R: AsyncRead + Unpin>(
    mut read: R,
    leftover: Vec<u8>,
    pending: SharedPending,
    frames: mpsc::UnboundedSender<Frame>,
) {
    let mut decoder = Decoder::new();
    decoder.push(&leftover);
    let mut chunk = vec![0u8; READ_CHUNK];
    'read: loop {
        loop {
            match decoder.next_frame() {
                Ok(Some(frame)) => route(frame, &pending, &frames),
                Ok(None) => break,
                Err(_) => break 'read,
            }
        }
        match read.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(count) => decoder.push(&chunk[..count]),
        }
    }
    let mut pending = lock(&pending);
    pending.closed = true;
    pending.calls.clear();
}

fn route(frame: Frame, pending: &SharedPending, frames: &mpsc::UnboundedSender<Frame>) {
    if frame.channel == 0 {
        if frame.kind != Kind::Data {
            return;
        }
        if let Some((id, result)) = jsonrpc::parse_response(&frame.payload) {
            if let Some(reply) = lock(pending).calls.remove(&id) {
                let _ = reply.send(result);
            }
        }
        return;
    }
    let _ = frames.send(frame);
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::DuplexStream;

    use super::*;

    fn hello_reply() -> Vec<u8> {
        let mut line = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "result": {
                "generation": 1,
                "version": "0.1.21",
                "framed": true,
                "core": {"id": "x", "name": "y"},
                "methods": [
                    "terminal.attach", "terminal.list", "terminal.run", "terminal.kill",
                    "events.subscribe"
                ],
                "features": ["automatic_input"],
            }
        })
        .to_string()
        .into_bytes();
        line.push(b'\n');
        line
    }

    async fn read_line(server: &mut DuplexStream) -> Vec<u8> {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while server.read_exact(&mut byte).await.is_ok() {
            if byte[0] == b'\n' {
                break;
            }
            line.push(byte[0]);
        }
        line
    }

    #[tokio::test]
    async fn bytes_after_the_hello_line_are_framed() {
        let (client, mut server) = tokio::io::duplex(65_536);
        let mut reply = hello_reply();
        reply.extend(frame::encode(5, Kind::Data, b"first paint"));
        let serve = tokio::spawn(async move {
            let line = read_line(&mut server).await;
            assert!(String::from_utf8_lossy(&line).contains("\"framed\":true"));
            server.write_all(&reply).await.expect("write");
            server
        });
        let (connection, mut frames) = Connection::from_stream(client).await.expect("open");
        assert!(connection.hello().has_feature("automatic_input"));
        let frame = frames.recv().await.expect("a frame");
        assert_eq!((frame.channel, frame.payload), (5, b"first paint".to_vec()));
        let _server = serve.await.expect("server");
    }

    #[tokio::test]
    async fn frames_in_the_same_read_as_the_attach_reply_reach_the_channel() {
        let (client, mut server) = tokio::io::duplex(65_536);
        let serve = tokio::spawn(async move {
            read_line(&mut server).await;
            server.write_all(&hello_reply()).await.expect("write");
            let mut header = [0u8; frame::HEADER_LEN];
            server
                .read_exact(&mut header)
                .await
                .expect("a request frame");
            let length = u32::from_le_bytes([header[5], header[6], header[7], header[8]]);
            let mut body = vec![0u8; length as usize];
            server.read_exact(&mut body).await.expect("body");
            let request: Value = serde_json::from_slice(&body).expect("json");
            let mut both = frame::rpc(
                json!({"jsonrpc": "2.0", "id": request["id"], "result": {"channel": 1}})
                    .to_string()
                    .as_bytes(),
            );
            both.extend(frame::encode(1, Kind::Resize, &[80, 0, 24, 0]));
            both.extend(frame::encode(1, Kind::Data, b"paint"));
            server.write_all(&both).await.expect("write");
            server
        });
        let (connection, mut frames) = Connection::from_stream(client).await.expect("open");
        let result = connection
            .call("terminal.attach", json!({}))
            .await
            .expect("the reply");
        assert_eq!(result["channel"], json!(1));
        let first = frames.recv().await.expect("resize");
        let second = frames.recv().await.expect("paint");
        assert_eq!(
            (first.kind, second.payload),
            (Kind::Resize, b"paint".to_vec())
        );
        let _server = serve.await.expect("server");
    }
}
