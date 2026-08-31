//! Data index for sweep-aware file loading.
//!
//! The index.json maps instruments to their TVC files with timestamp ranges,
//! enabling efficient date-range queries without scanning the filesystem.

use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// A single TVC file entry in the index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedFile {
    /// Relative path to the TVC file.
    pub path: String,
    /// First timestamp in the file (nanoseconds since epoch).
    pub start_ts: u64,
    /// Last timestamp in the file (nanoseconds since epoch).
    pub end_ts: u64,
    /// Number of ticks in the file.
    pub num_ticks: u64,
}

/// Files for a single instrument.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstrumentIndex {
    pub exchange: String,
    pub files: Vec<IndexedFile>,
}

/// The global data index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataIndex {
    /// Version marker for format compatibility.
    pub version: u32,
    /// Map from instrument symbol to instrument index.
    pub instruments: HashMap<String, InstrumentIndex>,
}

impl DataIndex {
    /// Load index from a JSON file.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let index: DataIndex = serde_json::from_str(&content)?;
        Ok(index)
    }

    /// Save index to a JSON file.
    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Find all files for an instrument that overlap with the given date range.
    ///
    /// Returns file paths sorted by start_ts.
    pub fn files_for_range(
        &self,
        symbol: &str,
        start_date: NaiveDate,
        end_date: NaiveDate,
    ) -> Vec<String> {
        let Some(instrument) = self.instruments.get(symbol) else {
            return Vec::new();
        };

        // Convert date range to timestamps (EST day boundary = UTC 05:00)
        // A tick belongs to EST date D if: UTC ts ∈ [midnight UTC of D-1, midnight UTC of D)
        // So we want files where end_ts >= start_of(EST start_date) AND start_ts < start_of(EST end_date + 1 day)
        let range_start = date_to_ts_utc(start_date);
        let range_end = date_to_ts_utc(end_date + chrono::Duration::days(1));

        let mut matching = Vec::new();
        for file in &instrument.files {
            // File overlaps if its time range intersects our query range
            if file.end_ts > range_start && file.start_ts < range_end {
                matching.push(file.path.clone());
            }
        }

        matching.sort_by_key(|p| {
            instrument
                .files
                .iter()
                .find(|f| f.path == *p)
                .map(|f| f.start_ts)
                .unwrap_or(0)
        });

        matching
    }
}

/// Convert a NaiveDate to UTC midnight timestamp (nanoseconds).
fn date_to_ts_utc(date: NaiveDate) -> u64 {
    use chrono::Datelike;
    let dt = chrono::NaiveDateTime::new(date, chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap());
    dt.and_utc().timestamp_nanos_opt().unwrap() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::naive_date_iso;

    #[test]
    fn test_files_for_range() {
        let mut index = DataIndex {
            version: 1,
            instruments: HashMap::new(),
        };

        // Jan 2 and Jan 3 files
        index.instruments.insert(
            "BTCUSDT".to_string(),
            InstrumentIndex {
                exchange: "BINANCE".to_string(),
                files: vec![
                    IndexedFile {
                        path: "BTCUSDT_2025-01-02.tvc".to_string(),
                        start_ts: 1735794059999000000, // 2025-01-02 05:00 UTC
                        end_ts: 1735880399999000000,   // 2025-01-03 04:59 UTC
                        num_ticks: 1440,
                    },
                    IndexedFile {
                        path: "BTCUSDT_2025-01-03.tvc".to_string(),
                        start_ts: 1735880400000000000, // 2025-01-03 05:00 UTC
                        end_ts: 1735966799999000000,   // 2025-01-04 04:59 UTC
                        num_ticks: 1440,
                    },
                ],
            },
        );

        let files = index.files_for_range("BTCUSDT", naive_date_iso!("2025-01-02"), naive_date_iso!("2025-01-02"));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], "BTCUSDT_2025-01-02.tvc");

        let files = index.files_for_range("BTCUSDT", naive_date_iso!("2025-01-01"), naive_date_iso!("2025-01-03"));
        assert_eq!(files.len(), 2);

        let files = index.files_for_range("BTCUSDT", naive_date_iso!("2025-01-05"), naive_date_iso!("2025-01-10"));
        assert!(files.is_empty());
    }
}