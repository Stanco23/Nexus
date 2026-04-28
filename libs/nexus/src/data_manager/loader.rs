//! Loads TVC3 files into `TickBufferSet` for backtesting.
//!
//! `DataLoader` handles loading one or more TVC3 files into a `TickBufferSet`
//! for use with `MergeCursor`.
//!
//! Supports:
//! - Single date: load one TVC3 file → TickBufferSet
//! - Date range: load multiple TVC3 files → sorted merge → TickBufferSet

use std::path::PathBuf;
use crate::buffer::{TickBufferSet, RingBufferError};
use crate::instrument::InstrumentId;
use super::types::{Exchange, Venue};

/// Error loading TVC3 data.
#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    TvcFormat(String),
    NoData,
}

impl From<std::io::Error> for LoadError {
    fn from(e: std::io::Error) -> Self { LoadError::Io(e) }
}

impl From<RingBufferError> for LoadError {
    fn from(e: RingBufferError) -> Self { LoadError::TvcFormat(e.to_string()) }
}

/// Loads TVC3 files into Nexus data structures.
#[derive(Debug)]
pub struct DataLoader;

impl DataLoader {
    /// Load a single TVC3 file into a TickBufferSet.
    pub fn load_file(path: &PathBuf, instrument_id: InstrumentId) -> Result<TickBufferSet, LoadError> {
        TickBufferSet::from_files([(path.clone(), instrument_id)])
            .map_err(|e| e.into())
    }

    /// Load multiple TVC3 files for the same symbol across a date range,
    /// merged into a single TickBufferSet.
    ///
    /// All files share the same (exchange, venue, symbol).
    pub fn load_range(
        paths: &[PathBuf],
        exchange: Exchange,
        venue: Venue,
        symbol: &str,
    ) -> Result<TickBufferSet, LoadError> {
        if paths.is_empty() {
            return Err(LoadError::NoData);
        }

        let instrument_id = InstrumentId::new(symbol, exchange.as_str());
        let files: Vec<(PathBuf, InstrumentId)> = paths.iter()
            .map(|p| (p.clone(), instrument_id.clone()))
            .collect();

        TickBufferSet::from_files(files)
            .map_err(|e| e.into())
    }
}