//! Data catalog — metadata database for all ingested TVC files.
//!
//! Tracks instrument metadata, date ranges, and file locations.
//! Provides query API to find TVC files covering a given time range.
//!
//! # Layout
//! ```text
//! catalog.json
//! ├── version: "1.0"
//! └── entries: [CatalogEntry, ...]
//!
//! CatalogEntry:
//!   instrument_id: u32    (FNV-1a hash of symbol, uppercase)
//!   symbol: String        (e.g. "BTCUSDT")
//!   start_time_ns: u64   (first tick timestamp)
//!   end_time_ns: u64     (last tick timestamp)
//!   num_ticks: u64       (total tick count)
//!   file_path: String    (absolute or relative path)
//!   checksum: String      (hex SHA256 of file)
//! ```
//!
//! # Usage
//! ```ignore
//! let catalog = Catalog::load("catalog.json")?;
//! let files = catalog.query("BTCUSDT", start_ns, end_ns)?;
//! for entry in files {
//!     println!("{}", entry.file_path);
//! }
//! ```

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use tvc::reader::ReaderError;
use tvc::TvcReader;

/// Catalog metadata entry for a single TVC file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogEntry {
    /// FNV-1a hash of the instrument symbol (uppercase).
    pub instrument_id: u32,
    /// Human-readable symbol name.
    pub symbol: String,
    /// First tick timestamp in nanoseconds.
    pub start_time_ns: u64,
    /// Last tick timestamp in nanoseconds.
    pub end_time_ns: u64,
    /// Total number of ticks in the file.
    pub num_ticks: u64,
    /// Path to the TVC file.
    pub file_path: String,
    /// SHA256 checksum of the file (hex string).
    pub checksum: String,
    /// Ingestion timestamp in nanoseconds.
    #[serde(default)]
    pub created_at_ns: u64,
    /// Source adapter that ingested the file (e.g., "BinanceMarketDataAdapter").
    #[serde(default)]
    pub source_adapter: String,
    /// True if the file has been fully ingested; false if still being streamed.
    #[serde(default = "default_true")]
    pub is_complete: bool,
    /// (start_ns, end_ns) range for partial files being streamed.
    #[serde(default)]
    pub partial_range: Option<(u64, u64)>,
}

/// Default value for `is_complete` (true = full file ingested).
fn default_true() -> bool {
    true
}

impl CatalogEntry {
    /// Check if this entry's time range overlaps with [start, end].
    pub fn overlaps(&self, start: u64, end: u64) -> bool {
        self.start_time_ns <= end && self.end_time_ns >= start
    }

    /// Check if this entry is entirely within [start, end].
    pub fn contains(&self, start: u64, end: u64) -> bool {
        self.start_time_ns >= start && self.end_time_ns <= end
    }

    /// Validate the TVC file at `file_path` by opening it with `TvcReader`.
    ///
    /// This triggers SHA256 verification performed inside `TvcReader::open()`.
    pub fn validate_entry(&self) -> Result<(), CatalogError> {
        let path = Path::new(&self.file_path);
        match TvcReader::open(path) {
            Ok(_) => Ok(()),
            Err(ReaderError::Sha256Mismatch) => {
                Err(CatalogError::ChecksumMismatch(self.file_path.clone()))
            }
            Err(e) => Err(CatalogError::TvcOpenError(e.to_string())),
        }
    }

    /// Validate a complete entry for catalog loading purposes.
    /// Returns Ok(()) if the file doesn't exist yet (placeholder during streaming ingestion).
    /// Use `validate_entry()` for explicit user-facing validation that errors on missing files.
    fn validate_entry_for_load(&self) -> Result<(), CatalogError> {
        let path = Path::new(&self.file_path);
        if !path.exists() {
            return Ok(());
        }
        self.validate_entry()
    }
}

/// A data catalog for TVC files, indexed by instrument.
pub struct Catalog {
    /// Entries grouped by instrument_id.
    entries: BTreeMap<u32, Vec<CatalogEntry>>,
    /// Symbol names for each instrument_id.
    symbols: BTreeMap<u32, String>,
    /// Path to the catalog file on disk.
    path: PathBuf,
}

impl Catalog {
    /// Create a new empty catalog at the given path.
    pub fn new(path: &Path) -> Self {
        Self {
            entries: BTreeMap::new(),
            symbols: BTreeMap::new(),
            path: path.to_path_buf(),
        }
    }

