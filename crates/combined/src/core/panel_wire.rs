//! A space's panels and one panel as `panels` and `panel` messages. Pure: it encodes only to measure
//! whether a list fits one frame.

use combine_protocol::{
    DaemonMessage, ErrorCode, ErrorMessage, Panel, PanelLifetime, PanelPinned, PanelState,
    PanelSummary, Panels, RequestId, SkippedPanel, WireMessage,
};

use crate::core::panel_store::{PanelApiError, PanelList, PanelView};
use crate::model::{Lifetime, SpaceId};

/// The `error` of a `panels` message for a space whose panels aren't read yet.
pub const PANELS_LOADING: &str = "the space's panels are loading";
/// The `reason` of a panel left out of a `panels` message so it fits one frame.
pub const TRIMMED_REASON: &str = "the panel list is larger than one frame; this panel isn't listed";
/// What a `panels` message's `error` says when skipped folders were left out to fit one frame.
pub const SKIPPED_TRIMMED: &str =
    "the panel list is larger than one frame; some skipped folders aren't listed";

/// The entry `panels` lists for `view`; `error` is set only while the panel is `failed`, and `lifetime` is
/// always the view's.
pub fn panel_summary(view: &PanelView) -> PanelSummary {
    PanelSummary {
        id: view.id.clone(),
        title: view.file.title.clone(),
        renderer: view.file.renderer,
        revision: view.revision,
        state: view.state,
        error: failed_error(view),
        created_by: view.file.created_by.clone(),
        lifetime: Some(match view.lifetime {
            Lifetime::Persistent => PanelLifetime::Persistent,
            Lifetime::Scratch => PanelLifetime::Scratch,
        }),
    }
}

/// The `panels` message for `list`: every panel that isn't `removed`, sorted by ID. A list too large for
/// one frame is trimmed, deterministically, to the largest ID-order prefix of panels that fits; the
/// panels left out move to `skipped` with [`TRIMMED_REASON`].
pub fn panels_message(request: Option<RequestId>, space: &SpaceId, list: PanelList) -> Panels {
    let mut views: Vec<&PanelView> = list
        .panels
        .iter()
        .filter(|view| view.state != PanelState::Removed)
        .collect();
    views.sort_by(|a, b| a.id.cmp(&b.id));
    let panels: Vec<PanelSummary> = views.into_iter().map(panel_summary).collect();
    let space = space.0.clone();
    let body =
        |panels: Vec<PanelSummary>, skipped: Vec<SkippedPanel>, error: Option<String>| Panels {
            request: Some(RequestId(u64::MAX)),
            space: space.clone(),
            panels,
            skipped,
            error,
        };

    let whole = body(panels.clone(), list.skipped.clone(), list.error.clone());
    if fits(&whole) {
        return Panels { request, ..whole };
    }

    let merged = |k: usize| merge_skipped(&list.skipped, &panels[k..]);
    let with_k = |k: usize| body(panels[..k].to_vec(), merged(k), list.error.clone());
    if let Some(k) = largest_fitting(panels.len(), |k| fits(&with_k(k))) {
        return Panels {
            request,
            ..with_k(k)
        };
    }

    let all = merged(0);
    let error = Some(match &list.error {
        Some(error) => format!("{error}; {SKIPPED_TRIMMED}"),
        None => SKIPPED_TRIMMED.to_owned(),
    });
    let with_m = |m: usize| body(Vec::new(), all[..m].to_vec(), error.clone());
    if let Some(m) = largest_fitting(all.len(), |m| fits(&with_m(m))) {
        return Panels {
            request,
            ..with_m(m)
        };
    }

    Panels {
        request,
        ..body(Vec::new(), Vec::new(), Some(SKIPPED_TRIMMED.to_owned()))
    }
}

/// The `panels` message for a space whose panels aren't read yet.
pub fn panels_loading(request: Option<RequestId>, space: &SpaceId) -> Panels {
    Panels {
        request,
        space: space.0.clone(),
        panels: Vec::new(),
        skipped: Vec::new(),
        error: Some(PANELS_LOADING.to_owned()),
    }
}

/// The `panel` reply for `view` with its `source`; `error` is set only while the panel is `failed`.
pub fn panel_reply(request: RequestId, view: &PanelView, source: &str) -> Panel {
    Panel {
        request,
        space: view.space.0.clone(),
        id: view.id.clone(),
        revision: view.revision,
        renderer: view.file.renderer,
        source: source.to_owned(),
        state: view.state,
        error: failed_error(view),
    }
}

