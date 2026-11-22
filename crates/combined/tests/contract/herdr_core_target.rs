//! The herdr-core adapter as a contract [`Target`], against the isolated core the lane started.
//!
//! Nothing here talks to a core unless `COMBINE_HC_LANE=1`, which only `tools/verify-herdr-core`
//! sets, and then only to the one socket `lane_safety` accepts. Destructive calls (restart,
//! `terminal.kill`, `terminal.rm`) follow a fresh `server.status` whose core id equals the id
//! the lane pinned. The only signal this file sends is the crash procedure's SIGKILL of the
//! lane's own core, after `check_kill_target` passed on one status reply and one `ps` look.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, Once, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use combine_protocol::TermId;
use combined::adapters::herdr_core::{
    HerdrCoreBackend, HerdrCoreConfig, HerdrCoreControl, PairHalf, PairStatus,
};
use combined::model::SpawnSpec;
use combined::ports::TerminalBackend;
use combined::testing::{Half, LATE_DATA, LATE_RESIZED};
use serde_json::{json, Value};

use crate::contract::generator::Payload;
use crate::contract::target::{Fault, Target, Unsupported};
use crate::lane_safety::{self as safety, Caller, Expect, Lane};

/// How a program's paint begins: CAN, then DECSTR (SPEC.md, "Terminal backends").
pub const PAINT_START: &[u8] = b"\x18\x1b[!p";

/// Bytes the program is asked to print per request.
const EMIT_CHUNK: usize = 64 * 1024;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static LANE_SERIAL: Mutex<()> = Mutex::new(());
static CURRENT_CASE: Mutex<String> = Mutex::new(String::new());
static DEADLINE: Once = Once::new();
static INSTANCES: AtomicUsize = AtomicUsize::new(0);

fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a tokio runtime")
    })
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Records which case is running, for the deadline's message. Cargo names a test's thread after
/// the test.
pub fn note_case() {
    let thread = std::thread::current();
    *guard(&CURRENT_CASE) = thread.name().unwrap_or("unnamed").to_string();
}

/// `Ok` when the lane is on; the reason otherwise. The first call with the lane on arms the
/// deadline: after 1,320 s (or a shorter `COMBINE_HC_DEADLINE_S`) the binary exits 124, so the
/// lane can never run unbounded and nothing outside the binary ever signals it.
pub fn lane() -> Result<(), String> {
    if std::env::var("COMBINE_HC_LANE").as_deref() != Ok("1") {
        return Err("the herdr-core lane is off (run `sh tools/verify-herdr-core`)".to_string());
    }
    DEADLINE.call_once(|| {
        let seconds =
            safety::deadline_seconds(std::env::var("COMBINE_HC_DEADLINE_S").ok().as_deref());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(seconds));
            let case = guard(&CURRENT_CASE).clone();
            println!("lane deadline: {seconds} s passed during {case}; exiting 124");
            std::process::exit(124);
        });
    });
    Ok(())
}

/// A blocking look at the lane core over its socket, with newline JSON-RPC and no framing.
#[derive(Clone)]
pub struct CoreProbe {
    socket: PathBuf,
}

impl CoreProbe {
    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let stream = UnixStream::connect(&self.socket)
            .map_err(|error| format!("connecting to the lane core: {error}"))?;
        let timeout = Some(Duration::from_secs(5));
        stream
            .set_read_timeout(timeout)
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(timeout)
            .map_err(|e| e.to_string())?;
        let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(stream);
        let mut send = |id: u64, method: &str, params: Value| -> Result<(), String> {
            let mut line =
                json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
            line.push('\n');
            writer.write_all(line.as_bytes()).map_err(|e| e.to_string())
        };
        let mut read = |id: u64| -> Result<Value, String> {
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                    return Err("the lane core closed the connection".to_string());
                }
                let value: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
                if value.get("id").and_then(Value::as_u64) == Some(id) {
                    if let Some(error) = value.get("error") {
                        return Err(format!("{method}: {error}"));
                    }
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
            }
        };
        send(0, "hello", json!({"generation": 1}))?;
        read(0)?;
        send(1, method, params)?;
        read(1)
    }

    pub fn status(&self) -> Result<Value, String> {
        self.call("server.status", json!({}))
    }

    /// All terminals, finished ones included.
    pub fn list_all(&self) -> Result<Vec<Value>, String> {
        let result = self.call("terminal.list", json!({"all": true}))?;
        Ok(result
            .get("terminals")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }
}

