//! Binance trade file ingestion
//! ==============================
//! Parses Binance Data Archive CSV files (`data.binance.vision`) and writes
//! ticks to TVC3 format.
//!
//! Supports two CSV formats:
//! 1. **Binance Data Archive** (`id,price,qty,quote_qty,time,is_buyer_maker,is_self_trade`)
//!    — `time` is **nanoseconds**
//! 2. **Generic trade CSV** (`timestamp,price,quantity,side`)

use std::path::{Path, PathBuf};

use tvc::{TradeTick, TvcWriter};

use crate::instrument::fnv1a_hash;

// ─────────────────────────────────────────────────────────────────────────────
// Binance Data Archive format
// ─────────────────────────────────────────────────────────────────────────────

/// A single parsed Binance trade row (Binance Data Archive CSV format).
#[derive(Debug, Clone)]
pub struct BinanceTradeRow {
    pub trade_id: u64,
    pub price: f64,
    pub quantity: f64,
    /// Timestamp in **microseconds** (Binance Data Archive format).
    pub time_us: u64,
    /// True if buyer was maker → aggressor is SELL.
    pub is_buyer_maker: bool,
}

impl BinanceTradeRow {
    /// Parse a record from Binance Data Archive CSV.
    /// Format: `id,price,qty,quote_qty,time,is_buyer_maker`
    pub fn parse_from_record(record: &csv::StringRecord) -> Option<Self> {
        if record.len() < 7 {
            return None;
        }
        let trade_id: u64 = record.get(0)?.parse().ok()?;
        let price: f64 = record.get(1)?.parse().ok()?;
        let quantity: f64 = record.get(2)?.parse().ok()?;
        // Column 3 = quote_qty (ignored)
        let time_us: u64 = record.get(4)?.parse().ok()?;
        // "True"/"False" from Python CSV — Rust bool::parse needs lowercase
        let is_buyer_maker: bool = match record.get(5)?.trim() {
            "True" | "true" => true,
            "False" | "false" => false,
            _ => return None,
        };
        Some(Self {
            trade_id,
            price,
            quantity,
            time_us,
            is_buyer_maker,
        })
    }

