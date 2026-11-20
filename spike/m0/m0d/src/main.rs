//! m0d: M0 spike relay daemon. Disposable code; see ../FINDINGS.md.
//!
//! m0d serve   --listen <unix sock> --core <herdr-core sock> --data <dir>
//!             [--log-cap BYTES] [--credit ack|eager] [--grace-ms MS]
//! m0d connect --listen <unix sock>        stdio <-> daemon socket bridge (what ssh runs)
//!
//! Client protocol (same over the local socket and over the bridge):
//!   frame = u32 BE len | u8 type | payload      (len = 1 + payload length, at most 1 MiB)
//!   type 1 JSON control, both directions
//!   type 2 EVENT DATA, daemon -> client: u32 BE att | u32 BE epoch | u64 BE seq | bytes
//!   type 3 INPUT, client -> daemon: u32 BE att | bytes
//! Every attachment event (data, resized, attached, closed, lost, core_lost) carries one
//! cursor (att, epoch, seq) in a single ordered log. Non-data events are JSON
//! {"op":"ev","att","epoch","seq","kind",...}.
//!
//! No network listeners: Unix socket (0600) only.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, Notify};

const DEFAULT_CORE_ID: &str = "4bcff4a58f4d97d505396e5a43a807e6";
const PROTO_VERSION: u64 = 1;
const MAX_FRAME: usize = 1 << 20;
/// herdr-core's initial per-attach credit allowance, observed in M0 (bytes, includes the paint).
const HC_WINDOW: i64 = 262_144;

const T_JSON: u8 = 1;
const T_DATA: u8 = 2;
const T_INPUT: u8 = 3;

// herdr-core frame kinds (M0 observation)
const K_DATA: u8 = 0;
const K_CREDIT: u8 = 1;
const K_CLOSE: u8 = 2;
const K_RESIZE: u8 = 3;
const K_FOCUS: u8 = 4;
const K_FACTS: u8 = 5;

type Io<T> = std::io::Result<T>;
fn err(s: impl Into<String>) -> std::io::Error {
    std::io::Error::other(s.into())
}

// ---------------------------------------------------------------- herdr-core client

struct HcReader {
    rd: OwnedReadHalf,
    buf: Vec<u8>,
}

impl HcReader {
    async fn fill(&mut self) -> Io<bool> {
        let mut tmp = vec![0u8; 65536];
        let k = self.rd.read(&mut tmp).await?;
        self.buf.extend_from_slice(&tmp[..k]);
        Ok(k > 0)
    }
    async fn frame(&mut self) -> Io<Option<(u32, u8, Vec<u8>)>> {
        loop {
            if self.buf.len() >= 9 {
                let n = u32::from_le_bytes(self.buf[5..9].try_into().unwrap()) as usize;
                if self.buf.len() >= 9 + n {
                    let ch = u32::from_le_bytes(self.buf[0..4].try_into().unwrap());
                    let kind = self.buf[4];
                    let p = self.buf[9..9 + n].to_vec();
                    self.buf.drain(..9 + n);
                    return Ok(Some((ch, kind, p)));
                }
            }
            if !self.fill().await? {
                return Ok(None);
            }
        }
    }
}

fn hc_frame(ch: u32, kind: u8, p: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(9 + p.len());
    v.extend_from_slice(&ch.to_le_bytes());
    v.push(kind);
    v.extend_from_slice(&(p.len() as u32).to_le_bytes());
    v.extend_from_slice(p);
    v
}

async fn hc_connect(path: &str) -> Io<(HcReader, OwnedWriteHalf, Value)> {
    let s = UnixStream::connect(path).await?;
    let (rd, mut wr) = s.into_split();
    wr.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"hello\",\"params\":{\"framed\":true,\"generation\":1}}\n")
        .await?;
    let mut r = HcReader { rd, buf: vec![] };
    loop {
        if let Some(i) = r.buf.iter().position(|&b| b == b'\n') {
            let line = r.buf[..i].to_vec();
            r.buf.drain(..=i);
            let v: Value = serde_json::from_slice(&line).map_err(|e| err(e.to_string()))?;
            let res = v["result"].clone();
            if res["core"]["id"] == DEFAULT_CORE_ID {
                return Err(err("refusing to talk to the user's default herdr-core"));
            }
            if res["framed"] != true {
                return Err(err(format!("hello not framed: {v}")));
            }
            return Ok((r, wr, res));
        }
        if !r.fill().await? {
            return Err(err("core closed during hello"));
        }
    }
}

