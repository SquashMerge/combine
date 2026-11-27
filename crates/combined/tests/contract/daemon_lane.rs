//! Daemon scenarios on the herdr-core backend: a real `combined serve` over the lane's isolated
//! core, spoken to through `combined connect` with piped stdio, which is what `ssh -T` runs.
//!
//! Every scenario takes its daemon paths from `DaemonPaths::with_lane_herdr_core_socket`, starts
//! its children only through `daemon_support`'s `combined_command` (a cleared environment), and
//! restarts the core only through the target, which checks the pinned core id first.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

use combine_protocol::{
    Attach, AttachmentId, BackendGeneration, ClientMessage, CloseReason, Cursor, DaemonMessage,
    Detach, Epoch, Input, OwnershipGeneration, RequestId, Resume, TermId, WatchTerminals,
};

use crate::common::request_options;
use crate::contract::report::{record, Verdict};
use crate::contract::target::Target;
use crate::contract::Failure;
use crate::daemon_support::{wait_until, BridgeClient, DaemonPaths, DaemonProcess, ShortTempDir};
use crate::herdr_core_target::HerdrCoreTarget;

pub type Scenario = fn(&mut HerdrCoreTarget, &ShortTempDir) -> Result<(), String>;

const MESSAGE_WAIT: Duration = Duration::from_secs(10);
const RESTART_WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(250);

/// Runs `scenario` on a fresh lane target and a fresh scratch directory, records the verdict and
/// panics with the result line on a failure. The scratch directory is declared after the target
/// and so is dropped first, after every child the scenario started.
pub fn run(case: &str, scenario: Scenario) {
    let mut target = HerdrCoreTarget::from_lane();
    let scratch = ShortTempDir::new();
    let outcome = catch_unwind(AssertUnwindSafe(|| scenario(&mut target, &scratch)));
    let verdict = match outcome {
        Ok(Ok(())) => Verdict::Pass,
        Ok(Err(reason)) => Verdict::Fail(Failure::new("daemon", reason)),
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|text| text.to_string()))
                .unwrap_or_else(|| "a panic".to_string());
            Verdict::Fail(Failure::new("panic", message))
        }
    };
    let line = record(case, &target.describe(), &verdict);
    if let Verdict::Fail(failure) = verdict {
        panic!("{line}\n{}", failure.message);
    }
}

/// What one attachment has shown the client so far.
#[derive(Default)]
struct Seen {
    started: Vec<(Epoch, BackendGeneration)>,
    epochs: BTreeSet<u64>,
    data: Vec<u8>,
    data_cursors: Vec<Cursor>,
    last: Option<Cursor>,
    gap: bool,
    lost: bool,
    closed: Option<CloseReason>,
    ownership: Option<(OwnershipGeneration, Option<AttachmentId>)>,
}

/// Everything a bridge has said, filed by attachment.
#[derive(Default)]
struct Inbox {
    seen: BTreeMap<AttachmentId, Seen>,
    attached: BTreeMap<u64, AttachmentId>,
    resumed: Option<(AttachmentId, Option<Cursor>)>,
    listed: Vec<Vec<TermId>>,
    /// The backend instance each `terminals` snapshot named.
    instances: Vec<Option<String>>,
}

impl Inbox {
    fn view(&self, attachment: AttachmentId) -> Option<&Seen> {
        self.seen.get(&attachment)
    }

