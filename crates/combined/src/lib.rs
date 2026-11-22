//! The Combine daemon's library: ports, model, core and, with `test-support`, the in-memory fake backend.
#![forbid(unsafe_code)]
pub mod core;
pub mod model;
pub mod ports;
#[cfg(feature = "test-support")]
pub mod testing;
