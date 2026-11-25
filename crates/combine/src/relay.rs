//! The relay: one agent's stdin and stdout against one MCP channel on the daemon's socket.

use std::ffi::OsStr;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use combine_protocol::{
    client_handshake, read_message, ClientMessage, DaemonMessage, DecodeError, EncodeError, Mcp,
    McpCaller, McpOpen, ReadError, RequestId, WireMessage,
};
use serde::de::IgnoredAny;
use serde::Deserialize;
use serde_json::value::RawValue;

/// The longest line the shim sends: one frame's worth of JSON.
const MAX_LINE: usize = 1_048_576;
/// The longest request `id` the shim echoes in a reply it writes itself.
const MAX_ID_BYTES: usize = 1_024;
/// The longest piece of serde's error text the shim echoes.
const MAX_DETAIL: usize = 256;
/// How long the handshake and `mcp_open` may take together.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
const STDIN_BUFFER: usize = 64 * 1024;
const OPEN_REQUEST: RequestId = RequestId(1);

/// The caller's environment, read once: empty values count as unset.
pub fn caller_from_env() -> McpCaller {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .and_then(|value| value.into_string().ok())
    };
    let pwd = std::env::var_os("PWD").filter(|value| !value.is_empty());
    McpCaller {
        terminal_id: var("HC_TERMINAL_ID"),
        terminal_socket: var("HC_TERMINAL_SOCKET"),
        cwd: std::env::current_dir()
            .ok()
            .and_then(|current| caller_cwd(pwd.as_deref(), &current)),
    }
}

/// `$PWD` when it is absolute and the same directory (device and inode) as `current`, else
/// `current`. `None` when the chosen path isn't UTF-8.
pub fn caller_cwd(pwd: Option<&OsStr>, current: &Path) -> Option<String> {
    let same_directory = |pwd: &Path| {
        let (a, b) = (
            std::fs::metadata(pwd).ok()?,
            std::fs::metadata(current).ok()?,
        );
        Some(a.dev() == b.dev() && a.ino() == b.ino())
    };
    let chosen = match pwd.map(Path::new) {
        Some(pwd) if pwd.is_absolute() && same_directory(pwd) == Some(true) => pwd,
        _ => current,
    };
    chosen.to_str().map(str::to_owned)
}

/// What the shim does with one stdin line.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    Skip,
    /// An encoded `mcp` frame for the daemon.
    Frame(Vec<u8>),
    /// A JSON-RPC error line the shim writes to stdout itself.
    Reply(String),
}

fn clipped(text: &str) -> &str {
    let mut end = text.len().min(MAX_DETAIL);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A JSON-RPC error line. `id` is the request's raw JSON text, or `None` for `null`.
fn error_line(id: Option<&str>, code: i32, message: &str) -> String {
    let message = serde_json::to_string(message).unwrap_or_else(|_| "\"error\"".to_string());
    format!(
        r#"{{"jsonrpc":"2.0","id":{},"error":{{"code":{code},"message":{message}}}}}"#,
        id.unwrap_or("null")
    )
}

#[derive(Deserialize)]
struct IdOnly<'a> {
    #[serde(borrow, default)]
    id: Option<&'a RawValue>,
}

/// The request's `id` as raw JSON text, when it may be echoed: a string or a number of at most
/// 1,024 bytes.
fn echoable_id(text: &str) -> Option<&str> {
    let id = serde_json::from_str::<IdOnly<'_>>(text).ok()?.id?.get();
    let starts_right = id.starts_with(['"', '-']) || id.starts_with(|c: char| c.is_ascii_digit());
    (starts_right && id.len() <= MAX_ID_BYTES).then_some(id)
}

/// The reply for a line over the frame cap that the shim didn't read to its end.
pub fn oversize_reply() -> String {
    error_line(
        None,
        -32600,
        &format!("Invalid Request: the message is larger than one frame ({MAX_LINE} bytes)"),
    )
}

