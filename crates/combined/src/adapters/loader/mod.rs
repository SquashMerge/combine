//! The loader adapter: Pele, vendored, run headlessly in a JS engine on its own thread, behind
//! the [`DiagramCheck`](crate::ports::DiagramCheck) port (SPEC.md, Feedback). Only `engine.rs`
//! names the JS engine.

mod diagram_check;
mod engine;
mod pele;

pub use diagram_check::{
    DiagramLimits, EngineStats, PeleDiagramCheck, Warmup, MAX_SOURCE_BYTES, MEMORY_LIMIT,
    TIME_LIMIT,
};
pub use pele::{
    MAX_MESSAGE_BYTES, MAX_SNIPPET_BYTES, PELE_GLOBAL_SHA256, PELE_INTEGRITY, PELE_LICENSE_SHA256,
    PELE_MAX_EDGES, PELE_OUTPUT_LIMIT, PELE_VERSION,
};