/// One herdr-core viewer: its own connection and channel.
struct Viewer {
    rd: HcReader,
    ch: u32,
    info: Value,
    pending: Vec<(u32, u8, Vec<u8>)>,
    tx: mpsc::UnboundedSender<(u8, Vec<u8>)>,
}

async fn open_viewer(core: &str, params: Value) -> Io<Viewer> {
    let (mut rd, mut wr, _hello) = hc_connect(core).await?;
    let req = json!({"jsonrpc":"2.0","id":1,"method":"terminal.attach","params":params});
    wr.write_all(&hc_frame(0, K_DATA, req.to_string().as_bytes())).await?;
    let mut pending = vec![];
    let info = loop {
        let f = rd.frame().await?.ok_or_else(|| err("core closed during attach"))?;
        if f.0 == 0 {
            let v: Value = serde_json::from_slice(&f.2).map_err(|e| err(e.to_string()))?;
            if v["id"] == 1 {
                if !v["error"].is_null() {
                    return Err(err(format!("attach failed: {}", v["error"])));
                }
                break v["result"].clone();
            }
        } else {
            pending.push(f);
        }
    };
    let ch = info["channel"].as_u64().ok_or_else(|| err("no channel"))? as u32;
    let (tx, mut rx) = mpsc::unbounded_channel::<(u8, Vec<u8>)>();
    tokio::spawn(async move {
        while let Some((k, p)) = rx.recv().await {
            if wr.write_all(&hc_frame(ch, k, &p)).await.is_err() {
                break;
            }
            if k == K_CLOSE {
                break;
            }
        }
    });
    Ok(Viewer { rd, ch, info, pending, tx })
}

// ---------------------------------------------------------------- attachments

#[derive(Clone)]
enum Body {
    Data(Arc<Vec<u8>>),
    Json(Arc<Value>),
}

