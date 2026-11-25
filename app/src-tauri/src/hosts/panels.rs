//! Where a host's panel traffic goes besides the list: get_panel replies and refusals.

use combine_protocol::{ErrorCode, Panel, RequestId};

/// One thing a host's daemon said about a panel fetch, or the end of the session that could
/// still answer one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelReply {
    Panel(Panel),
    /// The daemon refused a get_panel request. `message` is trimmed external text.
    Error {
        request: RequestId,
        code: ErrorCode,
        message: String,
    },
    /// The session ended, so replies to fetches still in flight will never come.
    Disconnected,
}