    /// Convert to a `TradeTick` with nanosecond timestamp and nano-integer fields.
    pub fn to_trade_tick(&self, sequence: u32, precision: u8) -> TradeTick {
        // Binance Data Archive uses microseconds; convert to nanoseconds for TVC3
        let price_int = (self.price * 10f64.powi(precision as i32)).round() as i64;
        let size_int = (self.quantity * 10f64.powi(precision as i32)).round() as i64;
        let side = if self.is_buyer_maker { 1u8 } else { 0u8 };
        TradeTick::new(self.time_us * 1000, price_int, size_int, side, 1, sequence)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Generic trade CSV format (timestamp_ns, price, quantity, side)
// ─────────────────────────────────────────────────────────────────────────────

/// A single parsed generic trade CSV row.
#[derive(Debug, Clone)]
pub struct GenericTradeRow {
    /// Timestamp in **nanoseconds**.
    pub timestamp_ns: u64,
    pub price: f64,
    pub quantity: f64,
    /// "BUY" or "SELL"
    pub side: String,
}

impl GenericTradeRow {
    /// Parse a record from generic trade CSV.
    /// Format: `timestamp,price,quantity,side`
    pub fn parse_from_record(record: &csv::StringRecord) -> Option<Self> {
        if record.len() < 4 {
            return None;
        }
        let timestamp_ns: u64 = record.get(0)?.parse().ok()?;
        let price: f64 = record.get(1)?.parse().ok()?;
        let quantity: f64 = record.get(2)?.parse().ok()?;
        let side = record.get(3)?.to_string();
        Some(Self {
            timestamp_ns,
            price,
            quantity,
            side,
        })
    }

    /// Convert to a `TradeTick`.
    pub fn to_trade_tick(&self, sequence: u32, precision: u8) -> TradeTick {
        let price_int = (self.price * 10f64.powi(precision as i32)).round() as i64;
        let size_int = (self.quantity * 10f64.powi(precision as i32)).round() as i64;
        let side = if self.side.trim().to_uppercase() == "BUY" { 0u8 } else { 1u8 };
        TradeTick::new(self.timestamp_ns, price_int, size_int, side, 1, sequence)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Binance file ingestor
// ─────────────────────────────────────────────────────────────────────────────

/// Binance file ingestor — reads CSV from `data.binance.vision` format
/// and writes to TVC3.
pub struct BinanceFileIngestor {
    symbol: String,
    precision: u8,
    anchor_interval: u32,
}

impl BinanceFileIngestor {
    pub fn new(symbol: &str) -> Self {
        Self {
            symbol: symbol.to_uppercase(),
            precision: 16,
            anchor_interval: 100,
        }
    }

    pub fn with_precision(mut self, precision: u8) -> Self {
        self.precision = precision;
        self
    }

    pub fn with_anchor_interval(mut self, interval: u32) -> Self {
        self.anchor_interval = interval;
        self
    }

    pub fn ingest_file(
        &self,
        csv_path: &Path,
        output_path: &Path,
    ) -> Result<IngestResult, IngestError> {
        let instrument_id = fnv1a_hash(format!("{}.BINANCE", self.symbol).as_bytes());
        let mut writer =
            TvcWriter::new(output_path, instrument_id, self.anchor_interval, self.precision)
                .map_err(|e| IngestError::TvcWrite(e.to_string()))?;

        let mut rdr = csv::Reader::from_path(csv_path).map_err(|e| {
            IngestError::FileOpen(csv_path.display().to_string(), e.to_string())
        })?;

        // No header row in Binance Data Archive CSVs — first row is data.
        let mut sequence = 0u32;
        let mut count = 0u64;

        for result in rdr.records() {
            let record = match result {
                Ok(r) => r,
                Err(_) => continue,
            };
            if let Some(row) = BinanceTradeRow::parse_from_record(&record) {
                let tick = row.to_trade_tick(sequence, self.precision);
                writer
                    .write_tick(&tick)
                    .map_err(|e| IngestError::TvcWrite(e.to_string()))?;
                sequence = sequence.wrapping_add(1);
                count += 1;
            }
        }

        let hash = writer
            .finalize()
            .map_err(|e| IngestError::TvcWrite(e.to_string()))?;

        Ok(IngestResult { count, hash })
    }

    pub fn ingest_directory(
        &self,
        input_dir: &Path,
        output_dir: &Path,
    ) -> Result<Vec<PathBuf>, IngestError> {
        std::fs::create_dir_all(output_dir).map_err(|e| {
            IngestError::FileOpen(output_dir.display().to_string(), e.to_string())
        })?;

        let mut files: Vec<PathBuf> = std::fs::read_dir(input_dir)
            .map_err(|e| {
                IngestError::FileOpen(input_dir.display().to_string(), e.to_string())
            })?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |e| e == "csv"))
            .filter(|p| {
                p.file_name()
                    .map_or(false, |n| n.to_string_lossy().contains(&self.symbol))
            })
            .collect();

        files.sort();

        let mut outputs = Vec::new();

        for csv_path in &files {
            let date = csv_path
                .file_stem()
                .and_then(|s| {
                    let stem = s.to_string_lossy();
                    stem.chars()
                        .filter(|c| c.is_ascii_digit() || *c == '-')
                        .collect::<String>()
                        .strip_prefix('-')
                        .map(String::from)
                })
                .unwrap_or_else(|| "unknown".to_string());

            let output_name = format!("{}_{}.tvc", self.symbol, date);
            let output_path = output_dir.join(&output_name);

            print!("  {} → {} ... ", csv_path.display(), output_name);
            let result = self.ingest_file(csv_path, &output_path)?;
            println!("{} ticks ✓", result.count);
            outputs.push(output_path);
        }

        Ok(outputs)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Generic CSV ingestor (handles bench_trades.csv and similar)
// ─────────────────────────────────────────────────────────────────────────────

/// Generic trade CSV ingestor — handles any CSV with
/// `timestamp,price,quantity,side` columns (nanoseconds, not milliseconds).
pub struct GenericCsvIngestor {
    symbol: String,
    precision: u8,
    anchor_interval: u32,
}

impl GenericCsvIngestor {
    pub fn new(symbol: &str) -> Self {
        Self {
            symbol: symbol.to_uppercase(),
            precision: 9,
            anchor_interval: 100,
        }
    }

    pub fn with_precision(mut self, precision: u8) -> Self {
        self.precision = precision;
        self
    }

    pub fn with_anchor_interval(mut self, interval: u32) -> Self {
        self.anchor_interval = interval;
        self
    }

    /// Ingest a single generic trade CSV file to TVC3.
    pub fn ingest_file(
        &self,
        csv_path: &Path,
        output_path: &Path,
    ) -> Result<IngestResult, IngestError> {
        let instrument_id = fnv1a_hash(format!("{}.BINANCE", self.symbol).as_bytes());
        let mut writer =
            TvcWriter::new(output_path, instrument_id, self.anchor_interval, self.precision)
                .map_err(|e| IngestError::TvcWrite(e.to_string()))?;

        let mut rdr = csv::Reader::from_path(csv_path).map_err(|e| {
            IngestError::FileOpen(csv_path.display().to_string(), e.to_string())
        })?;

        let _ = rdr.headers();

        let mut sequence = 0u32;
        let mut count = 0u64;

        for result in rdr.records() {
            let record = match result {
                Ok(r) => r,
                Err(_) => continue,
            };
            if let Some(row) = GenericTradeRow::parse_from_record(&record) {
                let tick = row.to_trade_tick(sequence, self.precision);
                writer
                    .write_tick(&tick)
                    .map_err(|e| IngestError::TvcWrite(e.to_string()))?;
                sequence = sequence.wrapping_add(1);
                count += 1;
            }
        }

        let hash = writer
            .finalize()
            .map_err(|e| IngestError::TvcWrite(e.to_string()))?;

        Ok(IngestResult { count, hash })
    }

    /// Ingest all CSV files from a directory.
    pub fn ingest_directory(
        &self,
        input_dir: &Path,
        output_dir: &Path,
    ) -> Result<Vec<PathBuf>, IngestError> {
        std::fs::create_dir_all(output_dir).map_err(|e| {
            IngestError::FileOpen(output_dir.display().to_string(), e.to_string())
        })?;

        let mut files: Vec<PathBuf> = std::fs::read_dir(input_dir)
            .map_err(|e| {
                IngestError::FileOpen(input_dir.display().to_string(), e.to_string())
            })?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |e| e == "csv"))
            .collect();

        files.sort();

        let mut outputs = Vec::new();

        for csv_path in &files {
            let output_name = format!(
                "{}.tvc",
                csv_path.file_stem().map_or("unknown".to_string(), |s| {
                    s.to_string_lossy().replace('/', "_").replace('\\', "_")
                })
            );
            let output_path = output_dir.join(&output_name);

            print!("  {} → {} ... ", csv_path.display(), output_name);
            let result = self.ingest_file(csv_path, &output_path)?;
            println!("{} ticks ✓", result.count);
            outputs.push(output_path);
        }

        Ok(outputs)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// Result of ingesting one file.
#[derive(Debug)]
pub struct IngestResult {
    pub count: u64,
    pub hash: [u8; 32],
}

/// Errors during ingestion.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("Cannot open file '{0}': {1}")]
    FileOpen(String, String),
    #[error("TVC write error: {0}")]
    TvcWrite(String),
}