struct Ev {
    seq: u64,
    epoch: u32,
    body: Body,
    bytes: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Credit {
    Ack,
    Eager,
}

struct Cfg {
    core: String,
    log_cap: usize,
    credit: Credit,
    grace: Duration,
}

struct Inner {
    epoch: u32,
    log: VecDeque<Ev>,
    next_seq: u64,
    trimmed_upto: u64, // every seq below this is gone
    log_bytes: usize,
    outstanding: i64, // credit herdr-core may still spend on the stream viewer
    stream_tx: Option<mpsc::UnboundedSender<(u8, Vec<u8>)>>,
    input_tx: Option<mpsc::UnboundedSender<(u8, Vec<u8>)>>,
    bind_gen: u64,
    bound_conn: Option<u64>,
    closed: bool,
    overflows: u64,
    data_bytes_in: u64,
    cols: u16,
    rows: u16,
}

struct Att {
    id: u32,
    terminal: String,
    params: Value, // attach options from the client
    paired: bool,
    inner: Mutex<Inner>,
    notify: Notify,
}

impl Att {
    fn push(&self, g: &mut Inner, body: Body, cap: usize, credit: Credit) {
        let bytes = match &body {
            Body::Data(d) => d.len(),
            Body::Json(_) => 64,
        };
        g.log.push_back(Ev { seq: g.next_seq, epoch: g.epoch, body, bytes });
        g.next_seq += 1;
        g.log_bytes += bytes;
        if credit == Credit::Eager && g.log_bytes > cap {
            // The client fell past its queue: drop everything; the client gets Gap.
            g.log.clear();
            g.log_bytes = 0;
            g.trimmed_upto = g.next_seq;
            g.overflows += 1;
        }
    }
    fn push_json(&self, d: &Daemon, v: Value) {
        let mut g = self.inner.lock().unwrap();
        self.push(&mut g, Body::Json(Arc::new(v)), d.cfg.log_cap, d.cfg.credit);
        drop(g);
        self.notify.notify_waiters();
    }
    /// Ack mode: how much CREDIT to grant now so that log + in-flight stays within the cap.
    fn credit_due(g: &mut Inner, cap: usize) -> u32 {
        let room = cap as i64 - g.log_bytes as i64;
        let target = room.clamp(0, HC_WINDOW);
        let give = target - g.outstanding;
        if give >= 16384 || (give > 0 && g.outstanding <= 0) {
            g.outstanding += give;
            give as u32
        } else {
            0
        }
    }
}

struct Daemon {
    cfg: Cfg,
    host_id: String,
    atts: Mutex<HashMap<u32, Arc<Att>>>,
    next_att: AtomicU32,
    next_conn: AtomicU64,
}

fn viewer_params(att: &Att, observe: bool, rn: bool, resumed: bool) -> Value {
    let p = &att.params;
    json!({
        "terminal": att.terminal,
        "cols": p["cols"].as_u64().unwrap_or(80),
        "rows": p["rows"].as_u64().unwrap_or(24),
        "display": p.get("display").cloned().unwrap_or(json!({})),
        "history": p["history"].as_u64().unwrap_or(0),
        "observe": observe,
        "resize_notifications": rn,
        "resumed": resumed,
    })
}

/// Drives one attachment's herdr-core viewer(s) across epochs.
async fn run_att(d: Arc<Daemon>, att: Arc<Att>, first: oneshot::Sender<Result<Value, String>>) {
    let mut first = Some(first);
    let mut lost_since: Option<tokio::time::Instant> = None;
    loop {
        if att.inner.lock().unwrap().closed {
            return;
        }
        let resumed = lost_since.is_some();
        let p = &att.params;
        let opened = if att.paired {
            match open_viewer(&d.cfg.core, viewer_params(&att, true, true, resumed)).await {
                Ok(s) => match open_viewer(&d.cfg.core, viewer_params(&att, false, false, resumed)).await {
                    Ok(i) => Ok((s, Some(i))),
                    Err(e) => Err(e),
                },
                Err(e) => Err(e),
            }
        } else {
            let observe = p["observe"].as_bool().unwrap_or(false);
            let rn = p["resize_notifications"].as_bool().unwrap_or(true);
            open_viewer(&d.cfg.core, viewer_params(&att, observe, rn, resumed)).await.map(|v| (v, None))
        };
        let (mut sv, iv) = match opened {
            Ok(x) => x,
            Err(e) => {
                if let Some(f) = first.take() {
                    let _ = f.send(Err(e.to_string()));
                    return;
                }
                if lost_since.unwrap().elapsed() > Duration::from_secs(30) {
                    att.push_json(&d, json!({"kind":"lost","error":e.to_string()}));
                    return;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
        };
        lost_since = None;
        let t = &sv.info["terminal"];
        let info = json!({
            "terminal": t["address"], "cols": t["cols"], "rows": t["rows"], "screen": t["screen"],
            "resize_notifications": sv.info["resize_notifications"],
            "automatic_input": sv.info["automatic_input"],
            "input_viewer_resize_notifications": iv.as_ref().map(|v| v.info["resize_notifications"].clone()),
            "paired": att.paired, "resumed_viewer": resumed,
        });
        {
            let mut g = att.inner.lock().unwrap();
            g.epoch += 1;
            g.outstanding = HC_WINDOW;
            g.cols = t["cols"].as_u64().unwrap_or(0) as u16;
            g.rows = t["rows"].as_u64().unwrap_or(0) as u16;
            g.stream_tx = Some(sv.tx.clone());
            g.input_tx = Some(iv.as_ref().map(|v| v.tx.clone()).unwrap_or_else(|| sv.tx.clone()));
            let mut ev = info.clone();
            ev["kind"] = json!("attached");
            ev["epoch"] = json!(g.epoch);
            att.push(&mut g, Body::Json(Arc::new(ev)), d.cfg.log_cap, d.cfg.credit);
        }
        att.notify.notify_waiters();
        if let Some(f) = first.take() {
            let _ = f.send(Ok(info));
        }
        // Input viewer of a pair: credit everything it sends and discard it.
        if let Some(mut iv) = iv {
            let ich = iv.ch;
            tokio::spawn(async move {
                while let Ok(Some((ch, k, p))) = iv.rd.frame().await {
                    if ch == ich && k == K_DATA {
                        let _ = iv.tx.send((K_CREDIT, (p.len() as u32).to_le_bytes().to_vec()));
                    }
                }
            });
        }
        let mut pending = std::mem::take(&mut sv.pending).into_iter();
        let ended_by_close = loop {
            let f = match pending.next() {
                Some(f) => Some(f),
                None => match sv.rd.frame().await {
                    Ok(f) => f,
                    Err(_) => None,
                },
            };
            let Some((ch, kind, p)) = f else { break false };
            if ch != sv.ch {
                continue;
            }
            let mut g = att.inner.lock().unwrap();
            if g.closed {
                return;
            }
            match kind {
                K_DATA => {
                    let n = p.len();
                    g.data_bytes_in += n as u64;
                    g.outstanding -= n as i64;
                    att.push(&mut g, Body::Data(Arc::new(p)), d.cfg.log_cap, d.cfg.credit);
                    let give = match d.cfg.credit {
                        Credit::Eager => {
                            g.outstanding += n as i64;
                            n as u32
                        }
                        Credit::Ack => Att::credit_due(&mut g, d.cfg.log_cap),
                    };
                    if give > 0 {
                        if let Some(tx) = &g.stream_tx {
                            let _ = tx.send((K_CREDIT, give.to_le_bytes().to_vec()));
                        }
                    }
                }
                K_RESIZE if p.len() == 4 => {
                    let cols = u16::from_le_bytes([p[0], p[1]]);
                    let rows = u16::from_le_bytes([p[2], p[3]]);
                    g.cols = cols;
                    g.rows = rows;
                    let ev = json!({"kind":"resized","cols":cols,"rows":rows,"source":"core"});
                    att.push(&mut g, Body::Json(Arc::new(ev)), d.cfg.log_cap, d.cfg.credit);
                }
                K_CLOSE => {
                    let detail: Value = serde_json::from_slice(&p).unwrap_or(Value::Null);
                    att.push(&mut g, Body::Json(Arc::new(json!({"kind":"closed","detail":detail}))), d.cfg.log_cap, d.cfg.credit);
                    drop(g);
                    att.notify.notify_waiters();
                    break true;
                }
                other => {
                    let ev = json!({"kind":"core_frame","frame_kind":other,"len":p.len()});
                    att.push(&mut g, Body::Json(Arc::new(ev)), d.cfg.log_cap, d.cfg.credit);
                }
            }
            drop(g);
            att.notify.notify_waiters();
        };
        if ended_by_close {
            return;
        }
        // Stream ended without CLOSE: core restart or crash. Reattach (resumed) in a new epoch.
        att.push_json(&d, json!({"kind":"core_lost"}));
        lost_since = Some(tokio::time::Instant::now());
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn detach(d: &Daemon, att: &Att) {
    let mut g = att.inner.lock().unwrap();
    g.closed = true;
    for tx in [g.stream_tx.take(), g.input_tx.take()].into_iter().flatten() {
        let _ = tx.send((K_CLOSE, vec![]));
    }
    drop(g);
    att.notify.notify_waiters();
    d.atts.lock().unwrap().remove(&att.id);
}

// ---------------------------------------------------------------- client side

fn cframe(t: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(5 + payload.len());
    v.extend_from_slice(&((payload.len() + 1) as u32).to_be_bytes());
    v.push(t);
    v.extend_from_slice(payload);
    v
}

fn jframe(v: &Value) -> Vec<u8> {
    cframe(T_JSON, v.to_string().as_bytes())
}

async fn read_cframe(rd: &mut OwnedReadHalf) -> Io<Option<(u8, Vec<u8>)>> {
    let mut h = [0u8; 4];
    match rd.read_exact(&mut h).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_be_bytes(h) as usize;
    if n == 0 || n > MAX_FRAME {
        return Err(err(format!("bad frame length {n}")));
    }
    let mut b = vec![0u8; n];
    rd.read_exact(&mut b).await?;
    let t = b[0];
    b.remove(0);
    Ok(Some((t, b)))
}

/// Streams one attachment's log to one client connection, from `next`, in order.
async fn sender(att: Arc<Att>, gen: u64, mut next: u64, data_tx: mpsc::Sender<Vec<u8>>) {
    loop {
        let notified = att.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let frame = {
            let g = att.inner.lock().unwrap();
            if g.bind_gen != gen || g.closed && next >= g.next_seq {
                return;
            }
            if next < g.trimmed_upto {
                Some(jframe(&json!({"op":"gap","att":att.id,"reason":"overflow","from_seq":next})))
            } else {
                let front = g.log.front().map(|e| e.seq).unwrap_or(g.next_seq);
                g.log.get((next - front) as usize).map(|e| match &e.body {
                    Body::Data(b) => {
                        let mut p = Vec::with_capacity(16 + b.len());
                        p.extend_from_slice(&att.id.to_be_bytes());
                        p.extend_from_slice(&e.epoch.to_be_bytes());
                        p.extend_from_slice(&e.seq.to_be_bytes());
                        p.extend_from_slice(b);
                        cframe(T_DATA, &p)
                    }
                    Body::Json(v) => {
                        let mut v = (**v).clone();
                        v["op"] = json!("ev");
                        v["att"] = json!(att.id);
                        v["seq"] = json!(e.seq);
                        if v.get("epoch").is_none() {
                            v["epoch"] = json!(e.epoch);
                        }
                        jframe(&v)
                    }
                })
            }
        };
        match frame {
            Some(f) => {
                let is_gap = f.len() > 5 && f[4] == T_JSON && f.windows(9).any(|w| w == b"\"op\":\"gap");
                if data_tx.send(f).await.is_err() {
                    return;
                }
                if is_gap {
                    return;
                }
                next += 1;
            }
            None => notified.await,
        }
    }
}

fn bind(att: &Arc<Att>, conn: u64, from: u64, data_tx: mpsc::Sender<Vec<u8>>) {
    let gen = {
        let mut g = att.inner.lock().unwrap();
        g.bind_gen += 1;
        g.bound_conn = Some(conn);
        g.bind_gen
    };
    att.notify.notify_waiters(); // stop any older sender
    tokio::spawn(sender(att.clone(), gen, from, data_tx));
}

async fn client(d: Arc<Daemon>, s: UnixStream) {
    let conn = d.next_conn.fetch_add(1, Ordering::Relaxed);
    let (mut rd, mut wr) = s.into_split();
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (data_tx, mut data_rx) = mpsc::channel::<Vec<u8>>(64);
    tokio::spawn(async move {
        loop {
            // Control frames always go first; data from several attachments interleaves
            // in arrival order (each attachment's sender pushes one frame at a time).
            let f = tokio::select! {
                biased;
                Some(f) = ctrl_rx.recv() => f,
                Some(f) = data_rx.recv() => f,
                else => break,
            };
            if wr.write_all(&f).await.is_err() {
                break;
            }
        }
    });
    // Handshake.
    match read_cframe(&mut rd).await {
        Ok(Some((T_JSON, b))) => {
            let v: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
            if v["op"] != "hello" || v["version"] != PROTO_VERSION {
                let _ = ctrl_tx.send(jframe(&json!({"op":"error","error":format!(
                    "protocol version mismatch: client {} daemon {}", v["version"], PROTO_VERSION)})));
                tokio::time::sleep(Duration::from_millis(100)).await;
                return;
            }
            let _ = ctrl_tx.send(jframe(&json!({"op":"hello","version":PROTO_VERSION,"host_id":d.host_id,"pid":std::process::id()})));
        }
        _ => return,
    }
    let mut mine: Vec<u32> = vec![];
    loop {
        let (t, b) = match read_cframe(&mut rd).await {
            Ok(Some(f)) => f,
            _ => break,
        };
        if t == T_INPUT && b.len() >= 4 {
            let id = u32::from_be_bytes(b[0..4].try_into().unwrap());
            if let Some(att) = d.atts.lock().unwrap().get(&id).cloned() {
                if let Some(tx) = &att.inner.lock().unwrap().input_tx {
                    let _ = tx.send((K_DATA, b[4..].to_vec()));
                }
            }
            continue;
        }
        if t != T_JSON {
            continue;
        }
        let v: Value = match serde_json::from_slice(&b) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = v["att"].as_u64().unwrap_or(0) as u32;
        let att = d.atts.lock().unwrap().get(&id).cloned();
        match v["op"].as_str().unwrap_or("") {
            "attach" => {
                let req = v["req"].clone();
                if let Some(r) = v.get("resume").filter(|r| !r.is_null()) {
                    let rid = r["att"].as_u64().unwrap_or(0) as u32;
                    let seq = r["seq"].as_u64().unwrap_or(0);
                    let ratt = d.atts.lock().unwrap().get(&rid).cloned();
                    let ok = ratt.as_ref().map(|a| {
                        let g = a.inner.lock().unwrap();
                        !g.closed && seq + 1 >= g.trimmed_upto && seq < g.next_seq
                    });
                    match (ratt, ok) {
                        (Some(a), Some(true)) => {
                            let epoch = a.inner.lock().unwrap().epoch;
                            let _ = ctrl_tx.send(jframe(&json!({"op":"attached","req":req,"att":rid,"resumed":true,"epoch":epoch})));
                            bind(&a, conn, seq + 1, data_tx.clone());
                            mine.push(rid);
                        }
                        (a, _) => {
                            let reason = if a.is_none() { "unknown_attachment" } else { "outside_replay_buffer" };
                            let _ = ctrl_tx.send(jframe(&json!({"op":"gap","req":req,"att":rid,"reason":reason})));
                        }
                    }
                    continue;
                }
                let term = v["terminal"].as_str().unwrap_or("").to_string();
                if term.contains('/') {
                    let _ = ctrl_tx.send(jframe(&json!({"op":"error","req":req,"error":"machine-qualified terminals are not this host's"})));
                    continue;
                }
                let nid = d.next_att.fetch_add(1, Ordering::Relaxed);
                let att = Arc::new(Att {
                    id: nid,
                    terminal: term,
                    paired: v["mode"] == "paired",
                    params: v.clone(),
                    notify: Notify::new(),
                    inner: Mutex::new(Inner {
                        epoch: 0, log: VecDeque::new(), next_seq: 0, trimmed_upto: 0, log_bytes: 0,
                        outstanding: HC_WINDOW, stream_tx: None, input_tx: None, bind_gen: 0,
                        bound_conn: None, closed: false, overflows: 0, data_bytes_in: 0, cols: 0, rows: 0,
                    }),
                });
                d.atts.lock().unwrap().insert(nid, att.clone());
                mine.push(nid);
                let (ftx, frx) = oneshot::channel();
                tokio::spawn(run_att(d.clone(), att.clone(), ftx));
                let (d2, ctrl2, data2) = (d.clone(), ctrl_tx.clone(), data_tx.clone());
                tokio::spawn(async move {
                    match frx.await {
                        Ok(Ok(info)) => {
                            let _ = ctrl2.send(jframe(&json!({"op":"attached","req":req,"att":nid,"resumed":false,"info":info})));
                            bind(&att, conn, 0, data2);
                        }
                        r => {
                            let e = match r { Ok(Err(e)) => e, _ => "attach task ended".into() };
                            let _ = ctrl2.send(jframe(&json!({"op":"error","req":req,"att":nid,"error":e})));
                            d2.atts.lock().unwrap().remove(&nid);
                        }
                    }
                });
            }
            "ack" => {
                if let Some(att) = att {
                    let seq = v["seq"].as_u64().unwrap_or(0);
                    let mut g = att.inner.lock().unwrap();
                    while g.log.front().is_some_and(|e| e.seq <= seq) {
                        let e = g.log.pop_front().unwrap();
                        g.log_bytes -= e.bytes;
                    }
                    g.trimmed_upto = g.trimmed_upto.max(seq + 1);
                    if d.cfg.credit == Credit::Ack {
                        let give = Att::credit_due(&mut g, d.cfg.log_cap);
                        if give > 0 {
                            if let Some(tx) = &g.stream_tx {
                                let _ = tx.send((K_CREDIT, give.to_le_bytes().to_vec()));
                            }
                        }
                    }
                }
            }
            "resize" => {
                if let Some(att) = att {
                    let (c, r) = (v["cols"].as_u64().unwrap_or(80) as u16, v["rows"].as_u64().unwrap_or(24) as u16);
                    let mut g = att.inner.lock().unwrap();
                    if !att.paired {
                        // Active viewers get no core RESIZE: log the resize where we send it.
                        g.cols = c;
                        g.rows = r;
                        let ev = json!({"kind":"resized","cols":c,"rows":r,"source":"client"});
                        att.push(&mut g, Body::Json(Arc::new(ev)), d.cfg.log_cap, d.cfg.credit);
                    }
                    let mut p = c.to_le_bytes().to_vec();
                    p.extend_from_slice(&r.to_le_bytes());
                    if let Some(tx) = &g.input_tx {
                        let _ = tx.send((K_RESIZE, p));
                    }
                    drop(g);
                    att.notify.notify_waiters();
                }
            }
            "focus" => {
                if let Some(att) = att {
                    if let Some(tx) = &att.inner.lock().unwrap().input_tx {
                        let _ = tx.send((K_FOCUS, vec![v["focused"].as_bool().unwrap_or(true) as u8]));
                    }
                }
            }
            "facts" => {
                if let Some(att) = att {
                    let g = att.inner.lock().unwrap();
                    let p = v["display"].to_string().into_bytes();
                    for tx in [&g.stream_tx, &g.input_tx].into_iter().flatten() {
                        let _ = tx.send((K_FACTS, p.clone()));
                    }
                }
            }
            "detach" => {
                if let Some(att) = att {
                    detach(&d, &att);
                }
            }
            "stats" => {
                let atts: Vec<Value> = d.atts.lock().unwrap().values().map(|a| {
                    let g = a.inner.lock().unwrap();
                    json!({"att":a.id,"terminal":a.terminal,"epoch":g.epoch,"log_events":g.log.len(),
                           "log_bytes":g.log_bytes,"next_seq":g.next_seq,"trimmed_upto":g.trimmed_upto,
                           "outstanding_credit":g.outstanding,"overflows":g.overflows,
                           "data_bytes_in":g.data_bytes_in,"bound":g.bound_conn.is_some(),
                           "cols":g.cols,"rows":g.rows})
                }).collect();
                let _ = ctrl_tx.send(jframe(&json!({"op":"stats","req":v["req"],"pid":std::process::id(),"atts":atts})));
            }
            _ => {
                let _ = ctrl_tx.send(jframe(&json!({"op":"error","error":"unknown op","got":v})));
            }
        }
    }
    // Connection dropped: unbind; detach after the grace period unless resumed.
    for id in mine {
        let Some(att) = d.atts.lock().unwrap().get(&id).cloned() else { continue };
        let gen = {
            let mut g = att.inner.lock().unwrap();
            if g.bound_conn != Some(conn) {
                continue;
            }
            g.bound_conn = None;
            g.bind_gen += 1;
            g.bind_gen
        };
        att.notify.notify_waiters();
        let (d2, grace) = (d.clone(), d.cfg.grace);
        tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            let expired = { let g = att.inner.lock().unwrap(); g.bind_gen == gen && g.bound_conn.is_none() };
            if expired {
                detach(&d2, &att);
            }
        });
    }
}

// ---------------------------------------------------------------- main

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn refuse_default(path: &str) {
    let home = std::env::var("HOME").unwrap_or_default();
    let canon = std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or(path.into());
    if path.contains("/herdr-core/default/") || canon.contains("/herdr-core/default/")
        || canon == format!("{home}/.local/state/herdr-core/default/default.sock")
    {
        eprintln!("m0d: refusing the default herdr-core socket {path}");
        std::process::exit(2);
    }
}

fn host_id(dir: &str) -> String {
    let p = format!("{dir}/host.json");
    if let Ok(s) = std::fs::read_to_string(&p) {
        if let Some(id) = serde_json::from_str::<Value>(&s).ok().and_then(|v| v["host_id"].as_str().map(String::from)) {
            return id;
        }
    }
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).expect("urandom");
    let id: String = b.iter().map(|x| format!("{x:02x}")).collect();
    std::fs::create_dir_all(dir).expect("data dir");
    std::fs::write(&p, json!({"host_id": id}).to_string()).expect("host.json");
    id
}

fn connect_bridge(listen: &str) -> ! {
    let s = std::os::unix::net::UnixStream::connect(listen).unwrap_or_else(|e| {
        eprintln!("m0d connect: cannot reach the daemon at {listen}: {e}");
        std::process::exit(1)
    });
    let mut s2 = s.try_clone().unwrap();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let _ = std::io::copy(&mut stdin, &mut s2);
        let _ = s2.shutdown(std::net::Shutdown::Write);
    });
    let mut s = s;
    let mut out = std::io::stdout().lock();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if out.write_all(&buf[..n]).and_then(|_| out.flush()).is_err() {
                    break;
                }
            }
        }
    }
    std::process::exit(0)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let listen = arg(&args, "--listen").unwrap_or_else(|| {
        eprintln!("usage: m0d serve|connect --listen <sock> [--core <sock> --data <dir>]");
        std::process::exit(2)
    });
    refuse_default(&listen);
    match args.get(1).map(String::as_str) {
        Some("connect") => connect_bridge(&listen),
        Some("serve") => {}
        _ => {
            eprintln!("usage: m0d serve|connect ...");
            std::process::exit(2)
        }
    }
    let core = arg(&args, "--core").unwrap_or_else(|| {
        eprintln!("m0d serve: --core <herdr-core socket> is required (no default, by design)");
        std::process::exit(2)
    });
    refuse_default(&core);
    let data = arg(&args, "--data").unwrap_or_else(|| "/tmp/cm0/m0d".into());
    let cfg = Cfg {
        core,
        log_cap: arg(&args, "--log-cap").and_then(|s| s.parse().ok()).unwrap_or(4 << 20),
        credit: if arg(&args, "--credit").as_deref() == Some("eager") { Credit::Eager } else { Credit::Ack },
        grace: Duration::from_millis(arg(&args, "--grace-ms").and_then(|s| s.parse().ok()).unwrap_or(30_000)),
    };
    // Check the core once up front (also refuses the default core by id).
    if let Err(e) = hc_connect(&cfg.core).await {
        eprintln!("m0d: cannot use core {}: {e}", cfg.core);
        std::process::exit(1);
    }
    let start = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as u32;
    let d = Arc::new(Daemon {
        host_id: host_id(&data),
        cfg,
        atts: Mutex::new(HashMap::new()),
        next_att: AtomicU32::new((start & 0xffff) << 16 | 1),
        next_conn: AtomicU64::new(1),
    });
    let _ = std::fs::remove_file(&listen);
    let l = UnixListener::bind(&listen).expect("bind");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&listen, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    eprintln!("m0d: listening on {listen} (unix only), core {}, host {}, credit {}, log cap {}",
              d.cfg.core, d.host_id, if d.cfg.credit == Credit::Ack { "ack" } else { "eager" }, d.cfg.log_cap);
    loop {
        if let Ok((s, _)) = l.accept().await {
            tokio::spawn(client(d.clone(), s));
        }
    }
}
