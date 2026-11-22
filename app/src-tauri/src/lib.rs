//! Combine desktop app: the Tauri backend.

pub mod hosts;
pub mod state;

mod commands;
pub use commands::run;