    /// Load a catalog from a JSON file.
    pub fn load(path: &Path) -> Result<Self, CatalogError> {
        if !path.exists() {
            return Err(CatalogError::FileNotFound(path.display().to_string()));
        }

        let content = fs::read_to_string(path)?;
        let data: CatalogData = serde_json::from_str(&content).map_err(CatalogError::Json)?;

        let mut entries: BTreeMap<u32, Vec<CatalogEntry>> = BTreeMap::new();
        let mut symbols: BTreeMap<u32, String> = BTreeMap::new();

        for entry in data.entries {
            symbols.insert(entry.instrument_id, entry.symbol.clone());
            entries.entry(entry.instrument_id).or_default().push(entry);
        }

        // Sort each instrument's entries by start_time_ns
        for entries_vec in entries.values_mut() {
            entries_vec.sort_by_key(|e| e.start_time_ns);
        }

        // Validate all entries (eager checksum verification on load)
        let catalog = Self {
            entries,
            symbols,
            path: path.to_path_buf(),
        };
        catalog.validate_all_entries()?;
        Ok(catalog)
    }

    /// Validate all entries in the catalog, returning the first error found.
    /// Only validates entries where `is_complete == true` (incomplete entries
    /// may have placeholder paths during streaming ingestion).
    fn validate_all_entries(&self) -> Result<(), CatalogError> {
        for entries_vec in self.entries.values() {
            for entry in entries_vec {
                if entry.is_complete {
                    entry.validate_entry_for_load()?;
                }
            }
        }
        Ok(())
    }

    /// Save the catalog to its JSON file.
    pub fn save(&self) -> Result<(), CatalogError> {
        let mut all_entries = Vec::new();
        for entries_vec in self.entries.values() {
            for entry in entries_vec {
                all_entries.push(entry.clone());
            }
        }

        let data = CatalogData {
            version: "1.0".to_string(),
            entries: all_entries,
        };

        let content = serde_json::to_string_pretty(&data).map_err(CatalogError::Json)?;

        fs::write(&self.path, content)?;
        Ok(())
    }

    /// Add a new entry to the catalog, merging overlapping ranges.
    ///
    /// If an entry for the same instrument overlaps with existing entries,
    /// they will be merged into a single entry with combined time range.
    pub fn add_entry(&mut self, entry: CatalogEntry) {
        let id = entry.instrument_id;
        self.symbols.insert(id, entry.symbol.clone());

        let entries = self.entries.entry(id).or_default();

        // Add and sort
        entries.push(entry);
        entries.sort_by_key(|e| e.start_time_ns);

        // Merge overlapping entries
        let merged = Self::merge_entries(entries);
        *entries = merged;
    }

    /// Update the partial range for the latest entry of the given instrument.
    ///
    /// Sets `partial_range` to `(existing_start_ns, partial_end_ns)` where
    /// `existing_start_ns` is the `start_time_ns` of the most recent entry.
    /// Does nothing if there is no entry for this instrument.
    pub fn update_entry(&mut self, instrument_id: u32, partial_end_ns: u64) {
        let entries = match self.entries.get_mut(&instrument_id) {
            Some(e) if !e.is_empty() => e,
            _ => return,
        };
        let entry = entries.last_mut().expect("entries is non-empty");
        entry.partial_range = Some((entry.start_time_ns, partial_end_ns));
    }

    /// Finalize the latest entry for the given instrument.
    ///
    /// Sets `is_complete = true` and clears `partial_range`.
    /// Does nothing if there is no entry for this instrument.
    pub fn finalize_entry(&mut self, instrument_id: u32) {
        let entries = match self.entries.get_mut(&instrument_id) {
            Some(e) if !e.is_empty() => e,
            _ => return,
        };
        let entry = entries.last_mut().expect("entries is non-empty");
        entry.is_complete = true;
        entry.partial_range = None;
    }

    /// Merge a list of overlapping/adjacent entries into a single entry.
    fn merge_entries(entries: &[CatalogEntry]) -> Vec<CatalogEntry> {
        if entries.is_empty() {
            return Vec::new();
        }

        let mut result = Vec::new();
        let mut current = entries[0].clone();

        for entry in entries.iter().skip(1) {
            // Check if overlapping or adjacent (within 1ns tolerance)
            if entry.start_time_ns <= current.end_time_ns + 1 {
                // Merge: extend end time, sum ticks, keep first file (latest)
                current.end_time_ns = current.end_time_ns.max(entry.end_time_ns);
                current.num_ticks += entry.num_ticks;
                // Keep current.file_path (assume we want the most recent/relevant file)
            } else {
                result.push(current);
                current = entry.clone();
            }
        }
        result.push(current);
        result
    }

