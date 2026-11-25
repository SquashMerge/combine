//! Which space an MCP caller belongs to.

use combine_protocol::McpCaller;

use super::terminal_list::TerminalList;
use crate::model::SpaceId;
use crate::ports::CallerIdentity;

/// What put the caller in its space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerMatch {
    Terminal,
    Cwd,
}

/// The space an MCP caller resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCaller {
    pub space: SpaceId,
    /// The caller's terminal slot: always `None` until terminal slots exist (M2-S8).
    pub slot: Option<String>,
    pub by: CallerMatch,
}

/// The caller's terminal when the backend's identity accepts it and the terminal is listed,
/// else the nearest listed space holding the caller's working directory, else `None`. Empty
/// strings count as absent.
pub fn resolve_caller(
    caller: &McpCaller,
    identity: Option<&dyn CallerIdentity>,
    terminals: &TerminalList,
) -> Option<ResolvedCaller> {
    if let Some(space) = terminal_space(caller, identity, terminals) {
        return Some(ResolvedCaller {
            space,
            slot: None,
            by: CallerMatch::Terminal,
        });
    }
    let cwd = caller.cwd.as_deref().filter(|cwd| !cwd.is_empty())?;
    terminals.space_for_cwd(cwd).map(|space| ResolvedCaller {
        space,
        slot: None,
        by: CallerMatch::Cwd,
    })
}

fn terminal_space(
    caller: &McpCaller,
    identity: Option<&dyn CallerIdentity>,
    terminals: &TerminalList,
) -> Option<SpaceId> {
    let id = caller.terminal_id.as_deref().filter(|id| !id.is_empty())?;
    let socket = caller
        .terminal_socket
        .as_deref()
        .filter(|socket| !socket.is_empty())?;
    let term = identity?.terminal(id, socket)?;
    terminals.space_of_terminal(&term)
}