/// The adapter on the lane's own core.
pub struct HerdrCoreTarget {
    lane: Lane,
    caller: Caller,
    version: String,
    backend: Arc<HerdrCoreBackend>,
    control: HerdrCoreControl,
    probe: CoreProbe,
    instance: String,
    next_ctl: usize,
    ctl_dirs: Mutex<HashMap<String, PathBuf>>,
    held: HashSet<String>,
    late: bool,
    /// Last field: dropped after `Drop::drop` has cleaned up.
    _serial: MutexGuard<'static, ()>,
}

impl HerdrCoreTarget {
    /// Reads and checks the lane the script described. Any refusal panics.
    pub fn from_lane() -> HerdrCoreTarget {
        note_case();
        let vars: Vec<(String, String)> = std::env::vars().collect();
        let caller = Caller::from_process();
        let lane = Lane::from_vars(&vars, |name| std::env::var(name).ok(), &caller.home)
            .unwrap_or_else(|reason| panic!("lane refused: {reason}"));
        Self::check_filesystem(&lane).unwrap_or_else(|reason| panic!("lane refused: {reason}"));
        let serial = guard(&LANE_SERIAL);
        let probe = CoreProbe {
            socket: lane.socket.clone(),
        };
        let target = {
            let status = probe
                .status()
                .unwrap_or_else(|error| panic!("the lane core doesn't answer: {error}"));
            Self::pinned(&lane, &status).unwrap_or_else(|reason| panic!("lane refused: {reason}"));
            let config = HerdrCoreConfig::new(lane.socket.clone());
            let (backend, control) = runtime()
                .block_on(HerdrCoreBackend::connect_with_control(config))
                .unwrap_or_else(|error| panic!("connecting to the lane core: {error}"));
            let instance = format!(
                "{}-{}",
                std::process::id(),
                INSTANCES.fetch_add(1, Ordering::Relaxed)
            );
            HerdrCoreTarget {
                version: backend.version().to_string(),
                backend: Arc::new(backend),
                control,
                probe,
                instance,
                next_ctl: 0,
                ctl_dirs: Mutex::new(HashMap::new()),
                held: HashSet::new(),
                late: false,
                lane,
                caller,
                _serial: serial,
            }
        };
        target
    }