/// Decides what one line (without its newline, or with it) becomes.
pub fn classify_line(bytes: &[u8]) -> Line {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Line::Skip;
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Line::Reply(error_line(
            None,
            -32700,
            "Parse error: the line isn't valid UTF-8",
        ));
    };
    if text.trim_start().starts_with('{') {
        let message = match combine_protocol::JsonRpcMessage::new(text.to_owned()) {
            Ok(message) => message,
            Err(e) => return Line::Reply(parse_error(&e)),
        };
        return match ClientMessage::Mcp(Mcp { message }).encode() {
            Ok(frame) => Line::Frame(frame),
            Err(EncodeError::FrameTooLarge { .. }) => Line::Reply(error_line(
                echoable_id(text),
                -32600,
                &format!(
                    "Invalid Request: the message is larger than one frame ({MAX_LINE} bytes)"
                ),
            )),
            Err(e) => Line::Reply(error_line(
                None,
                -32603,
                &format!("Internal error: {}", clipped(&e.to_string())),
            )),
        };
    }
    match serde_json::from_str::<IgnoredAny>(text) {
        Ok(_) => Line::Reply(error_line(
            None,
            -32600,
            "Invalid Request: a JSON-RPC message must be one JSON object; batches aren't supported",
        )),
        Err(e) => Line::Reply(error_line(
            None,
            -32700,
            &format!("Parse error: {}", clipped(&e.to_string())),
        )),
    }
}

fn parse_error(e: &DecodeError) -> String {
    let detail = match e {
        DecodeError::BadJson { detail } => detail.clone(),
        other => other.to_string(),
    };
    error_line(None, -32700, &format!("Parse error: {}", clipped(&detail)))
}

/// Writes one line to stdout under the stdout lock, so no other write can split it.
fn write_stdout_line(line: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    out.write_all(&bytes)?;
    out.flush()
}

/// Ends the process while holding the stdout lock, so a line being written is never cut.
/// `message`, when given, goes to stderr first.
fn finish(code: i32, message: Option<&str>) -> ! {
    let _stdout = io::stdout().lock();
    if let Some(message) = message {
        eprintln!("combine: {message}");
    }
    std::process::exit(code)
}

fn code_text(code: combine_protocol::ErrorCode) -> String {
    serde_json::to_string(&code)
        .map(|text| text.trim_matches('"').to_owned())
        .unwrap_or_else(|_| "unknown".to_owned())
}

fn read_failure(e: &ReadError) -> String {
    match e {
        ReadError::Io(_) => {
            "the Combine daemon closed the connection; restart this MCP server to reconnect"
                .to_owned()
        }
        ReadError::Decode(e) => format!("the daemon sent a malformed frame: {e}"),
    }
}

fn timed_out(e: &ReadError) -> bool {
    matches!(e, ReadError::Io(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut))
}

const CLOSED: &str =
    "the Combine daemon closed the connection; restart this MCP server to reconnect";

/// Connects, opens the MCP channel and relays until stdin ends (exit 0) or the daemon goes away
/// (exit 1). It never reconnects.
pub fn run(socket: &Path, caller: McpCaller) -> ! {
    let stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(e) => finish(
            1,
            Some(&format!(
                "cannot connect to the Combine daemon at {}: {e}",
                socket.display()
            )),
        ),
    };
    let (mut reader, writer) = match (stream.try_clone(), stream.try_clone()) {
        (Ok(reader), Ok(writer)) => (BufReader::with_capacity(STDIN_BUFFER, reader), writer),
        (Err(e), _) | (_, Err(e)) => finish(1, Some(&format!("cannot use the socket: {e}"))),
    };
    open_channel(&stream, &mut reader, caller);

    let stdin_writer = writer;
    let stdin_thread = std::thread::Builder::new()
        .name("stdin".to_owned())
        .spawn(move || relay_stdin(stdin_writer));
    if let Err(e) = stdin_thread {
        finish(1, Some(&format!("cannot start the stdin reader: {e}")));
    }
    relay_daemon(&mut reader)
}