/// The error code of a refused panel request. Every variant is named, so a new one is a compile
/// error here.
pub fn panel_error_code(error: &PanelApiError) -> ErrorCode {
    match error {
        PanelApiError::UnknownSpace => ErrorCode::UnknownSpace,
        PanelApiError::UnknownPanel { .. } => ErrorCode::UnknownPanel,
        PanelApiError::SpaceLoading
        | PanelApiError::Removed { .. }
        | PanelApiError::ReadOnly
        | PanelApiError::InvalidId { .. }
        | PanelApiError::IdTaken { .. }
        | PanelApiError::InvalidField { .. }
        | PanelApiError::PanelLimit { .. }
        | PanelApiError::TooLarge { .. }
        | PanelApiError::NotScratch { .. }
        | PanelApiError::ListingFailed { .. }
        | PanelApiError::ScratchListingFailed { .. }
        | PanelApiError::PanelFileInvalid { .. }
        | PanelApiError::Unreadable { .. }
        | PanelApiError::Stopped => ErrorCode::InvalidRequest,
    }
}

/// What a `pin_panel` answer says when the pin finished with the panel still a scratch panel and no
/// reason recorded.
pub const PIN_UNDONE: &str = "the pin was undone; the panel is still a scratch panel";

/// The answer to a `pin_panel`, from the panel as the store holds it once the pin's file work has run.
/// A panel that is persistent now was pinned; one that is still scratch had its pin undone, and
/// says why when the store recorded a reason.
pub fn pin_answer(
    request: RequestId,
    space: &SpaceId,
    panel: &str,
    found: Result<PanelView, PanelApiError>,
) -> DaemonMessage {
    let refused = |code, message| {
        DaemonMessage::Error(ErrorMessage {
            code,
            message,
            request: Some(request),
            attachment: None,
        })
    };
    match found {
        Ok(view) if view.state == PanelState::Removed => refused(
            ErrorCode::InvalidRequest,
            PanelApiError::Removed { id: view.id }.to_string(),
        ),
        Ok(view) if view.lifetime == Lifetime::Persistent => {
            DaemonMessage::PanelPinned(PanelPinned {
                request,
                space: space.0.clone(),
                panel: panel.to_owned(),
            })
        }
        Ok(view) => refused(
            ErrorCode::InvalidRequest,
            view.file_error.unwrap_or_else(|| PIN_UNDONE.to_owned()),
        ),
        Err(error) => refused(panel_error_code(&error), error.to_string()),
    }
}

fn failed_error(view: &PanelView) -> Option<combine_protocol::PanelError> {
    match view.state {
        PanelState::Failed => view.error.clone(),
        _ => None,
    }
}

fn fits(body: &Panels) -> bool {
    DaemonMessage::Panels(body.clone()).encode().is_ok()
}

/// The largest `n` in `0..n_max` for which `fits` holds, or `None` if not even 0 fits. `n_max`
/// itself is known not to fit.
fn largest_fitting(n_max: usize, fits: impl Fn(usize) -> bool) -> Option<usize> {
    if n_max == 0 || !fits(0) {
        return None;
    }
    let (mut lo, mut hi) = (0, n_max - 1);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Some(lo)
}

/// The folders that offer no panel merged, by `panel` in byte order, with the panels left out. A left-out
/// panel goes before a folder with the same ID.
fn merge_skipped(skipped: &[SkippedPanel], trimmed: &[PanelSummary]) -> Vec<SkippedPanel> {
    let mut merged = Vec::with_capacity(skipped.len() + trimmed.len());
    let (mut s, mut t) = (0, 0);
    while s < skipped.len() || t < trimmed.len() {
        let take_trimmed = match (skipped.get(s), trimmed.get(t)) {
            (Some(folder), Some(panel)) => panel.id <= folder.panel,
            (None, Some(_)) => true,
            _ => false,
        };
        if take_trimmed {
            merged.push(SkippedPanel {
                panel: trimmed[t].id.clone(),
                reason: TRIMMED_REASON.to_owned(),
            });
            t += 1;
        } else {
            merged.push(skipped[s].clone());
            s += 1;
        }
    }
    merged
}
