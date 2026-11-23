//! Host connections: hosts.json, the SSH bridge, the local socket, the handshake, the host ID check and reconnecting. Nothing here imports Tauri.

pub mod attachments;
pub mod backoff;
pub mod config;
pub mod failure;
pub mod handshake;
pub mod identity;
pub mod link;
pub mod local;
pub mod model;
pub mod outbound;
pub mod registry;
pub mod service;
pub mod session;
pub mod ssh;
pub mod supervisor;
pub mod terminals;
pub mod text;

#[cfg(test)]
mod testing;