/// The handshake and `mcp_open`, within [`OPEN_TIMEOUT`] altogether. Returns once the daemon
/// answered `mcp_opened`; any other outcome ends the process.
fn open_channel(stream: &UnixStream, reader: &mut BufReader<UnixStream>, caller: McpCaller) {
    let started = Instant::now();
    let remaining = || OPEN_TIMEOUT.checked_sub(started.elapsed());
    let _ = stream.set_read_timeout(Some(OPEN_TIMEOUT));
    let _ = stream.set_write_timeout(Some(OPEN_TIMEOUT));
    let mut write_half = stream;
    if let Err(e) = client_handshake(reader, &mut write_half) {
        finish(1, Some(&format!("the daemon refused the connection: {e}")));
    }
    let open = ClientMessage::McpOpen(McpOpen {
        request: OPEN_REQUEST,
        caller,
    });
    let sent = open.encode().map_err(|e| e.to_string()).and_then(|frame| {
        write_half
            .write_all(&frame)
            .and_then(|()| write_half.flush())
            .map_err(|e| e.to_string())
    });
    if let Err(e) = sent {
        finish(1, Some(&format!("cannot send mcp_open: {e}")));
    }
    let no_answer = "the daemon didn't answer mcp_open within 10 s";
    loop {
        let Some(left) = remaining().filter(|left| !left.is_zero()) else {
            finish(1, Some(no_answer));
        };
        let _ = stream.set_read_timeout(Some(left));
        match read_message::<DaemonMessage, _>(reader) {
            Ok(Some(DaemonMessage::McpOpened(opened))) if opened.request == OPEN_REQUEST => {
                let _ = stream.set_read_timeout(None);
                let _ = stream.set_write_timeout(None);
                return;
            }
            Ok(Some(DaemonMessage::Error(e))) if e.request == Some(OPEN_REQUEST) => finish(
                1,
                Some(&format!(
                    "the daemon refused the MCP channel ({}): {}",
                    code_text(e.code),
                    e.trimmed_message()
                )),
            ),
            Ok(Some(_)) => {}
            Ok(None) => finish(1, Some(CLOSED)),
            Err(e) if timed_out(&e) => finish(1, Some(no_answer)),
            Err(e) => finish(1, Some(&read_failure(&e))),
        }
    }
}

/// Reads stdin lines and sends each to the daemon. A write failure shuts the socket down so the
/// main thread's read ends. Stdin ending ends the process with 0.
fn relay_stdin(mut socket: UnixStream) {
    let mut stdin = BufReader::with_capacity(STDIN_BUFFER, io::stdin().lock());
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = (&mut stdin)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut line);
        match read {
            Ok(0) | Err(_) => finish(0, None),
            Ok(_) => {}
        }
        let reply = if line.len() > MAX_LINE && !line.ends_with(b"\n") {
            skip_to_newline(&mut stdin);
            Line::Reply(oversize_reply())
        } else {
            classify_line(&line)
        };
        match reply {
            Line::Skip => {}
            Line::Reply(text) => {
                if write_stdout_line(&text).is_err() {
                    finish(0, None);
                }
            }
            Line::Frame(frame) => {
                if socket
                    .write_all(&frame)
                    .and_then(|()| socket.flush())
                    .is_err()
                {
                    let _ = socket.shutdown(Shutdown::Both);
                    return;
                }
            }
        }
    }
}

/// Discards input up to and including the next newline, or to the end of the stream.
fn skip_to_newline(stdin: &mut impl BufRead) {
    loop {
        let available = match stdin.fill_buf() {
            Ok(available) if !available.is_empty() => available,
            _ => return,
        };
        match available.iter().position(|&b| b == b'\n') {
            Some(at) => {
                stdin.consume(at + 1);
                return;
            }
            None => {
                let all = available.len();
                stdin.consume(all);
            }
        }
    }
}

