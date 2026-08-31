//! Catalog — tracks which TVC3 files exist locally.
//!
//! Scans the folder tree on startup (or on demand) to build an in-memory index.
//! Used by DataManager to detect gaps and decide whether to download.

use std::collections::HashMap;
use std::path::PathBuf;
use chrono::NaiveDate;
use super::types::{Exchange, Venue, TvcFile};

/// A catalog of available TVC3 files, indexed by (exchange, venue, symbol, date).
#[derive(Debug)]
pub struct Catalog {
    /// Index: (exchange, venue, symbol, date) → TvcFile
    index: HashMap<(Exchange, Venue, String, NaiveDate), TvcFile>,
    /// Root directory scanned
    root: PathBuf,
}

impl Catalog {
    /// Scan the data root directory and build the catalog.
    /// Expected structure: `{root}/{exchange}/{venue}/{symbol}/{date}.tvc`
    pub fn scan(root: PathBuf) -> std::io::Result<Self> {
        let mut index = HashMap::new();

        if !root.exists() {
            return Ok(Self { index, root });
        }

        for exchange_entry in std::fs::read_dir(&root)? {
            let exchange_entry = exchange_entry?;

            // Skip non-directories (flat TVC files, CSV files, etc.)
            if !exchange_entry.file_type()?.is_dir() {
                continue;
            }

            let exchange_name = exchange_entry.file_name().to_string_lossy().to_lowercase();
            let Some(exchange) = match_exchange(&exchange_name) else {
                // Skip unknown exchange dirs (e.g. TVCB flat structure: btcusdt/15m/)
                continue;
            };

            for venue_entry in std::fs::read_dir(exchange_entry.path())? {
                let venue_entry = venue_entry?;

                if !venue_entry.file_type()?.is_dir() {
                    continue;
                }

                let venue_name = venue_entry.file_name().to_string_lossy().to_lowercase();
                let venue = match_venue(&venue_name)
                    .ok_or_else(|| std::io::Error::other("invalid venue dir"))?;

                for symbol_entry in std::fs::read_dir(venue_entry.path())? {
                    let symbol_entry = symbol_entry?;

                    if !symbol_entry.file_type()?.is_dir() {
                        continue;
                    }

                    // Normalize to UPPERCASE — exchanges standardize on uppercase tickers,
                    // and writers may use different casings (e.g. bar_ingester lowercases,
                    // tvc_builder preserves caller's case). Catalog stores uppercase to
                    // make lookups consistent regardless of on-disk casing.
                    let symbol = symbol_entry.file_name().to_string_lossy().to_uppercase();
                    for date_entry in std::fs::read_dir(symbol_entry.path())? {
                        let date_entry = date_entry?;
                        let path = date_entry.path();
                        if path.extension().map_or(false, |e| e == "tvc") {
                            // Skip files that don't have the TVC3 magic — these are
                            // corrupt/zero-header files (typically from a writer bug)
                            // and would just waste time on read attempts.
                            if !is_valid_tvc_magic(&path) {
                                continue;
                            }
                            let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
                            if let Some(date) = parse_date_from_stem(&stem) {
                                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                                let tvc_file = TvcFile {
                                    exchange,
                                    venue,
                                    symbol: symbol.clone(),
                                    date,
                                    path: path.clone(),
                                    size_bytes: size,
                                };
                                index.insert((exchange, venue, symbol.clone(), date), tvc_file);
                            }
                        }
                    }
                }
            }
        }

        Ok(Self { index, root })
    }

    /// Check if a specific (exchange, venue, symbol, date) file exists.
    /// Symbol is normalized to UPPERCASE for consistent lookup.
    pub fn contains(&self, exchange: Exchange, venue: Venue, symbol: &str, date: NaiveDate) -> bool {
        self.index.contains_key(&(exchange, venue, symbol.to_uppercase(), date))
    }

    /// Get a specific file entry. Symbol is normalized to UPPERCASE.
    pub fn get(&self, exchange: Exchange, venue: Venue, symbol: &str, date: NaiveDate) -> Option<&TvcFile> {
        self.index.get(&(exchange, venue, symbol.to_uppercase(), date))
    }

    /// List all files for a given (exchange, venue, symbol). Symbol is normalized to UPPERCASE.
    pub fn files_for(&self, exchange: Exchange, venue: Venue, symbol: &str) -> Vec<&TvcFile> {
        let symbol_upper = symbol.to_uppercase();
        self.index
            .values()
            .filter(|f| f.exchange == exchange && f.venue == venue && f.symbol == symbol_upper)
            .collect()
    }

    /// List all files for a given (exchange, venue, symbol, date_range). Symbol normalized to UPPERCASE.
    pub fn files_in_range(
        &self,
        exchange: Exchange,
        venue: Venue,
        symbol: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Vec<&TvcFile> {
        let symbol_upper = symbol.to_uppercase();
        self.index
            .values()
            .filter(|f| {
                f.exchange == exchange
                    && f.venue == venue
                    && f.symbol == symbol_upper
                    && f.date >= start
                    && f.date <= end
            })
            .collect()
    }

    /// Return all missing dates in the given range for a (exchange, venue, symbol).
    pub fn missing_dates(
        &self,
        exchange: Exchange,
        venue: Venue,
        symbol: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Vec<NaiveDate> {
        let mut missing = Vec::new();
        let mut current = start;
        while current <= end {
            if !self.contains(exchange, venue, symbol, current) {
                missing.push(current);
            }
            current = current.succ_opt().unwrap_or(current);
        }
        missing
    }
}

fn match_exchange(name: &str) -> Option<Exchange> {
    match name {
        "binance" => Some(Exchange::Binance),
        "bybit" => Some(Exchange::Bybit),
        "okx" => Some(Exchange::Okx),
        "coinbase" => Some(Exchange::Coinbase),
        _ => None,
    }
}

fn match_venue(name: &str) -> Option<Venue> {
    match name {
        "spot" => Some(Venue::Spot),
        "futures" => Some(Venue::Futures),
        "linear" => Some(Venue::Linear),
        "swap" => Some(Venue::Swap),
        _ => None,
    }
}

/// Parse "YYYY-MM-DD" from a TVC3 filename stem (e.g. "BTCUSDT_2025-01-01" → "2025-01-01")
fn parse_date_from_stem(stem: &str) -> Option<NaiveDate> {
    // Filename format: "SYMBOL_YYYY-MM-DD" or just "YYYY-MM-DD"
    // Extract the last component that looks like a date
    let parts: Vec<&str> = stem.split('_').collect();
    for part in parts.iter().rev() {
        if let Ok(date) = NaiveDate::parse_from_str(part, "%Y-%m-%d") {
            return Some(date);
        }
    }
    None
}

/// Check whether the first 4 bytes of a TVC file are the TVC3 magic.
/// Used by the catalog to skip corrupt/zero-header files that would
/// just fail later at read time.
fn is_valid_tvc_magic(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut buf = [0u8; 4];
    match file.read(&mut buf) {
        Ok(4) => &buf == b"TVC3",
        _ => false,
    }
}