//! Allocation bounds while decoding, measured as live bytes: what a decode holds at once, its
//! peak, and what it still holds when it returns, with the largest single allocation beside them.
//! SPEC.md ("Frames") and `src/lib.rs` bound the memory of one decoded control frame at 32 MiB
//! ([`LIVE_BOUND`]), and every frame here is decoded from a slice and read from a stream within it.
//!
//! The counting allocator counts only the thread inside [`measure`], and [`measure`] takes a lock,
//! so the tests can run on any number of test threads: another thread's allocations, the harness's
//! included, never reach the counters, and two measurements never overlap. Each measurement runs
//! on the test's own thread, from start to end, so its numbers are the same on every run.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use combine_protocol::*;

/// The most live bytes decoding one frame may hold: SPEC.md, "Frames", and `src/lib.rs`.
const LIVE_BOUND: usize = 32 << 20;
/// The frame cap, the unit of the tighter tiers below.
const FRAME_CAP: usize = MAX_FRAME_LEN as usize;
/// What [`read_message`] holds beyond a slice decode: the frame body it reads into.
const STREAM_BUFFER: usize = FRAME_CAP;
/// Room for small allocations: a decode that stores nothing large stays under it.
const SLACK: usize = 64 * 1024;
/// What a decode that failed may still hold: the capped error detail, and change.
const KEPT_ERROR: isize = (MAX_ERROR_DETAIL + 1024) as isize;

struct Tracking;

