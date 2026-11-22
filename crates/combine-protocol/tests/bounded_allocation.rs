//! Allocation bounds while decoding. This file holds one test so that no other
//! test thread allocates against the shared counter.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use combine_protocol::*;

struct Tracking;

static LARGEST: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LARGEST.fetch_max(layout.size(), Ordering::Relaxed);
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
}

#[global_allocator]
static ALLOCATOR: Tracking = Tracking;

/// Runs `f` and returns its result with the largest single allocation it made.
fn measure<T>(f: impl FnOnce() -> T) -> (T, usize) {
    LARGEST.store(0, Ordering::Relaxed);
    let result = f();
    (result, LARGEST.load(Ordering::Relaxed))
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

fn decode_both<M: WireMessage>(wire: &[u8]) -> [(Result<M, DecodeError>, usize); 2] {
    let (decoded, decode_peak) = measure(|| M::decode(wire).map(|(m, _)| m));
    let (read, read_peak) = measure(|| {
        let mut reader = wire;
        match read_message::<M, _>(&mut reader) {
            Ok(Some(m)) => Ok(m),
            Ok(None) => panic!("no frame"),
            Err(ReadError::Decode(e)) => Err(e),
            Err(e) => panic!("{e}"),
        }
    });
    [(decoded, decode_peak), (read, read_peak)]
}

#[test]
fn decoding_stays_within_its_allocation_bounds() {
    let hello = ClientMessage::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
    });
    let frame_cap = 1usize << 20;

    let oversize = u32::MAX.to_be_bytes().to_vec();
    for (result, peak) in decode_both::<ClientMessage>(&oversize) {
        assert!(matches!(result, Err(DecodeError::FrameTooLarge { .. })));
        assert!(peak < 64 * 1024, "peak {peak}");
    }

    let truncated = MAX_FRAME_LEN.to_be_bytes().to_vec();
    for (result, peak) in decode_both::<ClientMessage>(&truncated) {
        assert!(matches!(result, Err(DecodeError::Truncated { .. })));
        assert!(peak <= frame_cap, "peak {peak}");
    }

    let mut data = Vec::new();
    for word in [7u64, 1, 1] {
        data.extend_from_slice(&word.to_be_bytes());
    }
    data.resize(24 + MAX_DATA_CHUNK, 0xab);
    let data = frame(2, &data);
    for (result, peak) in decode_both::<DaemonMessage>(&data) {
        assert!(matches!(result, Ok(DaemonMessage::Data(_))));
        assert!(peak <= frame_cap, "peak {peak}");
    }

    let unknown_array = full_control_frame(
        &format!(r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION},"x":["#),
        |_| "0,".to_owned(),
        "0]",
    );
    for (result, peak) in decode_both::<ClientMessage>(&unknown_array) {
        assert_eq!(result.unwrap(), hello);
        assert!(peak <= frame_cap, "unknown array: peak {peak}");
    }

    let many_keys = full_control_frame(
        &format!(r#"{{"type":"hello","protocol_version":{PROTOCOL_VERSION},"#),
        |i| format!(r#""u{i}":0,"#),
        r#""last":0"#,
    );
    for (result, peak) in decode_both::<ClientMessage>(&many_keys) {
        assert_eq!(result.unwrap(), hello);
        assert!(peak <= frame_cap, "many keys: peak {peak}");
    }

    let worst_error = {
        let target = MAX_FRAME_LEN as usize - 1;
        let prefix = r#"{"type":"hello","protocol_version":""#;
        let mut payload = prefix.as_bytes().to_vec();
        payload.resize(target - 2, 0x7f);
        payload.extend_from_slice(b"\"}");
        frame(1, &payload)
    };
    for (result, peak) in decode_both::<ClientMessage>(&worst_error) {
        match result {
            Err(DecodeError::BadJson { detail }) => assert!(detail.len() <= MAX_ERROR_DETAIL),
            other => panic!("expected BadJson, got {other:?}"),
        }
        assert!(peak < 32 << 20, "worst error: peak {peak}");
    }
}