    /// The root is this lane's real directory, only its owner can enter it, and the socket is a
    /// real socket whose parent really is `<root>/run/herdr-core`.
    fn check_filesystem(lane: &Lane) -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;
        let root = Path::new(&lane.root);
        let mode = std::fs::symlink_metadata(root)
            .map(|meta| meta.permissions().mode())
            .unwrap_or(0);
        safety::check_root_kind(safety::kind_of(root), mode)?;
        safety::check_kind(
            safety::kind_of(&lane.socket),
            Expect::Socket,
            "the lane socket",
        )?;
        let canonical_root = root.canonicalize().map_err(|e| e.to_string())?;
        let parent = lane
            .socket
            .parent()
            .ok_or("the socket has no parent")?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if parent != canonical_root.join("run/herdr-core") {
            return Err(format!("the socket's directory is {}", parent.display()));
        }
        Ok(())
    }

    /// The status reply's core is the lane's own.
    fn pinned(lane: &Lane, status: &Value) -> Result<(), String> {
        let reported = status
            .pointer("/core/id")
            .and_then(Value::as_str)
            .unwrap_or("");
        safety::check_core_id(reported, &lane.core_id)
    }

    /// A fresh status whose core id passed the pin: the gate for every destructive call.
    fn checked_status(&self) -> Result<Value, String> {
        let status = self.probe.status()?;
        Self::pinned(&self.lane, &status)?;
        Ok(status)
    }

    pub fn control(&self) -> &HerdrCoreControl {
        &self.control
    }

    pub fn probe(&self) -> &CoreProbe {
        &self.probe
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    fn address_of(term: &TermId) -> Result<&str, Unsupported> {
        term.0
            .strip_prefix("hc:")
            .ok_or(Unsupported("not an hc: terminal"))
    }

    fn run_hc(&self, args: &[&str]) -> Result<std::process::Output, String> {
        Command::new(&self.lane.hc_bin)
            .args(args)
            .env_clear()
            .envs(safety::hc_env(&self.lane, &self.caller, false))
            .output()
            .map_err(|error| format!("running hc: {error}"))
    }

    fn ps(args: &[&str]) -> Result<String, String> {
        let output = Command::new("/bin/ps")
            .args(args)
            .output()
            .map_err(|error| format!("ps: {error}"))?;
        if !output.status.success() && !output.stdout.is_empty() {
            return Err("ps failed".to_string());
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Signal site S1. One status reply passes `check_core_id`; one `ps` look at its pid, with
    /// that reply's holders and our own pid, parent and group, passes `check_kill_target`;
    /// then a plain `/bin/kill -KILL -- <pid>`. Waiting for the death polls `ps`.
    fn kill_core(&self) -> Result<(), String> {
        let status = self.checked_status()?;
        let pid = status.get("pid").cloned().unwrap_or(Value::Null);
        let text = match pid.as_u64() {
            Some(number) if number > 1 => number.to_string(),
            _ => return Err(format!("refusing to signal pid {pid}")),
        };
        let observed = safety::parse_ps(&Self::ps(&["-o", "ppid=,pgid=,comm=", "-p", &text])?)
            .ok_or("ps gave no readable look at the core")?;
        let own = std::process::id();
        let group_text = Self::ps(&["-o", "pgid=", "-p", &own.to_string()])?;
        let group: u32 = group_text
            .trim()
            .parse()
            .map_err(|_| "ps gave no process group for the test".to_string())?;
        let holders: Vec<u32> = status
            .get("holders")
            .and_then(Value::as_array)
            .map(|holders| {
                holders
                    .iter()
                    .filter_map(|holder| holder.get("pid").and_then(Value::as_u64))
                    .filter_map(|pid| u32::try_from(pid).ok())
                    .collect()
            })
            .unwrap_or_default();
        let target = safety::check_kill_target(
            &pid,
            &holders,
            own,
            std::os::unix::process::parent_id(),
            group,
            &observed,
        )?;
        let killed = Command::new("/bin/kill")
            .args(["-KILL", "--", &target.to_string()])
            .output()
            .map_err(|error| format!("kill: {error}"))?;
        if !killed.status.success() {
            return Err("kill failed".to_string());
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if Self::ps(&["-o", "pid=", "-p", &target.to_string()])?
                .trim()
                .is_empty()
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err("the killed core is still running after 5 s".to_string())
    }

    /// The crash procedure: kill the core, start the next one, wait for the pairs.
    fn crash(&self) -> Result<(), String> {
        let before = self.control.pairs();
        self.kill_core()?;
        let started = self.run_hc(&["server", "start", "--json"])?;
        if !started.status.success() {
            return Err(format!(
                "hc server start failed: {}",
                String::from_utf8_lossy(&started.stderr)
            ));
        }
        self.wait_for_core()?;
        self.wait_for_pairs(&before)
    }

    /// After `server.restart` the next core adopts the running programs. With none to adopt
    /// (every terminal has finished) nothing starts it again until someone asks, so the way a
    /// client would is to start it: `hc server start`, which is a no-op for a core already up.
    fn wait_for_restarted_core(&self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.checked_status().is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let started = self.run_hc(&["server", "start", "--json"])?;
        if !started.status.success() {
            return Err(format!(
                "hc server start failed: {}",
                String::from_utf8_lossy(&started.stderr)
            ));
        }
        self.wait_for_core()
    }

    fn wait_for_core(&self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match self.checked_status() {
                Ok(_) => return Ok(()),
                Err(error) if Instant::now() >= deadline => {
                    return Err(format!("the core didn't come back: {error}"))
                }
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }

    /// Every pair that was open comes back open, one generation later.
    fn wait_for_pairs(&self, before: &[(String, PairStatus)]) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let after = self.control.pairs();
            let back = after.len() == before.len()
                && before.iter().zip(&after).all(|((_, was), (_, now))| {
                    !was.open || (now.open && now.generation > was.generation)
                });
            if back {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the pairs didn't reopen: {before:?} then {after:?}"
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The program's control directory, found by the labels it was started with.
    fn ctl_dir(&self, address: &str) -> Result<PathBuf, Unsupported> {
        if let Some(dir) = guard(&self.ctl_dirs).get(address) {
            return Ok(dir.clone());
        }
        let terminals = self
            .probe
            .list_all()
            .map_err(|_| Unsupported("the lane core didn't list its terminals"))?;
        for terminal in terminals {
            if terminal.get("address").and_then(Value::as_str) != Some(address) {
                continue;
            }
            let label = |name: &str| {
                terminal
                    .pointer(&format!("/labels/{name}"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            };
            if label("combine-contract").as_deref() != Some(&self.instance) {
                return Err(Unsupported("the terminal isn't this target's"));
            }
            let number =
                label("combine-ctl").ok_or(Unsupported("the terminal has no ctl label"))?;
            let dir = PathBuf::from(format!("{}/c/{}-{number}", self.lane.root, self.instance));
            guard(&self.ctl_dirs).insert(address.to_string(), dir.clone());
            return Ok(dir);
        }
        Err(Unsupported("the lane core has no such terminal"))
    }

    /// Has the program print `bytes`, `EMIT_CHUNK` at a time, and returns once it has written
    /// them all to its terminal.
    fn emit_blocking(&self, address: &str, bytes: &[u8]) -> Result<(), Unsupported> {
        let dir = self.ctl_dir(address)?;
        let ready = dir.join("ready");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            if Instant::now() >= deadline {
                return Err(Unsupported("the program never became ready"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Output printed after a resize request lands after the core has applied the resize,
        // as it would from a program that redraws in answer to it.
        let settled = Instant::now() + Duration::from_secs(1);
        while self.control.recently_touched() && Instant::now() < settled {
            std::thread::sleep(Duration::from_millis(5));
        }
        let delivery = self.control.expect_delivery(address, bytes.len());
        let chunks = bytes.chunks(EMIT_CHUNK).count();
        let payload = bytes.to_vec();
        let (tx, rx) = channel::<Result<(), &'static str>>();
        std::thread::spawn(move || {
            let run = || -> Result<(), &'static str> {
                let mut emit = std::fs::OpenOptions::new()
                    .write(true)
                    .open(dir.join("emit"))
                    .map_err(|_| "opening the emit FIFO")?;
                let mut ack =
                    std::fs::File::open(dir.join("ack")).map_err(|_| "opening the ack FIFO")?;
                for chunk in payload.chunks(EMIT_CHUNK) {
                    let mut request = (chunk.len() as u32).to_be_bytes().to_vec();
                    request.extend_from_slice(chunk);
                    emit.write_all(&request)
                        .map_err(|_| "writing the emit FIFO")?;
                    let mut byte = [0u8; 1];
                    ack.read_exact(&mut byte)
                        .map_err(|_| "reading the ack FIFO")?;
                    tx.send(Ok(())).map_err(|_| "the emitter went away")?;
                }
                Ok(())
            };
            if let Err(reason) = run() {
                let _ = tx.send(Err(reason));
            }
        });
        for _ in 0..chunks {
            match rx.recv_timeout(Duration::from_secs(60)) {
                Ok(Ok(())) => {}
                Ok(Err(reason)) => return Err(Unsupported(reason)),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(Unsupported("the program didn't acknowledge its output"))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Unsupported("the emitter stopped"))
                }
            }
        }
        // Returns once the output has reached the adapter, wherever the core can send it.
        let arrived = Instant::now() + Duration::from_secs(2);
        while !delivery.reached() && Instant::now() < arrived {
            std::thread::sleep(Duration::from_millis(1));
        }
        if self.held.contains(address) && !bytes.is_empty() {
            let needle = &bytes[bytes.len().saturating_sub(16)..];
            let deadline = Instant::now() + Duration::from_secs(5);
            while !self.control.held_contains(address, needle) {
                if Instant::now() >= deadline {
                    return Err(Unsupported("held output never reached the adapter"));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(())
    }

    fn sleep_blocking(duration: Duration) {
        std::thread::sleep(duration);
    }
}

impl Drop for HerdrCoreTarget {
    /// Kills and removes this instance's own terminals, after the pinned-id check, then its
    /// scratch directories. It also runs after a failed case, so nothing is left in the core.
    fn drop(&mut self) {
        if self.checked_status().is_ok() {
            for terminal in self.probe.list_all().unwrap_or_default() {
                let ours = terminal
                    .pointer("/labels/combine-contract")
                    .and_then(Value::as_str)
                    == Some(&self.instance);
                let Some(address) = terminal.get("address").and_then(Value::as_str) else {
                    continue;
                };
                if !ours || self.checked_status().is_err() {
                    continue;
                }
                let params = json!({"terminal": address});
                let _ = self.probe.call("terminal.kill", params.clone());
                let until = Instant::now() + Duration::from_secs(2);
                while self.probe.call("terminal.rm", params.clone()).is_err()
                    && Instant::now() < until
                {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        let root = PathBuf::from(&self.lane.root);
        let _ = std::fs::remove_dir_all(root.join("s").join(&self.instance));
        for number in 0..self.next_ctl {
            let _ =
                std::fs::remove_dir_all(root.join("c").join(format!("{}-{number}", self.instance)));
        }
    }
}

impl Target for HerdrCoreTarget {
    fn describe(&self) -> String {
        format!("herdr-core-{}", self.version)
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        runtime().block_on(future)
    }

    fn backend(&self) -> Arc<dyn TerminalBackend> {
        Arc::clone(&self.backend) as Arc<dyn TerminalBackend>
    }

    fn supports(&self, fault: Fault) -> bool {
        !matches!(
            fault,
            Fault::RemoteTwin | Fault::NoReplacementPaint | Fault::NoByteCredit
        )
    }

    fn scratch_dir(&mut self, name: &str) -> String {
        let dir = Path::new(&self.lane.root)
            .join("s")
            .join(&self.instance)
            .join(name);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir.canonicalize()
            .expect("a canonical scratch directory")
            .display()
            .to_string()
    }

    fn spawn_spec(&mut self, cwd: &str, cols: u16, rows: u16) -> SpawnSpec {
        let number = self.next_ctl;
        self.next_ctl += 1;
        let ctl = Path::new(&self.lane.root)
            .join("c")
            .join(format!("{}-{number}", self.instance));
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&ctl)
            .expect("a control directory");
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/contract/hc_echo.py");
        SpawnSpec {
            argv: vec![
                self.lane.python.display().to_string(),
                "-I".to_string(),
                script.to_string(),
            ],
            cwd: cwd.to_string(),
            env: [
                ("COMBINE_HC_CTL".to_string(), ctl.display().to_string()),
                ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ]
            .into_iter()
            .collect(),
            labels: [
                ("combine-contract".to_string(), self.instance.clone()),
                ("combine-ctl".to_string(), number.to_string()),
            ]
            .into_iter()
            .collect(),
            cols,
            rows,
        }
    }

    fn paint_start(&self) -> Option<&'static [u8]> {
        Some(PAINT_START)
    }

    async fn emit(&mut self, term: &TermId, bytes: &[u8]) -> Result<(), Unsupported> {
        let address = Self::address_of(term)?.to_string();
        self.emit_blocking(&address, bytes)
    }

    async fn idle(&mut self) -> bool {
        tokio::time::sleep(Duration::from_millis(5)).await;
        self.control.busy()
    }

    fn quiet_rounds(&self) -> usize {
        4
    }

    fn max_rounds(&self) -> usize {
        24_000
    }

    fn typed_input(&self, term: &TermId) -> Result<Vec<u8>, Unsupported> {
        let dir = self.ctl_dir(Self::address_of(term)?)?;
        Ok(std::fs::read(dir.join("typed")).unwrap_or_default())
    }

    /// What the adapter says it sent as automatic replies, once the program has also received
    /// each of them, in order, among the bytes it was typed.
    fn automatic_input(&self, term: &TermId) -> Result<Vec<Vec<u8>>, Unsupported> {
        let address = Self::address_of(term)?;
        let sent = self.control.automatic_input(address);
        let dir = self.ctl_dir(address)?;
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let typed = std::fs::read(dir.join("typed")).unwrap_or_default();
            if appear_in_order(&typed, &sent) {
                return Ok(sent);
            }
            if Instant::now() >= deadline {
                return Err(Unsupported(
                    "the program never received the automatic replies the adapter sent",
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Fails when a late-event fault was switched on and the adapter has delivered none: the
    /// cases that ask for old-generation events judge their client only if they were sent.
    fn pty_size(&self, term: &TermId) -> Result<(u16, u16), Unsupported> {
        if self.late && self.control.late_events_delivered() < 2 {
            return Err(Unsupported(
                "the adapter delivered no old-generation events after the rebuild",
            ));
        }
        let address = Self::address_of(term)?;
        let terminals = self
            .probe
            .list_all()
            .map_err(|_| Unsupported("the lane core didn't list its terminals"))?;
        terminals
            .iter()
            .find(|terminal| terminal.get("address").and_then(Value::as_str) == Some(address))
            .and_then(|terminal| {
                let dimension = |name: &str| {
                    terminal
                        .get(name)
                        .and_then(Value::as_u64)
                        .and_then(|value| u16::try_from(value).ok())
                };
                Some((dimension("cols")?, dimension("rows")?))
            })
            .ok_or(Unsupported("the lane core has no such terminal"))
    }

    fn open_halves(&self, term: &TermId) -> Result<usize, Unsupported> {
        Ok(self.control.open_halves(Self::address_of(term)?))
    }

    fn set_inexact(&mut self, term: &TermId) -> Result<(), Unsupported> {
        let address = Self::address_of(term)?.to_string();
        self.crash().map_err(|reason| {
            eprintln!("crash procedure failed: {reason}");
            Unsupported("the crash procedure failed")
        })?;
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            let inexact = self.probe.list_all().is_ok_and(|terminals| {
                terminals.iter().any(|terminal| {
                    terminal.get("address").and_then(Value::as_str) == Some(address.as_str())
                        && terminal.get("screen").and_then(Value::as_str) == Some("inexact")
                })
            });
            if inexact {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(Unsupported("the terminal never reported an inexact screen"))
    }

    fn remote_twin(&mut self, _local: &TermId) -> Result<TermId, Unsupported> {
        Err(Unsupported(
            "a remote twin needs a second core the lane can't check first",
        ))
    }

    fn delay_output(&mut self, term: &TermId) -> Result<(), Unsupported> {
        let address = Self::address_of(term)?.to_string();
        self.control.hold_output(&address, true);
        self.held.insert(address);
        Ok(())
    }

    fn release_output(&mut self, term: &TermId) -> Result<(), Unsupported> {
        let address = Self::address_of(term)?.to_string();
        self.held.remove(&address);
        self.control.hold_output(&address, false);
        Ok(())
    }

    /// A real overflow: 10 MiB the client isn't reading makes the core drop its queue and
    /// repaint in band.
    fn force_resync(&mut self, term: &TermId, index: usize) -> Result<(), Unsupported> {
        let address = Self::address_of(term)?.to_string();
        if index >= self.control.attachments(&address) {
            return Err(Unsupported("no attachment at that index"));
        }
        let payload = Payload::new(0xF111, 10 * 1024 * 1024);
        self.emit_blocking(&address, payload.bytes())?;
        Self::sleep_blocking(Duration::from_millis(200));
        Ok(())
    }

    fn fail_half(&mut self, term: &TermId, index: usize, half: Half) -> Result<(), Unsupported> {
        let half = match half {
            Half::Display => PairHalf::Display,
            Half::Input => PairHalf::Input,
        };
        if self.control.fail_half(Self::address_of(term)?, index, half) {
            Ok(())
        } else {
            Err(Unsupported("no open attachment at that index"))
        }
    }

    fn fail_next_opens(&mut self, term: &TermId, count: u32) -> Result<(), Unsupported> {
        self.control.fail_next_opens(Self::address_of(term)?, count);
        Ok(())
    }

    fn max_reopen_attempts(&self) -> Option<u32> {
        Some(40)
    }

    async fn restart_backend(&mut self) -> Result<(), Unsupported> {
        let before = self.control.pairs();
        self.checked_status()
            .map_err(|_| Unsupported("the lane core's id didn't match the pin"))?;
        let started = Instant::now();
        self.probe
            .call("server.restart", json!({}))
            .map_err(|_| Unsupported("server.restart failed"))?;
        self.wait_for_restarted_core()
            .and_then(|()| self.wait_for_pairs(&before))
            .map_err(|reason| {
                eprintln!("restart failed after {:?}: {reason}", started.elapsed());
                Unsupported("the core didn't come back after server.restart")
            })?;
        eprintln!("restart: core and pairs back after {:?}", started.elapsed());
        Ok(())
    }

    async fn crash_backend(&mut self) -> Result<(), Unsupported> {
        self.crash().map_err(|reason| {
            eprintln!("crash procedure failed: {reason}");
            Unsupported("the crash procedure failed")
        })
    }

    fn set_late_old_generation_events(&mut self, on: bool) -> Result<(), Unsupported> {
        self.control
            .set_late_old_generation_events(on, LATE_DATA, LATE_RESIZED);
        self.late = on;
        Ok(())
    }

    fn set_paint_after_resize(&mut self, _on: bool) -> Result<(), Unsupported> {
        Err(Unsupported(
            "herdr-core always repaints after an observer's RESIZE",
        ))
    }

    fn disable_byte_credit(&mut self) -> Result<(), Unsupported> {
        Err(Unsupported("herdr-core meters every viewer's bytes"))
    }
}

/// Whether each of `sent` occurs in `typed`, one after another.
fn appear_in_order(typed: &[u8], sent: &[Vec<u8>]) -> bool {
    let mut from = 0;
    for payload in sent {
        let Some(at) = typed[from..]
            .windows(payload.len().max(1))
            .position(|window| window == payload.as_slice())
        else {
            return false;
        };
        from += at + payload.len();
    }
    true
}
