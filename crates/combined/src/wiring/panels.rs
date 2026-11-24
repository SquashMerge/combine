//! What the panels run on: the diagram check, the wall clock, the scratch area and the timing.

use std::sync::Arc;

use super::clock::SystemWallClock;
use crate::core::PanelTiming;
use crate::ports::{DiagramCheck, ScratchStore, WallClock};

/// The panel parts of [`crate::rpc::ServerParts`].
pub struct PanelParts {
    pub diagram_check: Arc<dyn DiagramCheck>,
    pub wall_clock: Arc<dyn WallClock>,
    pub scratch: Arc<dyn ScratchStore>,
    pub timing: PanelTiming,
}

/// The panel parts of a server nobody configured: fakes for the in-process test servers, and
/// the real check with no scratch area otherwise. `run_serve` passes its own.
#[cfg(feature = "test-support")]
pub fn default_parts() -> PanelParts {
    use crate::testing::{FakeDiagramCheck, MemoryScratchStore};

    PanelParts {
        diagram_check: Arc::new(FakeDiagramCheck::new()),
        wall_clock: Arc::new(SystemWallClock),
        scratch: Arc::new(MemoryScratchStore::new()),
        timing: PanelTiming::default(),
    }
}

/// The panel parts of a server nobody configured: the real check with no scratch area. Call
/// inside the tokio runtime.
#[cfg(not(feature = "test-support"))]
pub fn default_parts() -> PanelParts {
    use crate::adapters::loader::DiagramLimits;

    PanelParts {
        diagram_check: super::diagram::build(DiagramLimits::default()),
        wall_clock: Arc::new(SystemWallClock),
        scratch: Arc::new(NoScratch),
        timing: PanelTiming::default(),
    }
}

#[cfg(not(feature = "test-support"))]
use crate::ports::{ScratchDir, ScratchListing, StoreError, StoredFile, WriteOutcome};

/// A scratch store that holds nothing and refuses every write.
#[cfg(not(feature = "test-support"))]
struct NoScratch;

#[cfg(not(feature = "test-support"))]
#[async_trait::async_trait]
impl ScratchStore for NoScratch {
    async fn list(&self, _max_dirs: usize) -> Result<ScratchListing, StoreError> {
        Ok(ScratchListing {
            dirs: Vec::new(),
            truncated: false,
        })
    }

    async fn read(
        &self,
        _dir: &ScratchDir,
        _file: &str,
        _max_bytes: usize,
    ) -> Result<Option<StoredFile>, StoreError> {
        Ok(None)
    }

    async fn write(
        &self,
        _dir: &ScratchDir,
        _file: &str,
        _bytes: Vec<u8>,
    ) -> Result<WriteOutcome, StoreError> {
        Err(unavailable())
    }

    async fn remove(&self, _dir: &ScratchDir) -> Result<(), StoreError> {
        Err(unavailable())
    }
}

#[cfg(not(feature = "test-support"))]
fn unavailable() -> StoreError {
    StoreError::Unavailable("this daemon has no scratch area".into())
}
