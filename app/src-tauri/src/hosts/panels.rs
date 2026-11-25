//! Where a host's panel traffic goes besides the list: get_panel, close_panel and pin_panel replies, and refusals.

use combine_protocol::{ErrorCode, Panel, PanelClosed, PanelPinned, RequestId};

/// One thing a host's daemon said about a panel request, or the end of the session that could
/// still answer one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelReply {
    Panel(Panel),
    /// The daemon closed a panel.
    Closed(PanelClosed),
    /// The daemon kept a scratch panel in the project.
    Pinned(PanelPinned),
    /// The daemon refused a panel request. `message` is trimmed external text.
    Error {
        request: RequestId,
        code: ErrorCode,
        message: String,
    },
    /// The session ended, so replies to fetches still in flight will never come.
    Disconnected,
}