thread_local! {
    /// Whether this thread's allocations are counted: only while it is inside [`measure`].
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

static LARGEST: AtomicUsize = AtomicUsize::new(0);
/// Bytes allocated less bytes freed since the measurement began. A free of a block from before
/// it could take this below zero; a decode from a borrowed buffer frees none.
static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

fn counting() -> bool {
    COUNTING.try_with(Cell::get).unwrap_or(false)
}

fn grow(size: usize) {
    LARGEST.fetch_max(size, Ordering::Relaxed);
    let live = LIVE.fetch_add(size as isize, Ordering::Relaxed) + size as isize;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

fn shrink(size: usize) {
    LIVE.fetch_sub(size as isize, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() && counting() {
            grow(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc_zeroed(layout);
        if !ptr.is_null() && counting() {
            grow(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if counting() {
            shrink(layout.size());
        }
        System.dealloc(ptr, layout)
    }

    /// Counted as a move, the worst case: the new block is live before the old one is freed.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let counted = counting();
        if counted {
            grow(new_size);
        }
        let moved = System.realloc(ptr, layout, new_size);
        if counted {
            shrink(if moved.is_null() {
                new_size
            } else {
                layout.size()
            });
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Tracking = Tracking;

/// What one measurement allocated.
#[derive(Debug, Clone, Copy)]
struct Usage {
    /// The largest single allocation or reallocation.
    largest: usize,
    /// The most bytes live at once, counted from zero at the start.
    peak: usize,
    /// The bytes still live at the end: what the result holds.
    retained: isize,
}

/// One measurement at a time, so the shared counters belong to it alone.
static SERIAL: Mutex<()> = Mutex::new(());

/// Stops counting this thread when dropped, even if the measured code panics.
struct Counting;

impl Counting {
    fn start() -> Counting {
        COUNTING.with(|on| on.set(true));
        Counting
    }
}

impl Drop for Counting {
    fn drop(&mut self) {
        COUNTING.with(|on| on.set(false));
    }
}

/// Runs `f` on this thread and returns its result with what it allocated.
fn measure<T>(f: impl FnOnce() -> T) -> (T, Usage) {
    let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    LARGEST.store(0, Ordering::Relaxed);
    LIVE.store(0, Ordering::Relaxed);
    PEAK.store(0, Ordering::Relaxed);
    let result = {
        let _counting = Counting::start();
        f()
    };
    let usage = Usage {
        largest: LARGEST.load(Ordering::Relaxed),
        peak: PEAK.load(Ordering::Relaxed).max(0) as usize,
        retained: LIVE.load(Ordering::Relaxed),
    };
    (result, usage)
}

fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = ((payload.len() + 1) as u32).to_be_bytes().to_vec();
    out.push(kind);
    out.extend_from_slice(payload);
    out
}

/// A control frame whose payload is exactly `MAX_FRAME_LEN - 1` bytes:
/// `prefix`, then `unit(i)` while it fits, then `tail`, then spaces, then `}`.
fn full_control_frame(prefix: &str, unit: impl Fn(usize) -> String, tail: &str) -> Vec<u8> {
    let target = MAX_FRAME_LEN as usize - 1;
    let mut payload = prefix.to_owned();
    let mut i = 0;
    loop {
        let next = unit(i);
        if payload.len() + next.len() + tail.len() + 1 > target {
            break;
        }
        payload.push_str(&next);
        i += 1;
    }
    payload.push_str(tail);
    while payload.len() < target - 1 {
        payload.push(' ');
    }
    payload.push('}');
    assert_eq!(payload.len(), target);
    frame(1, payload.as_bytes())
}

/// A control frame whose payload is exactly `MAX_FRAME_LEN - 1` bytes: `prefix`, then `fill`
/// while it fits, then `tail`, then spaces, then `}`. One value as long as the frame allows.
fn one_long_value(prefix: &str, fill: &[u8], tail: &str) -> Vec<u8> {
    let target = MAX_FRAME_LEN as usize - 1;
    let mut payload = prefix.as_bytes().to_vec();
    while payload.len() + fill.len() + tail.len() < target {
        payload.extend_from_slice(fill);
    }
    payload.extend_from_slice(tail.as_bytes());
    payload.resize(target - 1, b' ');
    payload.push(b'}');
    frame(1, &payload)
}

/// Decodes `wire` from a slice and reads it from a stream, and checks both against
/// [`LIVE_BOUND`]. Returns each result with its usage, the slice decode first.
fn decode_both<M: WireMessage>(case: &str, wire: &[u8]) -> [(Result<M, DecodeError>, Usage); 2] {
    let (decoded, decode_usage) = measure(|| M::decode(wire).map(|(m, _)| m));
    let (read, read_usage) = measure(|| {
        let mut reader = wire;
        match read_message::<M, _>(&mut reader) {
            Ok(Some(m)) => Ok(m),
            Ok(None) => panic!("no frame"),
            Err(ReadError::Decode(e)) => Err(e),
            Err(e) => panic!("{e}"),
        }
    });
    for (how, usage) in [("slice", decode_usage), ("stream", read_usage)] {
        assert!(
            usage.peak <= LIVE_BOUND,
            "{case} ({how}): {} live bytes, over the {LIVE_BOUND}-byte bound",
            usage.peak
        );
    }
    [(decoded, decode_usage), (read, read_usage)]
}

/// Decodes `wire` both ways and checks that it decodes, that no allocation is over
/// `largest_max`, and that the slice decode holds at most `live_max` bytes at once (the stream
/// read holds its frame buffer as well). Returns the slice decode's usage.
fn bounded<M: WireMessage + std::fmt::Debug>(
    case: &str,
    wire: &[u8],
    largest_max: usize,
    live_max: usize,
) -> Usage {
    let [(decoded, slice), (read, stream)] = decode_both::<M>(case, wire);
    assert!(decoded.is_ok(), "{case}: {decoded:?}");
    assert!(read.is_ok(), "{case}: {read:?}");
    for (how, usage, live) in [
        ("slice", slice, live_max),
        ("stream", stream, live_max + STREAM_BUFFER),
    ] {
        assert!(
            usage.largest <= largest_max,
            "{case} ({how}): largest {}",
            usage.largest
        );
        assert!(usage.peak <= live, "{case} ({how}): live {}", usage.peak);
    }
    slice
}

/// Decodes `wire` both ways and expects `BadJson` with a capped detail. Checks the slice
/// decode's live peak against `live_max` (the stream read's against that and its frame buffer),
/// and what the error keeps against [`KEPT_ERROR`].
fn bad_json<M: WireMessage + std::fmt::Debug>(case: &str, wire: &[u8], live_max: usize) {
    let [slice, stream] = decode_both::<M>(case, wire);
    for (how, (result, usage), live) in [
        ("slice", slice, live_max),
        ("stream", stream, live_max + STREAM_BUFFER),
    ] {
        match result {
            Err(DecodeError::BadJson { detail }) => {
                assert!(detail.len() <= MAX_ERROR_DETAIL, "{case}: {}", detail.len())
            }
            other => panic!("{case} ({how}): expected BadJson, got {other:?}"),
        }
        assert!(usage.peak <= live, "{case} ({how}): {usage:?}");
        assert!(usage.retained <= KEPT_ERROR, "{case} ({how}): {usage:?}");
    }
}

#[test]
fn the_counter_sees_live_bytes_reallocations_and_frees() {
    let (kept, usage) = measure(|| (0..4).map(|_| vec![0u8; 1 << 20]).collect::<Vec<_>>());
    assert_eq!(usage.largest, 1 << 20);
    assert!(usage.peak >= 4 << 20, "{usage:?}");
    assert!(usage.retained >= 4 << 20, "{usage:?}");
    drop(kept);

    // Growing a vector by doubling reallocates; each step counts the old and the new block.
    let (grown, usage) = measure(|| {
        let mut v = Vec::new();
        for i in 0..(1u32 << 20) {
            v.push(i);
        }
        v
    });
    assert!(usage.largest >= 4 << 20, "{usage:?}");
    assert!(
        usage.peak >= usage.largest + (usage.largest / 2),
        "{usage:?}"
    );
    assert!(usage.retained >= 4 << 20, "{usage:?}");
    drop(grown);

    let ((), usage) = measure(|| drop(vec![0u8; 8 << 20]));
    assert_eq!(
        (usage.largest, usage.peak, usage.retained),
        (8 << 20, 8 << 20, 0)
    );
}

#[test]
fn oversize_and_truncated_headers_allocate_little() {
    let oversize = u32::MAX.to_be_bytes().to_vec();
    for (result, usage) in decode_both::<ClientMessage>("oversize, client", &oversize) {
        assert!(matches!(result, Err(DecodeError::FrameTooLarge { .. })));
        assert!(usage.largest < 64 * 1024, "{usage:?}");
        assert!(usage.peak < SLACK, "{usage:?}");
    }

    for (result, usage) in decode_both::<DaemonMessage>("oversize, daemon", &oversize) {
        assert!(matches!(result, Err(DecodeError::FrameTooLarge { .. })));
        assert!(usage.largest < 64 * 1024, "{usage:?}");
        assert!(usage.peak < SLACK, "{usage:?}");
    }

    let truncated = MAX_FRAME_LEN.to_be_bytes().to_vec();
    for (result, usage) in decode_both::<ClientMessage>("truncated", &truncated) {
        assert!(matches!(result, Err(DecodeError::Truncated { .. })));
        assert!(usage.largest <= FRAME_CAP, "{usage:?}");
        assert!(usage.peak <= FRAME_CAP + SLACK, "{usage:?}");
    }
}

#[test]
fn decoding_stays_within_its_allocation_bounds() {
    let hello = ClientMessage::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
    });
    let frame_cap = FRAME_CAP;

    let mut data = Vec::new();
    for word in [7u64, 1, 1] {
        data.extend_from_slice(&word.to_be_bytes());
    }
    data.resize(24 + MAX_DATA_CHUNK, 0xab);
    let data = frame(2, &data);
    for (result, usage) in decode_both::<DaemonMessage>("data", &data) {
        assert!(matches!(result, Ok(DaemonMessage::Data(_))));
        assert!(usage.largest <= frame_cap, "{usage:?}");
    }

    let unknown_array = full_control_frame(
        &format!(r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION},"x":["#),
        |_| "0,".to_owned(),
        "0]",
    );
    for (result, usage) in decode_both::<ClientMessage>("unknown array", &unknown_array) {
        assert_eq!(result.unwrap(), hello);
        assert!(usage.largest <= frame_cap, "unknown array: {usage:?}");
        assert!(
            usage.peak <= STREAM_BUFFER + SLACK,
            "unknown array: {usage:?}"
        );
    }

    let many_keys = full_control_frame(
        &format!(r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION},"#),
        |i| format!(r#""u{i}":0,"#),
        r#""last":0"#,
    );
    for (result, usage) in decode_both::<ClientMessage>("many keys", &many_keys) {
        assert_eq!(result.unwrap(), hello);
        assert!(usage.largest <= frame_cap, "many keys: {usage:?}");
        assert!(usage.peak <= STREAM_BUFFER + SLACK, "many keys: {usage:?}");
    }

    let terminal = r#"{"id":"t","cwd":"","argv":[],"state":"running","exit_code":null,"labels":{},"cols":1,"rows":1}"#;
    let full_terminals = full_control_frame(
        r#"{"type":"terminals","request":null,"spaces":[],"terminals":["#,
        |_| format!("{terminal},"),
        &format!("{terminal}]"),
    );
    for (result, usage) in decode_both::<DaemonMessage>("full terminals", &full_terminals) {
        match result {
            Ok(DaemonMessage::Terminals(list)) => assert!(list.terminals.len() > 10_000),
            other => panic!("expected terminals, got {other:?}"),
        }
        assert!(usage.largest <= 2 * frame_cap, "full terminals: {usage:?}");
    }

    let full_layout = full_control_frame(
        r#"{"type":"layout","request":null,"layout":{"space":"s","layout_revision":1,"read_only":false,"error":null,"dropped":null,"root":{"type":"tile","id":"a","active":null,"panels":["#,
        |_| r#""p","#.to_owned(),
        r#""p"]}}"#,
    );
    for (result, usage) in decode_both::<DaemonMessage>("full layout", &full_layout) {
        match result {
            Ok(DaemonMessage::Layout(layout)) => match layout.layout.root {
                LayoutNode::Tile { panels, .. } => assert!(panels.len() > 100_000),
                other => panic!("expected a tile, got {other:?}"),
            },
            other => panic!("expected layout, got {other:?}"),
        }
        // The largest single allocation: the panel list's `Vec` of 24-byte `String`s.
        assert!(usage.largest <= 8 * frame_cap, "full layout: {usage:?}");
    }

    // Live bytes, not the largest allocation: an unknown key is skipped, never stored, and a
    // list of tiny IDs costs a `String` each.
    let nested_unknown = full_control_frame(
        r#"{"type":"layout","request":null,"layout":{"space":"s","layout_revision":1,"read_only":false,"error":null,"dropped":null,"root":{"type":"split","dir":"h","ratio":0.5,"a":{"type":"tile","id":"a","panels":[],"active":null,"#,
        |i| format!(r#""u{i}":0,"#),
        r#""last":0},"b":{"type":"tile","id":"b","panels":[],"active":null}}}"#,
    );
    let op_unknown = full_control_frame(
        r#"{"type":"mutate_layout","request":1,"space":"s","base_layout_revision":0,"ops":[{"op":"close_tile","tile":"a","#,
        |i| format!(r#""u{i}":0,"#),
        r#""last":0}]"#,
    );
    let leaves = full_control_frame(
        r#"{"type":"mutate_layout","request":1,"space":"s","base_layout_revision":0,"ops":[{"op":"set_ratio","ratio":0.5,"leaves":["#,
        |_| r#""p","#.to_owned(),
        r#""p"]}]"#,
    );
    bounded::<DaemonMessage>("nested unknown keys", &nested_unknown, frame_cap, SLACK);
    bounded::<ClientMessage>("op unknown keys", &op_unknown, frame_cap, SLACK);
    bounded::<DaemonMessage>("panel list", &full_layout, 8 * frame_cap, 32 * frame_cap);
    bounded::<ClientMessage>("leaves list", &leaves, 8 * frame_cap, 32 * frame_cap);

    // Version 5 messages. Each case asserts the smallest tier that is at least 1.25 times what
    // was measured; a "measured" note gives the slice decode's largest allocation and live peak.
    // A JSON tree of an `mcp` message would hold far more than its text.
    let entry = r#"{"id":"p","title":null,"renderer":"mermaid","revision":1,"state":"validated","error":null,"created_by":{"type":"agent","slot":null}}"#;
    let panels = full_control_frame(
        r#"{"type":"panels","request":null,"space":"s","skipped":[],"error":null,"panels":["#,
        |_| format!("{entry},"),
        &format!("{entry}]"),
    );
    match DaemonMessage::decode(&panels) {
        Ok((DaemonMessage::Panels(list), _)) => assert!(list.panels.len() > 5_000),
        other => panic!("expected panels, got {other:?}"),
    }
    // measured: largest 1441792, live 2166792
    bounded::<DaemonMessage>("panels", &panels, 2 * frame_cap, 32 * frame_cap);

    // An `mcp` message is kept as its text, so its frame costs about one copy of itself in either
    // direction.
    let mcp_prefix = r#"{"type":"mcp","message":{"jsonrpc":"2.0","method":"x","params":["#;
    let mcp_array = full_control_frame(mcp_prefix, |_| "0,".to_owned(), "0]}");
    // measured: largest 1048549, live 1048560
    bounded::<ClientMessage>("mcp array, client", &mcp_array, frame_cap, 2 * frame_cap);
    bounded::<DaemonMessage>("mcp array, daemon", &mcp_array, frame_cap, 2 * frame_cap);
    // A JSON tree of this frame would hold about 100 times its text.
    let mcp_nested = full_control_frame(mcp_prefix, |_| r#"{"":{"":{"":0}}},"#.to_owned(), "0]}");
    // measured: largest 1048535, live 1048546
    bounded::<ClientMessage>("mcp nested, client", &mcp_nested, frame_cap, 2 * frame_cap);
    bounded::<DaemonMessage>("mcp nested, daemon", &mcp_nested, frame_cap, 2 * frame_cap);

    let panel_ops = full_control_frame(
        r#"{"type":"mutate_layout","request":1,"space":"s","base_layout_revision":0,"ops":["#,
        |_| r#"{"op":"add_panel","panel":"p","tile":"t"},"#.to_owned(),
        r#"{"op":"remove_panel","panel":"p"}]"#,
    );
    match ClientMessage::decode(&panel_ops) {
        Ok((ClientMessage::MutateLayout(mutate), _)) => assert!(mutate.ops.len() > 10_000),
        other => panic!("expected mutate_layout, got {other:?}"),
    }
    // measured: largest 2883584, live 4358160
    bounded::<ClientMessage>("panel ops", &panel_ops, 8 * frame_cap, 32 * frame_cap);

    // Unknown keys are skipped, never stored.
    let created_by_keys = full_control_frame(
        r#"{"type":"panels","request":null,"space":"s","skipped":[],"error":null,"panels":[{"id":"p","renderer":"mermaid","revision":1,"state":"validated","created_by":{"type":"user","#,
        |i| format!(r#""u{i}":0,"#),
        r#""last":0}}]"#,
    );
    // measured: largest 704, live 712
    bounded::<DaemonMessage>("created_by keys", &created_by_keys, 2 * frame_cap, SLACK);

    let get_panel_keys = full_control_frame(
        r#"{"type":"get_panel","request":1,"space":"s","panel":"p","#,
        |i| format!(r#""u{i}":0,"#),
        r#""last":0"#,
    );
    // measured: largest 9, live 11
    bounded::<ClientMessage>("get_panel keys", &get_panel_keys, 2 * frame_cap, SLACK);
}

/// An unknown key is skipped, but serde_json copies one with an escape into its key scratch
/// buffer first. `src/lib.rs` allows that buffer about twice the key; measured, it is about once.
/// Each case asserts the smallest tier at least 1.25 times the slice decode's measure.
#[test]
fn escaped_unknown_keys_stay_within_the_live_bound() {
    let frame_cap = FRAME_CAP;
    let hello = format!(r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION},""#);
    let cases = [
        // One key as long as the frame, escaped only at its start, so all of it is copied.
        // measured: largest 1048528, live 1048541
        (
            "one key",
            one_long_value(&format!(r#"{hello}\u0041"#), b"k", r#"":0"#),
        ),
        // Every character escaped: the copy is a sixth of the text.
        // measured: largest 262144, live 393221
        (
            "one escaped key",
            one_long_value(&hello, br"\u0041", r#"":0"#),
        ),
        // Escapes that unescape to two, three and four bytes. measured: largest 524288, live 786437
        (
            "wide key",
            one_long_value(&hello, br"\u00e9\u4e2d\ud83d\ude00", r#"":0"#),
        ),
        // Many escaped keys: the buffer is reused, so it stays the size of one. measured: live 13
        (
            "many escaped keys",
            full_control_frame(
                &format!(r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION},"#),
                |i| format!(r#""\u0041{i}":0,"#),
                r#""last":0"#,
            ),
        ),
        // An unknown string value is skipped without a copy, escapes and all. measured: live 5
        (
            "escaped unknown value",
            one_long_value(&format!(r#"{hello}x":""#), br"\n", r#"""#),
        ),
    ];
    let tiers = [
        (frame_cap, 2 * frame_cap),
        (frame_cap, frame_cap),
        (frame_cap, frame_cap),
        // The stream read's frame buffer is the largest allocation of these two.
        (frame_cap, SLACK),
        (frame_cap, SLACK),
    ];
    for ((case, wire), (largest_max, live_max)) in cases.iter().zip(tiers) {
        let usage = bounded::<ClientMessage>(case, wire, largest_max, live_max);
        assert!(usage.retained <= SLACK as isize, "{case}: {usage:?}");
    }
    // An escaped key in a nested known struct of a daemon message.
    // measured: largest 1048390, live 1048410
    let nested = one_long_value(
        r#"{"type":"layout","request":null,"layout":{"space":"s","layout_revision":1,"read_only":false,"error":null,"dropped":null,"root":{"type":"tile","id":"a","panels":[],"active":null,"\n"#,
        b"k",
        r#"":0}}"#,
    );
    let usage = bounded::<DaemonMessage>("nested key", &nested, frame_cap, 2 * frame_cap);
    assert!(usage.retained <= SLACK as isize, "nested key: {usage:?}");
}

/// The longest strings a message keeps: one value as long as the frame, plain, escaped and wide,
/// so both the kept `String` and serde_json's scratch copy of an escaped one are counted. The
/// most measured: largest 1048447, live 1048518 (quote escapes: a half-frame copy, then the
/// half-frame `String`).
#[test]
fn the_largest_kept_strings_stay_within_the_live_bound() {
    let frame_cap = FRAME_CAP;
    let panel = r#"{"type":"panel","request":1,"space":"s","id":"p","revision":1,"renderer":"mermaid","state":"validated","error":null,"source":""#;
    for (case, fill) in [
        ("plain source", &b"x"[..]),
        ("quote escapes", br#"\""#),
        ("two-byte escapes", br"\u00e9"),
        ("surrogate pairs", br"\ud83d\ude00"),
        ("raw UTF-8", "\u{e9}\u{4e2d}".as_bytes()),
    ] {
        let wire = one_long_value(panel, fill, r#"""#);
        match DaemonMessage::decode(&wire) {
            Ok((DaemonMessage::Panel(panel), _)) => assert!(panel.source.len() > 300_000),
            other => panic!("{case}: expected panel, got {other:?}"),
        }
        let usage = bounded::<DaemonMessage>(case, &wire, frame_cap, 2 * frame_cap);
        assert!(usage.retained <= frame_cap as isize, "{case}: {usage:?}");
    }

    let error = one_long_value(
        r#"{"type":"error","code":"internal","request":null,"attachment":null,"message":""#,
        br"\u00e9",
        r#"""#,
    );
    // measured: largest 524288, live 873791
    bounded::<DaemonMessage>("error message", &error, frame_cap, 2 * frame_cap);

    let mcp_open = one_long_value(
        r#"{"type":"mcp_open","request":1,"caller":{"terminal_id":null,"terminal_socket":null,"cwd":""#,
        b"x",
        r#""}"#,
    );
    match ClientMessage::decode(&mcp_open) {
        Ok((ClientMessage::McpOpen(open), _)) => {
            assert!(open.caller.cwd.unwrap().len() > 1_000_000)
        }
        other => panic!("expected mcp_open, got {other:?}"),
    }
    // measured: largest 1048482, live 1048490
    bounded::<ClientMessage>("mcp_open cwd", &mcp_open, frame_cap, 2 * frame_cap);

    // One long final string. measured: largest 1048477 to 1048531, live 1048491 to 1048541
    for (case, prefix) in [
        (
            "get_panel",
            r#"{"type":"get_panel","request":1,"space":"s","panel":""#,
        ),
        (
            "close_panel",
            r#"{"type":"close_panel","request":1,"space":"s","panel":""#,
        ),
        (
            "pin_panel",
            r#"{"type":"pin_panel","request":1,"space":"s","panel":""#,
        ),
        (
            "report_render",
            r#"{"type":"report_render","space":"s","revision":1,"status":"render_failed","error":null,"panel":""#,
        ),
    ] {
        let wire = full_control_frame(prefix, |_| "x".to_owned(), r#"""#);
        bounded::<ClientMessage>(case, &wire, 2 * frame_cap, 2 * frame_cap);
    }
    for (case, prefix) in [
        (
            "panel_closed",
            r#"{"type":"panel_closed","request":1,"space":"s","panel":""#,
        ),
        (
            "panel_pinned",
            r#"{"type":"panel_pinned","request":1,"space":"s","panel":""#,
        ),
        (
            "mcp_opened",
            r#"{"type":"mcp_opened","request":1,"space":""#,
        ),
    ] {
        let wire = full_control_frame(prefix, |_| "x".to_owned(), r#"""#);
        bounded::<DaemonMessage>(case, &wire, 2 * frame_cap, 2 * frame_cap);
    }
}

/// serde_json builds its whole error text before `CappedText` keeps the first
/// [`MAX_ERROR_DETAIL`] bytes: for a bad string value the text quotes the value, with each
/// unusual character spelled as an escape. That text is live during the decode, within the
/// bound, and only the capped detail is left when the decode returns.
#[test]
fn serde_error_text_is_capped_before_it_is_kept() {
    let frame_cap = FRAME_CAP;
    let hello = r#"{"type":"hello","protocol_version":""#;
    // DEL, which the error text spells `\u{7f}`: 8 bytes for each byte of the frame. The worst
    // case here. measured: largest 6553600, live 12844864
    let del = one_long_value(hello, b"\x7f", r#"""#);
    bad_json::<ClientMessage>("invalid type, DEL", &del, 16 * frame_cap);
    // measured: largest 2097118, live 3145697
    let plain = one_long_value(hello, b"x", r#"""#);
    bad_json::<ClientMessage>("invalid type, plain", &plain, 4 * frame_cap);
    // Unescaped into serde_json's scratch buffer first, then quoted in the text.
    // measured: largest 1638400, live 2774366
    let escaped = one_long_value(hello, br"\u0001", r#"""#);
    bad_json::<ClientMessage>("invalid type, escaped", &escaped, 4 * frame_cap);
    // measured: largest 2097012 and 2097040, live 3145574 and 3145893
    let variant = one_long_value(
        r#"{"type":"report_render","space":"s","panel":"p","revision":1,"error":null,"status":""#,
        b"\x7f",
        r#"""#,
    );
    bad_json::<ClientMessage>("unknown variant, client", &variant, 4 * frame_cap);
    let code = one_long_value(
        r#"{"type":"error","request":null,"attachment":null,"message":"","code":""#,
        b"\x7f",
        r#"""#,
    );
    bad_json::<DaemonMessage>("unknown variant, daemon", &code, 4 * frame_cap);
}