    fn file(&mut self, message: &DaemonMessage) -> Result<(), String> {
        if let Some(cursor) = message.cursor() {
            let seen = self.seen.entry(cursor.attachment).or_default();
            seen.epochs.insert(cursor.epoch.0);
            seen.last = Some(cursor);
        }
        match message {
            DaemonMessage::Error(error) => {
                return Err(format!(
                    "the daemon sent an error: {:?} {}",
                    error.code,
                    error.trimmed_message()
                ));
            }
            DaemonMessage::Attached(attached) => {
                self.attached
                    .insert(attached.request.0, attached.attachment);
            }
            DaemonMessage::Resumed(resumed) => {
                self.resumed = Some((resumed.attachment, resumed.replay_through));
            }
            DaemonMessage::Terminals(terminals) => {
                let ids = terminals.terminals.iter().map(|t| t.id.clone()).collect();
                self.listed.push(ids);
                self.instances.push(
                    terminals
                        .backend
                        .clone()
                        .and_then(|backend| backend.instance),
                );
            }
            DaemonMessage::Ownership(ownership) => {
                self.seen.entry(ownership.attachment).or_default().ownership =
                    Some((ownership.ownership_generation, ownership.owner));
            }
            DaemonMessage::Started(started) => {
                let seen = self.seen.entry(started.cursor.attachment).or_default();
                seen.started
                    .push((started.cursor.epoch, started.backend_generation));
            }
            DaemonMessage::Data(data) => {
                let seen = self.seen.entry(data.cursor.attachment).or_default();
                seen.data.extend_from_slice(&data.bytes);
                seen.data_cursors.push(data.cursor);
            }
            DaemonMessage::Closed(closed) => {
                self.seen
                    .entry(closed.cursor.attachment)
                    .or_default()
                    .closed = Some(closed.reason);
            }
            DaemonMessage::Lost(lost) => {
                self.seen.entry(lost.cursor.attachment).or_default().lost = true;
            }
            DaemonMessage::Gap(gap) => {
                self.seen.entry(gap.attachment).or_default().gap = true;
            }
            _ => {}
        }
        Ok(())
    }
}

fn input(bridge: &mut BridgeClient, attachment: AttachmentId, bytes: &[u8]) {
    bridge.send(&ClientMessage::Input(Input {
        attachment,
        bytes: bytes.to_vec(),
    }));
}

/// Reads and files messages until `pred` holds, acking each attachment event at once when `ack`
/// is set. It fails on a daemon error and at `within`.
fn pump_until(
    bridge: &mut BridgeClient,
    inbox: &mut Inbox,
    within: Duration,
    what: &str,
    ack: bool,
    pred: impl Fn(&Inbox) -> bool,
) -> Result<(), String> {
    let deadline = Instant::now() + within;
    while !pred(inbox) {
        if Instant::now() >= deadline {
            return Err(format!("no {what} within {within:?}"));
        }
        let Some(message) = bridge.recv_within(POLL) else {
            continue;
        };
        inbox.file(&message)?;
        if ack && !matches!(message, DaemonMessage::Closed(_) | DaemonMessage::Lost(_)) {
            if let Some(cursor) = message.cursor() {
                bridge.ack(cursor);
            }
        }
    }
    Ok(())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn typed_has(target: &HerdrCoreTarget, term: &TermId, needle: &[u8]) -> bool {
    target
        .typed_input(term)
        .is_ok_and(|typed| contains(&typed, needle))
}

fn spawn_terminal(target: &mut HerdrCoreTarget) -> Result<TermId, String> {
    let cwd = target.scratch_dir("lane");
    let spec = target.spawn_spec(&cwd, 80, 24);
    let backend = target.backend();
    HerdrCoreTarget::block_on(backend.spawn(spec)).map_err(|error| format!("spawn: {error}"))
}

fn emit(target: &mut HerdrCoreTarget, term: &TermId, bytes: &[u8]) -> Result<(), String> {
    HerdrCoreTarget::block_on(target.emit(term, bytes))
        .map_err(|error| format!("emit: {}", error.0))
}

fn start_daemon(target: &HerdrCoreTarget, scratch: &ShortTempDir) -> DaemonProcess {
    let paths = DaemonPaths::in_scratch(scratch, "d")
        .with_lane_herdr_core_socket(target.lane_socket().to_path_buf());
    let mut daemon = DaemonProcess::spawn(scratch, &paths, "herdr-core");
    daemon.wait_ready();
    daemon
}

fn attach(bridge: &mut BridgeClient, request: u64, term: &TermId, cols: u16, rows: u16) {
    attach_pinned(bridge, request, term, cols, rows, None);
}

/// An attach that names the backend instance a pinned panel expects.
fn attach_pinned(
    bridge: &mut BridgeClient,
    request: u64,
    term: &TermId,
    cols: u16,
    rows: u16,
    instance: Option<String>,
) {
    bridge.send(&ClientMessage::Attach(Attach {
        request: RequestId(request),
        terminal: term.clone(),
        options: request_options(cols, rows, false),
        instance,
    }));
}

fn require_clean_exit(what: &str, status: std::process::ExitStatus) -> Result<(), String> {
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} exited with {status}"))
    }
}

