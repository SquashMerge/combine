//! Per-client state: the focus, active terminal and tabs of each space, and the last active space. Stored per host in `<client data>/hosts/<host-id>.json`. Nothing here imports Tauri or `hosts`.

pub mod model;
pub mod path;
pub mod service;
pub mod store;

#[cfg(test)]
mod testing;
