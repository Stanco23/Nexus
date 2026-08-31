//! Converts raw exchange trade data into TVC3 format.
//!
//! TVC3 is the in-house tick storage format: memory-mapped, SHA256-validated,
//! anchor-indexed for fast seeking. `TvcBuilder` writes the binary format from
//! raw trade data (u64 timestamp, i64 price, i64 size in nano-integer representation).

use std::fs::OpenOptions;
use std::io::BufWriter;
use std::path::PathBuf;
use chrono::NaiveDate;
use tvc::{TradeTick, TvcWriter};
use crate::data_manager::types::{Exchange, Venue};
use crate::instrument::InstrumentId;
use super::downloader::RawTradeData;

/// Result of building a TVC3 file.
#[derive(Debug)]
pub struct BuildResult {
    pub path: PathBuf,
    pub num_trades: usize,
    pub size_bytes: u64,
}

/// Error during TVC3 construction.
#[derive(Debug)]
pub enum BuildError {
    Io(std::io::Error),
    TvcWrite(String),
    InvalidData(String),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Io(e) => write!(f, "IO error: {}", e),
            BuildError::TvcWrite(msg) => write!(f, "TVC write error: {}", msg),
            BuildError::InvalidData(msg) => write!(f, "Invalid data: {}", msg),
        }
    }
}

impl From<std::io::Error> for BuildError {
    fn from(e: std::io::Error) -> Self { BuildError::Io(e) }
}

/// Builder for TVC3 files from raw exchange trade data.
#[derive(Debug)]
pub struct TvcBuilder {
    output_dir: PathBuf,
    anchor_interval: u32,
    decimal_precision: u8,
}

impl TvcBuilder {
    pub fn new(output_dir: PathBuf) -> Self {
        Self {
            output_dir,
            anchor_interval: 1024, // TBC3 standard: full anchor every 1,024 ticks
            decimal_precision: 9,   // default: 9 decimal places (nano-integer)
        }
    }

    /// Set anchor interval (ticks between full anchor ticks).
    pub fn with_anchor_interval(mut self, interval: u32) -> Self {
        self.anchor_interval = interval;
        self
    }

    /// Set decimal precision.
    pub fn with_precision(mut self, precision: u8) -> Self {
        self.decimal_precision = precision;
        self
    }

    /// Build a TVC3 file from raw trade data.
    /// Writes to `{output_dir}/{exchange}/{venue}/{symbol}/{date}.tvc`
    pub fn build(&self, data: RawTradeData) -> Result<BuildResult, BuildError> {
        let path = self.output_dir
            .join(data.exchange.as_str())
            .join(data.venue.as_str())
            .join(&data.symbol)
            .join(format!("{}.tvc", data.date));

        // Create parent directories
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Instrument ID = FNV-1a hash of symbol.exchange
        let instrument_id = InstrumentId::new(&data.symbol, data.exchange.as_str());
        let id_hash = fnv1a_hash(instrument_id.to_string().as_bytes());

        // TvcWriter opens the file internally and handles its own write buffer.
        // We do NOT create a separate file handle + BufWriter here — that caused
        // a double-open bug where File::create in TvcWriter would truncate the file
        // before the BufWriter ever flushed anything, writing zeros in the header.
        let mut tvc = TvcWriter::new(&path, id_hash, self.anchor_interval, self.decimal_precision)
            .map_err(|e| BuildError::TvcWrite(e.to_string()))?;

        for (i, (timestamp_ns, price_int, size_int, side)) in data.trades.iter().enumerate() {
            let tick = TradeTick {
                timestamp_ns: *timestamp_ns,
                price_int: *price_int,
                size_int: *size_int,
                side: *side,    // aggressor side (0=buy, 1=sell) — was hardcoded to 0
                flags: 1,       // 1=trade
                sequence: i as u32,
            };
            tvc.write_tick(&tick)
                .map_err(|e| BuildError::TvcWrite(e.to_string()))?;
        }

        tvc.finalize()
            .map_err(|e| BuildError::TvcWrite(e.to_string()))?;

        let num_trades = data.trades.len();
        let size_bytes = path.metadata()?.len();

        Ok(BuildResult { path, num_trades, size_bytes })
    }

    /// Convenience: build TVC3 from raw trade data for a single exchange.
    pub fn build_from_trades(
        &self,
        exchange: Exchange,
        venue: Venue,
        symbol: &str,
        date: NaiveDate,
        trades: Vec<(u64, i64, i64, u8)>,
    ) -> Result<BuildResult, BuildError> {
        let data = RawTradeData {
            exchange,
            venue,
            symbol: symbol.to_string(),
            date,
            trades,
        };
        self.build(data)
    }
}

/// Compute a 32-bit FNV-1a hash.
fn fnv1a_hash(data: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for byte in data {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}