/// Writes the daemon's `mcp` frames to stdout. Daemon loss ends the process with 1.
fn relay_daemon(reader: &mut BufReader<UnixStream>) -> ! {
    loop {
        match read_message::<DaemonMessage, _>(reader) {
            Ok(Some(DaemonMessage::Mcp(Mcp { message }))) => {
                if write_stdout_line(message.as_str()).is_err() {
                    finish(0, None);
                }
            }
            Ok(Some(DaemonMessage::Error(e))) if e.request.is_none() => eprintln!(
                "combine: the daemon reported {}: {}",
                code_text(e.code),
                e.trimmed_message()
            ),
            Ok(Some(_)) => {}
            Ok(None) => finish(1, Some(CLOSED)),
            Err(e) => finish(1, Some(&read_failure(&e))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(line: Line) -> String {
        match line {
            Line::Reply(text) => text,
            other => panic!("expected a reply, got {other:?}"),
        }
    }

    fn error_of(line: &[u8]) -> (String, i64, String) {
        let text = reply(classify_line(line));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        (
            value["id"].to_string(),
            value["error"]["code"].as_i64().unwrap(),
            value["error"]["message"].as_str().unwrap().to_owned(),
        )
    }

    #[test]
    fn blank_lines_are_skipped() {
        for line in [&b""[..], b"\n", b"\r\n", b"  \t\r\n"] {
            assert_eq!(classify_line(line), Line::Skip, "{line:?}");
        }
    }

    #[test]
    fn an_object_becomes_a_frame_with_its_text_intact() {
        let text = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        for ending in [&b""[..], b"\n", b"\r\n"] {
            let mut line = text.to_vec();
            line.extend_from_slice(ending);
            let Line::Frame(frame) = classify_line(&line) else {
                panic!("expected a frame");
            };
            let mut stream = &frame[..];
            let message = read_message::<ClientMessage, _>(&mut stream)
                .unwrap()
                .unwrap();
            let ClientMessage::Mcp(Mcp { message }) = message else {
                panic!("expected an mcp frame");
            };
            assert_eq!(message.as_str().as_bytes(), text);
        }
    }

    #[test]
    fn bad_utf8_is_a_parse_error() {
        let (id, code, message) = error_of(b"{\"a\":\"\xff\"}");
        assert_eq!(
            (id.as_str(), code, message.as_str()),
            ("null", -32700, "Parse error: the line isn't valid UTF-8")
        );
    }

    #[test]
    fn an_unfinished_object_is_a_parse_error() {
        let (id, code, message) = error_of(br#"{"jsonrpc":"2.0","id":1"#);
        assert_eq!((id.as_str(), code), ("null", -32700));
        assert!(message.starts_with("Parse error: "), "{message}");
    }

    #[test]
    fn a_batch_or_a_scalar_is_an_invalid_request() {
        for line in [
            &br#"[{"jsonrpc":"2.0"}]"#[..],
            b"1",
            b"\"x\"",
            b"null",
            b"true",
        ] {
            let (id, code, message) = error_of(line);
            assert_eq!(
                (id.as_str(), code, message.as_str()),
                (
                    "null",
                    -32600,
                    "Invalid Request: a JSON-RPC message must be one JSON object; \
                     batches aren't supported"
                ),
                "{line:?}"
            );
        }
    }

    #[test]
    fn text_that_is_not_json_is_a_parse_error() {
        for line in [&b"hello"[..], b"[1,", b"{\"a\" 1}x"] {
            let (id, code, message) = error_of(line);
            assert_eq!((id.as_str(), code), ("null", -32700), "{line:?}");
            assert!(message.starts_with("Parse error: "), "{message}");
        }
    }

    /// A one-line request with `id` and enough padding to be `total` bytes long.
    fn padded(id: &str, total: usize) -> Vec<u8> {
        let head = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"m","params":""#);
        let tail = r#""}"#;
        let padding = total - head.len() - tail.len();
        format!("{head}{}{tail}", "p".repeat(padding)).into_bytes()
    }

    #[test]
    fn the_largest_line_that_fits_a_frame_is_sent() {
        let line = padded("1", MAX_LINE - 100);
        assert!(matches!(classify_line(&line), Line::Frame(_)));
    }

    #[test]
    fn a_line_too_big_for_a_frame_echoes_an_id_of_1024_bytes_but_not_1025() {
        let message =
            format!("Invalid Request: the message is larger than one frame ({MAX_LINE} bytes)");
        let id_of_len = |len: usize| format!("\"{}\"", "i".repeat(len - 2));
        let at_limit = id_of_len(1_024);
        let (id, code, text) = error_of(&padded(&at_limit, MAX_LINE));
        assert_eq!((code, text.as_str()), (-32600, message.as_str()));
        assert_eq!(serde_json::from_str::<String>(&id).unwrap().len(), 1_022);
        let over = id_of_len(1_025);
        let (id, code, text) = error_of(&padded(&over, MAX_LINE));
        assert_eq!(
            (id.as_str(), code, text.as_str()),
            ("null", -32600, message.as_str())
        );
    }

    #[test]
    fn a_number_id_is_echoed_and_an_object_id_is_not() {
        let (id, ..) = error_of(&padded("-12", MAX_LINE));
        assert_eq!(id, "-12");
        let (id, ..) = error_of(&padded(r#"{"a":1}"#, MAX_LINE));
        assert_eq!(id, "null");
        let (id, ..) = error_of(&padded("null", MAX_LINE));
        assert_eq!(id, "null");
        let (id, ..) = error_of(&padded("true", MAX_LINE));
        assert_eq!(id, "null");
    }

    #[test]
    fn the_oversize_reply_has_a_null_id() {
        let value: serde_json::Value = serde_json::from_str(&oversize_reply()).unwrap();
        assert_eq!(value["id"], serde_json::Value::Null);
        assert_eq!(value["error"]["code"], -32600);
    }

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("combine-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn pwd_is_used_when_it_names_the_current_directory() {
        let scratch = Scratch::new("cwd-same");
        let real = scratch.0.join("real");
        std::fs::create_dir(&real).unwrap();
        let link = scratch.0.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(
            caller_cwd(Some(link.as_os_str()), &real),
            Some(link.to_str().unwrap().to_owned())
        );
    }

    #[test]
    fn a_pwd_naming_another_directory_is_ignored() {
        let scratch = Scratch::new("cwd-other");
        let (a, b) = (scratch.0.join("a"), scratch.0.join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        assert_eq!(
            caller_cwd(Some(b.as_os_str()), &a),
            Some(a.to_str().unwrap().to_owned())
        );
    }

    #[test]
    fn a_relative_missing_or_unset_pwd_falls_back_to_the_current_directory() {
        let scratch = Scratch::new("cwd-fallback");
        let current = scratch.0.clone();
        let expected = Some(current.to_str().unwrap().to_owned());
        assert_eq!(caller_cwd(Some(OsStr::new(".")), &current), expected);
        assert_eq!(
            caller_cwd(Some(scratch.0.join("missing").as_os_str()), &current),
            expected
        );
        assert_eq!(caller_cwd(None, &current), expected);
    }

    #[test]
    fn a_current_directory_that_is_not_utf8_has_no_cwd() {
        use std::os::unix::ffi::OsStrExt;
        let odd = Path::new(OsStr::from_bytes(b"/tmp/\xff"));
        assert_eq!(caller_cwd(None, odd), None);
    }

    #[test]
    fn skipping_to_a_newline_stops_after_it() {
        let mut input = &b"abc\ndef\n"[..];
        skip_to_newline(&mut input);
        assert_eq!(input, b"def\n");
        let mut input = &b"abc"[..];
        skip_to_newline(&mut input);
        assert!(input.is_empty());
    }
}
