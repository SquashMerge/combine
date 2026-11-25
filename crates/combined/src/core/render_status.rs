//! What each client connection last reported drawing, per panel (D19). In memory, per connection.

use std::collections::BTreeMap;

use combine_protocol::{PanelError, RenderStatus};

use crate::core::effects::ConnectionId;
use crate::core::panel_file::{clip_text, PANEL_ERROR_TEXT_MAX};
use crate::model::SpaceId;

/// The panels one connection keeps a render status for. A new one beyond this evicts the
/// connection's least recently recorded.
pub const RENDER_STATUS_MAX_PER_CONNECTION: usize = 1_024;
/// The longest `code` a reported render error keeps, in bytes.
pub const RENDER_ERROR_CODE_MAX: usize = 64;

/// One client's report for one panel revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRender {
    pub revision: u64,
    pub status: RenderStatus,
    pub error: Option<PanelError>,
}

type Key = (SpaceId, String);

#[derive(Debug, Default)]
struct ConnRenders {
    entries: BTreeMap<Key, (u64, ClientRender)>,
    by_seq: BTreeMap<u64, Key>,
    next_seq: u64,
}

/// The render statuses of every connection.
#[derive(Debug, Default)]
pub struct RenderStatuses {
    conns: BTreeMap<ConnectionId, ConnRenders>,
}

impl RenderStatuses {
    pub fn new() -> RenderStatuses {
        RenderStatuses::default()
    }

    /// Keeps `report` unless its revision is above `current` or below this connection's last report for
    /// the panel; an equal revision replaces it. `rendered` keeps no error; `render_failed`'s `code` is cut
    /// to 64 bytes and `message`/`snippet` to PANEL_ERROR_TEXT_MAX (clip_text). A new entry beyond 1,024
    /// evicts the connection's least recently recorded one. Returns whether it was kept.
    pub fn record(
        &mut self,
        conn: ConnectionId,
        space: &SpaceId,
        panel: &str,
        report: ClientRender,
        current: u64,
    ) -> bool {
        if report.revision > current {
            return false;
        }
        let key = (space.clone(), panel.to_owned());
        let renders = self.conns.entry(conn).or_default();
        let old_seq = match renders.entries.get(&key) {
            Some((seq, last)) => {
                if report.revision < last.revision {
                    return false;
                }
                Some(*seq)
            }
            None => None,
        };
        match old_seq {
            Some(seq) => {
                renders.by_seq.remove(&seq);
            }
            None if renders.entries.len() >= RENDER_STATUS_MAX_PER_CONNECTION => {
                if let Some((_, oldest)) = renders.by_seq.pop_first() {
                    renders.entries.remove(&oldest);
                }
            }
            None => {}
        }
        let seq = renders.next_seq;
        renders.next_seq = renders.next_seq.saturating_add(1);
        renders.by_seq.insert(seq, key.clone());
        renders.entries.insert(key, (seq, clipped(report)));
        true
    }

    /// Forgets everything `conn` reported.
    pub fn disconnect(&mut self, conn: ConnectionId) {
        self.conns.remove(&conn);
    }

    /// One entry per connection that reported the panel, in ConnectionId order; no IDs exposed.
    pub fn render_status(&self, space: &SpaceId, panel: &str) -> Vec<ClientRender> {
        let key = (space.clone(), panel.to_owned());
        self.conns
            .values()
            .filter_map(|renders| renders.entries.get(&key).map(|(_, render)| render.clone()))
            .collect()
    }

    /// How many panels `conn` has a status for.
    pub fn len_of(&self, conn: ConnectionId) -> usize {
        self.conns
            .get(&conn)
            .map_or(0, |renders| renders.entries.len())
    }
}

fn clipped(report: ClientRender) -> ClientRender {
    let error = match report.status {
        RenderStatus::Rendered => None,
        RenderStatus::RenderFailed => report.error.map(|error| PanelError {
            code: clip_text(&error.code, RENDER_ERROR_CODE_MAX).to_owned(),
            message: clip_text(&error.message, PANEL_ERROR_TEXT_MAX).to_owned(),
            line: error.line,
            column: error.column,
            snippet: error
                .snippet
                .map(|snippet| clip_text(&snippet, PANEL_ERROR_TEXT_MAX).to_owned()),
        }),
    };
    ClientRender { error, ..report }
}