/// AC2: attach, disconnect with no signal, reconnect, replay from the cursor and resume.
pub fn connect_attach_replay_resume(
    target: &mut HerdrCoreTarget,
    scratch: &ShortTempDir,
) -> Result<(), String> {
    let term = spawn_terminal(target)?;
    let mut daemon = start_daemon(target, scratch);

    let (mut bridge, _welcome) = BridgeClient::open(scratch, daemon.socket());
    let mut inbox = Inbox::default();
    bridge.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(1),
    }));
    pump_until(
        &mut bridge,
        &mut inbox,
        RESTART_WAIT,
        "terminals listing with the terminal",
        true,
        |inbox| inbox.listed.iter().any(|ids| ids.contains(&term)),
    )?;
    // Attach and resume as a pinned panel does, naming the core the daemon reported.
    let instance = inbox
        .instances
        .last()
        .cloned()
        .flatten()
        .ok_or("the daemon named no backend instance")?;
    if instance != target.pinned_core_id() {
        return Err(format!(
            "the daemon named backend instance {instance:?}, not the lane's core {:?}",
            target.pinned_core_id()
        ));
    }

    attach_pinned(&mut bridge, 2, &term, 80, 24, Some(instance.clone()));
    pump_until(
        &mut bridge,
        &mut inbox,
        MESSAGE_WAIT,
        "attached",
        true,
        |inbox| inbox.attached.contains_key(&2),
    )?;
    let attachment = inbox.attached[&2];
    pump_until(
        &mut bridge,
        &mut inbox,
        MESSAGE_WAIT,
        "started",
        true,
        |inbox| {
            inbox
                .view(attachment)
                .is_some_and(|seen| !seen.started.is_empty())
        },
    )?;

    emit(target, &term, b"[c23 one]")?;
    pump_until(
        &mut bridge,
        &mut inbox,
        MESSAGE_WAIT,
        "one",
        true,
        |inbox| {
            inbox
                .view(attachment)
                .is_some_and(|seen| contains(&seen.data, b"[c23 one]"))
        },
    )?;
    let c1 = inbox
        .view(attachment)
        .and_then(|seen| seen.last)
        .ok_or("no cursor after one")?;

    emit(target, &term, b"[c23 two]")?;
    pump_until(
        &mut bridge,
        &mut inbox,
        MESSAGE_WAIT,
        "two",
        false,
        |inbox| {
            inbox
                .view(attachment)
                .is_some_and(|seen| contains(&seen.data, b"[c23 two]"))
        },
    )?;

    bridge.close_stdin();
    require_clean_exit("the first bridge", bridge.wait_exit())?;
    drop(bridge);

    emit(target, &term, b"[c23 three]")?;

    let (mut bridge, _welcome) = BridgeClient::open(scratch, daemon.socket());
    let mut inbox = Inbox::default();
    bridge.send(&ClientMessage::Resume(Resume {
        request: RequestId(3),
        cursor: c1,
        instance: Some(instance),
    }));
    pump_until(
        &mut bridge,
        &mut inbox,
        MESSAGE_WAIT,
        "resumed",
        true,
        |inbox| inbox.resumed.is_some(),
    )?;
    let (resumed, replay_through) = inbox.resumed.ok_or("no resumed")?;
    if resumed != attachment {
        return Err(format!("resumed {resumed:?}, not {attachment:?}"));
    }
    let rt = replay_through.ok_or("the resume replayed nothing")?;
    pump_until(
        &mut bridge,
        &mut inbox,
        MESSAGE_WAIT,
        "the replay",
        true,
        |inbox| {
            inbox
                .view(attachment)
                .is_some_and(|seen| seen.data_cursors.last().is_some_and(|c| c.seq >= rt.seq))
        },
    )?;
    let replayed = inbox.view(attachment).ok_or("no replay")?;
    if !contains(&replayed.data, b"[c23 two]") || !contains(&replayed.data, b"[c23 three]") {
        return Err("the replay is missing two or three".to_string());
    }
    if contains(&replayed.data, b"[c23 one]") {
        return Err("the replay repeats one, which was acked".to_string());
    }
    let seqs: Vec<u64> = replayed.data_cursors.iter().map(|c| c.seq.0).collect();
    if !seqs.windows(2).all(|pair| pair[0] < pair[1]) || seqs.first().is_none_or(|s| *s <= c1.seq.0)
    {
        return Err(format!("replayed seqs {seqs:?} after {}", c1.seq.0));
    }

    emit(target, &term, b"[c23 four]")?;
    pump_until(
        &mut bridge,
        &mut inbox,
        MESSAGE_WAIT,
        "four",
        true,
        |inbox| {
            inbox
                .view(attachment)
                .is_some_and(|seen| contains(&seen.data, b"[c23 four]"))
        },
    )?;
    let live = inbox
        .view(attachment)
        .and_then(|seen| seen.data_cursors.get(seqs.len()))
        .ok_or("four's event is missing")?;
    if live.seq.0 != rt.seq.0 + 1 {
        return Err(format!("four has seq {}, after {}", live.seq.0, rt.seq.0));
    }

    bridge.send(&ClientMessage::Detach(Detach { attachment }));
    pump_until(
        &mut bridge,
        &mut inbox,
        MESSAGE_WAIT,
        "closed",
        true,
        |inbox| {
            inbox
                .view(attachment)
                .is_some_and(|seen| seen.closed == Some(CloseReason::Detached))
        },
    )?;
    bridge.close_stdin();
    require_clean_exit("the second bridge", bridge.wait_exit())?;
    let stderr = bridge.stderr();
    if !stderr.is_empty() {
        return Err(format!("the bridge wrote to stderr: {stderr}"));
    }

    daemon.terminate();
    require_clean_exit("the daemon", daemon.wait_exit())
}

