//! The panel API as the tools call it.

use crate::core::{PanelApiError, PanelCreate, PanelList, PanelUpdate, PanelView};

/// One space's panel API as the tools call it. Each call blocks until the server loop answers.
pub trait PanelApi {
    fn create(&self, create: PanelCreate) -> Result<PanelView, PanelApiError>;
    fn update(&self, id: &str, update: PanelUpdate) -> Result<PanelView, PanelApiError>;
    fn remove(&self, id: &str) -> Result<PanelView, PanelApiError>;
    fn status(&self, id: &str) -> Result<PanelView, PanelApiError>;
    fn list(&self) -> Result<PanelList, PanelApiError>;
}