    /// Query for TVC files covering the given instrument and time range.
    ///
    /// Returns all catalog entries where:
    /// - instrument_id matches
    /// - entry overlaps with [start_ns, end_ns]
    ///
    /// Results are sorted by start_time_ns.
    pub fn query(&self, symbol: &str, start_ns: u64, end_ns: u64) -> Vec<CatalogEntry> {
        let upper = symbol.to_uppercase();
        let instrument_id = fnv1a_hash(upper.as_bytes());

        let Some(entries) = self.entries.get(&instrument_id) else {
            return Vec::new();
        };

        entries
            .iter()
            .filter(|e| e.overlaps(start_ns, end_ns))
            .cloned()
            .collect()
    }

    /// Query by instrument_id directly.
    pub fn query_by_id(&self, instrument_id: u32, start_ns: u64, end_ns: u64) -> Vec<CatalogEntry> {
        let Some(entries) = self.entries.get(&instrument_id) else {
            return Vec::new();
        };

        entries
            .iter()
            .filter(|e| e.overlaps(start_ns, end_ns))
            .cloned()
            .collect()
    }

    /// Get all entries for an instrument.
    pub fn get_instrument(&self, symbol: &str) -> Vec<CatalogEntry> {
        let instrument_id = fnv1a_hash(symbol.to_uppercase().as_bytes());
        self.entries
            .get(&instrument_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Get all instrument IDs in the catalog.
    pub fn instruments(&self) -> Vec<u32> {
        self.entries.keys().cloned().collect()
    }

    /// Get the total number of entries.
    pub fn len(&self) -> usize {
        self.entries.values().map(|v| v.len()).sum()
    }

    /// Check if the catalog is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Load all entries and verify each TVC file via `validate_entry()`.
    ///
    /// Returns the first `CatalogError` encountered, if any.
    pub fn load_with_verification(&mut self) -> Result<(), CatalogError> {
        for entries_vec in self.entries.values() {
            if let Some(entry) = entries_vec.last() {
                entry.validate_entry()?;
            }
        }
        Ok(())
    }

    /// Compute SHA256 checksum of a file.
    pub fn compute_checksum(path: &Path) -> Result<String, CatalogError> {
        let data = fs::read(path)?;
        let mut hasher = Sha256::new();
        hasher.update(&data);
        Ok(format!("{:x}", hasher.finalize()))
    }
}

/// FNV-1a hash of a byte string (32-bit).
fn fnv1a_hash(data: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for byte in data {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

/// On-disk format for the catalog JSON.
#[derive(Debug, Serialize, Deserialize)]
struct CatalogData {
    version: String,
    entries: Vec<CatalogEntry>,
}

/// Catalog errors.
#[derive(Debug)]
pub enum CatalogError {
    FileNotFound(String),
    Json(serde_json::Error),
    Io(std::io::Error),
    /// TVC file checksum does not match stored checksum.
    ChecksumMismatch(String),
    /// Failed to open TVC file.
    TvcOpenError(String),
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CatalogError::FileNotFound(s) => write!(f, "Catalog file not found: {}", s),
            CatalogError::Json(e) => write!(f, "JSON error: {}", e),
            CatalogError::Io(e) => write!(f, "IO error: {}", e),
            CatalogError::ChecksumMismatch(s) => write!(f, "Checksum mismatch: {}", s),
            CatalogError::TvcOpenError(s) => write!(f, "TVC open error: {}", s),
        }
    }
}

impl std::error::Error for CatalogError {}

impl From<std::io::Error> for CatalogError {
    fn from(e: std::io::Error) -> Self {
        CatalogError::Io(e)
    }
}

impl From<serde_json::Error> for CatalogError {
    fn from(e: serde_json::Error) -> Self {
        CatalogError::Json(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn make_entry(symbol: &str, start: u64, end: u64, num_ticks: u64, path: &str) -> CatalogEntry {
        let instrument_id = fnv1a_hash(symbol.to_uppercase().as_bytes());
        CatalogEntry {
            instrument_id,
            symbol: symbol.to_string(),
            start_time_ns: start,
            end_time_ns: end,
            num_ticks,
            file_path: path.to_string(),
            checksum: "abcd1234".to_string(),
            created_at_ns: 0,
            source_adapter: String::new(),
            is_complete: true,
            partial_range: None,
        }
    }

    #[test]
    fn test_merge_entries_non_overlapping() {
        let entries = vec![
            make_entry("BTCUSDT", 1000, 2000, 1000, "/a.tvc"),
            make_entry("BTCUSDT", 3000, 4000, 1000, "/b.tvc"),
        ];
        let merged = Catalog::merge_entries(&entries);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn test_merge_entries_overlapping() {
        let entries = vec![
            make_entry("BTCUSDT", 1000, 2000, 1000, "/a.tvc"),
            make_entry("BTCUSDT", 1500, 2500, 1000, "/b.tvc"),
        ];
        let merged = Catalog::merge_entries(&entries);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].start_time_ns, 1000);
        assert_eq!(merged[0].end_time_ns, 2500);
        assert_eq!(merged[0].num_ticks, 2000);
    }

    #[test]
    fn test_query_returns_matching_entries() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        drop(tmp);
        std::fs::remove_file(&path).ok();

        let mut catalog = Catalog::new(&path);
        catalog.add_entry(make_entry("BTCUSDT", 1000, 2000, 100, "/a.tvc"));
        catalog.add_entry(make_entry("BTCUSDT", 3000, 4000, 100, "/b.tvc"));
        catalog.add_entry(make_entry("ETHUSDT", 1000, 2000, 100, "/c.tvc"));
        catalog.save().unwrap();

        // Reload and query
        let loaded = Catalog::load(&path).unwrap();
        let results = loaded.query("BTCUSDT", 1500, 3500);
        assert_eq!(results.len(), 2); // Both overlap with [1500, 3500]
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        drop(tmp);
        std::fs::remove_file(&path).ok();

        let mut catalog = Catalog::new(&path);
        catalog.add_entry(make_entry("BTCUSDT", 1000, 5000, 1000, "/btc.tvc"));
        catalog.add_entry(make_entry("ETHUSDT", 2000, 6000, 500, "/eth.tvc"));
        catalog.save().unwrap();

        let loaded = Catalog::load(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        let btc = loaded.query("BTCUSDT", 0, u64::MAX);
        assert_eq!(btc.len(), 1);
        assert_eq!(btc[0].symbol, "BTCUSDT");
        assert_eq!(btc[0].num_ticks, 1000);
    }

    #[test]
    fn test_fnv1a_stable() {
        let id1 = fnv1a_hash(b"BTCUSDT");
        let id2 = fnv1a_hash(b"BTCUSDT");
        assert_eq!(id1, id2);
        assert_ne!(id1, 0);
    }

    #[test]
    fn test_entry_overlaps() {
        let entry = make_entry("BTCUSDT", 1000, 2000, 100, "/a.tvc");
        assert!(entry.overlaps(500, 1500)); // Partial overlap
        assert!(entry.overlaps(1000, 2000)); // Exact match
        assert!(entry.overlaps(1500, 2500)); // Partial overlap
        assert!(!entry.overlaps(0, 500)); // No overlap
        assert!(!entry.overlaps(3000, 4000)); // No overlap
    }

    #[test]
    fn test_validate_entry_nonexistent_file() {
        let entry = CatalogEntry {
            instrument_id: fnv1a_hash(b"BTCUSDT"),
            symbol: "BTCUSDT".to_string(),
            start_time_ns: 1000,
            end_time_ns: 2000,
            num_ticks: 100,
            file_path: "/nonexistent/path/to/file.tvc".to_string(),
            checksum: "abcd1234".to_string(),
            created_at_ns: 0,
            source_adapter: String::new(),
            is_complete: true,
            partial_range: None,
        };
        let result = entry.validate_entry();
        assert!(matches!(result, Err(CatalogError::TvcOpenError(_))));
    }

    #[test]
    fn test_streaming_ingestion_cycle() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        drop(tmp);
        std::fs::remove_file(&path).ok();

        let mut catalog = Catalog::new(&path);
        let instrument_id = fnv1a_hash(b"BTCUSDT");

        // Add entry as partial/streaming
        let entry = CatalogEntry {
            instrument_id,
            symbol: "BTCUSDT".to_string(),
            start_time_ns: 1000,
            end_time_ns: 1000,
            num_ticks: 0,
            file_path: "/dummy.tvc".to_string(),
            checksum: "abcd1234".to_string(),
            created_at_ns: 0,
            source_adapter: String::new(),
            is_complete: false,
            partial_range: Some((1000, 2000)),
        };
        catalog.add_entry(entry);

        // Simulate more bytes written: update partial range end
        catalog.update_entry(instrument_id, 3000);

        // Verify partial_range was updated to (1000, 3000)
        {
            let entries = catalog.entries.get(&instrument_id).unwrap();
            let latest = entries.last().unwrap();
            assert_eq!(latest.partial_range, Some((1000, 3000)));
            assert!(!latest.is_complete);
        }

        // Finalize the entry
        catalog.finalize_entry(instrument_id);

        // Verify finalized state
        {
            let entries = catalog.entries.get(&instrument_id).unwrap();
            let latest = entries.last().unwrap();
            assert!(latest.is_complete, "entry should be complete after finalize");
            assert!(
                latest.partial_range.is_none(),
                "partial_range should be None after finalize"
            );
        }
    }
}
