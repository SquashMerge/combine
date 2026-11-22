//! Attachment-ID reservation: IDs are issued only below a ceiling that is already on disk.

use std::io;
use std::path::{Path, PathBuf};

use super::StartError;
use crate::adapters::fs::ids_file::{self, IdsFile};

/// IDs reserved at a time.
pub const ID_BLOCK: u64 = 1 << 20;

#[derive(Debug)]
pub struct IdReservation {
    path: PathBuf,
    reserved_through: u64,
}

impl IdReservation {
    /// Reads the last incarnation's ceiling, writes one block above it and returns the first ID
    /// this incarnation may issue.
    pub fn start(path: &Path) -> Result<(IdReservation, u64), StartError> {
        let previous = ids_file::load(path)
            .map_err(|error| StartError::Data(error.to_string()))?
            .map_or(0, |file| file.ids_reserved_through);
        let overflow =
            || StartError::Data(format!("{} is at the end of the ID range", path.display()));
        let id_base = previous.checked_add(1).ok_or_else(overflow)?;
        let reserved_through = previous.checked_add(ID_BLOCK).ok_or_else(overflow)?;
        ids_file::store(path, &IdsFile::new(reserved_through)).map_err(|error| {
            StartError::Runtime(format!("cannot write {}: {error}", path.display()))
        })?;
        let reservation = IdReservation {
            path: path.to_path_buf(),
            reserved_through,
        };
        Ok((reservation, id_base))
    }

    pub fn reserved_through(&self) -> u64 {
        self.reserved_through
    }

    /// Reserves another block when fewer than half a block remain above `high_water`.
    pub fn keep_ahead(&mut self, high_water: u64) -> io::Result<()> {
        if high_water.saturating_add(ID_BLOCK / 2) < self.reserved_through {
            return Ok(());
        }
        let next = self
            .reserved_through
            .checked_add(ID_BLOCK)
            .ok_or_else(|| io::Error::other("attachment ID range is exhausted"))?;
        ids_file::store(&self.path, &IdsFile::new(next))?;
        self.reserved_through = next;
        Ok(())
    }
}