/// AC6: two panels on one terminal across a `server.restart`, with no stale owner.
pub fn two_panels_across_a_restart(
    target: &mut HerdrCoreTarget,
    scratch: &ShortTempDir,
) -> Result<(), String> {
    let term = spawn_terminal(target)?;
    let mut daemon = start_daemon(target, scratch);

    let (mut bridge, _welcome) = BridgeClient::open(scratch, daemon.socket());
    let mut inbox = Inbox::default();
    bridge.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(9),
    }));
    attach(&mut bridge, 1, &term, 80, 24);
    attach(&mut bridge, 2, &term, 100, 30);
    pump_until(
        &mut bridge,
        &mut inbox,
        MESSAGE_WAIT,
        "both attached",
        true,
        |inbox| inbox.attached.len() == 2,
    )?;
    let (a, b) = (inbox.attached[&1], inbox.attached[&2]);
    pump_until(
        &mut bridge,
        &mut inbox,
        MESSAGE_WAIT,
        "both started",
        true,
        |inbox| {
            [a, b].iter().all(|id| {
                inbox
                    .view(*id)
                    .is_some_and(|seen| seen.started.iter().any(|(epoch, _)| epoch.0 == 1))
            })
        },
    )?;

    input(&mut bridge, a, b"a-before");
    pump_until(
        &mut bridge,
        &mut inbox,
        MESSAGE_WAIT,
        "A as owner",
        true,
        |inbox| {
            [a, b].iter().all(|id| {
                inbox
                    .view(*id)
                    .and_then(|seen| seen.ownership)
                    .is_some_and(|(_, owner)| owner == Some(a))
            })
        },
    )?;
    let generation = inbox
        .view(a)
        .and_then(|seen| seen.ownership)
        .map(|(generation, _)| generation)
        .ok_or("no ownership")?;
    if !wait_until(MESSAGE_WAIT, || typed_has(target, &term, b"a-before")) {
        return Err("the program never received a-before".to_string());
    }
    let first_generation = inbox
        .view(a)
        .and_then(|seen| seen.started.first())
        .map(|(_, generation)| generation.0)
        .ok_or("no started")?;

    HerdrCoreTarget::block_on(target.restart_backend())
        .map_err(|error| format!("restart: {}", error.0))?;
    pump_until(
        &mut bridge,
        &mut inbox,
        RESTART_WAIT,
        "a started in epoch 2 for both",
        true,
        |inbox| {
            [a, b].iter().all(|id| {
                inbox.view(*id).is_some_and(|seen| {
                    seen.started
                        .iter()
                        .any(|(epoch, generation)| epoch.0 == 2 && generation.0 > first_generation)
                })
            })
        },
    )?;

    let before = inbox.instances.len();
    bridge.send(&ClientMessage::WatchTerminals(WatchTerminals {
        request: RequestId(10),
    }));
    pump_until(
        &mut bridge,
        &mut inbox,
        MESSAGE_WAIT,
        "a terminals snapshot after the restart",
        true,
        |inbox| inbox.instances.len() > before,
    )?;
    // The restarted core is checked against the pinned core id, so every snapshot naming that id
    // means the daemon reports the core it serves, before and after the restart.
    target.checked_status()?;
    let pinned = Some(target.pinned_core_id().to_string());
    if inbox.instances.iter().any(|seen| *seen != pinned) {
        return Err(format!(
            "a terminals snapshot named another backend instance than the lane's core {pinned:?}: {:?}",
            inbox.instances
        ));
    }

    input(&mut bridge, a, b"a-after");
    if !wait_until(MESSAGE_WAIT, || typed_has(target, &term, b"a-after")) {
        return Err("the program never received a-after".to_string());
    }
    input(&mut bridge, b, b"b-after");
    pump_until(
        &mut bridge,
        &mut inbox,
        MESSAGE_WAIT,
        "B as owner",
        true,
        |inbox| {
            [a, b].iter().all(|id| {
                inbox
                    .view(*id)
                    .and_then(|seen| seen.ownership)
                    .is_some_and(|(_, owner)| owner == Some(b))
            })
        },
    )?;
    for id in [a, b] {
        let (now, _) = inbox
            .view(id)
            .and_then(|seen| seen.ownership)
            .ok_or("no ownership")?;
        if now.0 != generation.0 + 1 {
            return Err(format!(
                "ownership generation {} for {id:?}, expected {}",
                now.0,
                generation.0 + 1
            ));
        }
    }

    let ordered = |typed: &[u8]| {
        let mut from = 0;
        for needle in [&b"a-before"[..], b"a-after", b"b-after"] {
            match typed[from..]
                .windows(needle.len())
                .position(|window| window == needle)
            {
                Some(at) => from += at + needle.len(),
                None => return false,
            }
        }
        true
    };
    if !wait_until(MESSAGE_WAIT, || {
        target.typed_input(&term).is_ok_and(|typed| ordered(&typed))
    }) {
        return Err(format!(
            "typed input out of order: {:?}",
            target
                .typed_input(&term)
                .map(|typed| String::from_utf8_lossy(&typed).into_owned())
        ));
    }
    if !wait_until(MESSAGE_WAIT, || {
        target.pty_size(&term).is_ok_and(|size| size == (100, 30))
    }) {
        return Err(format!(
            "the PTY is {:?}, not the new owner's 100x30",
            target.pty_size(&term).ok()
        ));
    }

    for id in [a, b] {
        let seen = inbox.view(id).ok_or("an attachment saw nothing")?;
        let epochs: Vec<u64> = seen.epochs.iter().copied().collect();
        if epochs != [1, 2] || seen.started.len() != 2 {
            return Err(format!(
                "{id:?} saw epochs {epochs:?} with {} started, expected exactly 1 and 2",
                seen.started.len()
            ));
        }
        if seen.gap || seen.lost || seen.closed.is_some() {
            return Err(format!(
                "{id:?} saw a gap, lost or closed across the restart"
            ));
        }
    }

    for id in [a, b] {
        bridge.send(&ClientMessage::Detach(Detach { attachment: id }));
    }
    pump_until(
        &mut bridge,
        &mut inbox,
        MESSAGE_WAIT,
        "both closed",
        true,
        |inbox| {
            [a, b].iter().all(|id| {
                inbox
                    .view(*id)
                    .is_some_and(|seen| seen.closed == Some(CloseReason::Detached))
            })
        },
    )?;
    bridge.close_stdin();
    require_clean_exit("the bridge", bridge.wait_exit())?;

    daemon.terminate();
    require_clean_exit("the daemon", daemon.wait_exit())
}
