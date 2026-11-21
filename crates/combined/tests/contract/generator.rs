//! Markers and payloads for byte-exact checks.
//!
//! A marker says where measurement starts. The OSC form is an escape no terminal displays, so
//! it never shows up in a paint. The printable form is for a backend that strips unknown OSCs.
//! A payload never holds `<`, `ESC` or `BEL`, so neither marker form can occur inside one.

use std::ops::Range;

use sha2::{Digest, Sha256};

/// `ESC ] 9999 ; combine-mark ; <nonce> BEL`.
pub fn osc_marker(nonce: &str) -> Vec<u8> {
    let mut marker = b"\x1b]9999;combine-mark;".to_vec();
    marker.extend_from_slice(nonce.as_bytes());
    marker.push(0x07);
    marker
}

/// `<<combine-mark:<nonce>>>`, the fallback for a backend that strips unknown OSCs.
pub fn printable_marker(nonce: &str) -> Vec<u8> {
    let mut marker = b"<<combine-mark:".to_vec();
    marker.extend_from_slice(nonce.as_bytes());
    marker.extend_from_slice(b">>");
    marker
}

/// Hands out nonces that are unique within one case: `<case>-<n>`. The case name is the
/// test thread's name, which cargo sets to the test's name.
#[derive(Debug, Clone)]
pub struct Nonces {
    case: String,
    next: u32,
}

impl Nonces {
    pub fn new() -> Self {
        let thread = std::thread::current();
        let case: String = thread
            .name()
            .unwrap_or("case")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        Nonces { case, next: 1 }
    }

    pub fn next(&mut self) -> String {
        let nonce = format!("{}-{}", self.case, self.next);
        self.next += 1;
        nonce
    }
}

impl Default for Nonces {
    fn default() -> Self {
        Nonces::new()
    }
}

/// Deterministic program output over `0x20..=0x7e`, without `<`.
#[derive(Debug, Clone)]
pub struct Payload {
    bytes: Vec<u8>,
}

impl Payload {
    /// `len` bytes of an xorshift64* stream seeded with `seed`.
    pub fn new(seed: u64, len: usize) -> Self {
        let alphabet: Vec<u8> = (0x20..=0x7e).filter(|b| *b != b'<').collect();
        let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        let bytes = (0..len)
            .map(|_| {
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                let out = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
                alphabet[(out >> 33) as usize % alphabet.len()]
            })
            .collect();
        Payload { bytes }
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The payload cut into pieces of `size` bytes; the last may be shorter.
    pub fn pieces(&self, size: usize) -> std::slice::Chunks<'_, u8> {
        self.bytes.chunks(size)
    }

    pub fn sha256(&self) -> [u8; 32] {
        Sha256::digest(&self.bytes).into()
    }

    /// The SHA-256 of one stretch of the payload.
    pub fn sha256_of(&self, range: Range<usize>) -> [u8; 32] {
        Sha256::digest(&self.bytes[range]).into()
    }
}
