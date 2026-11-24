//! The Combine daemon's library: ports, model, core and, with `test-support`, the in-memory fake backend.
#![forbid(unsafe_code)]
pub mod adapters {
    pub mod fs;
    pub mod herdr_core;
    pub mod loader;
}
pub mod core;
pub mod model;
pub mod ports;
pub mod rpc;
#[cfg(feature = "test-support")]
pub mod testing;
pub mod wiring;
