//! The `combine` command line: the `combine mcp` shim that relays an agent's stdio JSON-RPC to
//! the Combine daemon's MCP channel.
#![forbid(unsafe_code)]

pub mod cli;
pub mod relay;
pub mod socket;
