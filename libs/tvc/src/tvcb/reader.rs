//! TVCB reader — memory-mapped reading with random access.
//!
//! # Layout
//! ```text
//! [128-byte header]
//! [anchor bar 72B][delta bar ...][anchor bar 72B][delta bar...]...
//!                                                           ^ index at EOF
//! [index: 16B * num_anchors]
//! [32B SHA256 digest]
//! ```

use memmap2::Mmap;
use std::fs::File;
use std::path::{Path, PathBuf};

use crate::tvcb::encoding::{decode_delta_bar, DecodeError};
use crate::tvcb::types::{
    bytes_to_anchor_bar,
    TvcbHeader, HEADER_SIZE, ANCHOR_BAR_SIZE, INDEX_ENTRY_SIZE,
    Bar, AnchorBar, IndexEntry,
};
use crate::tvcb::types::TvcbError;

#[derive(Debug)]
pub enum ReaderError {
    Io(std::io::Error),
    InvalidMagic([u8; 4]),
    UnsupportedVersion(u8),
    InvalidIndexOffset,
    IndexOutOfBounds,
    NoAnchors,
    BarNotFound,
    Sha256Mismatch,
    Decode(DecodeError),
}

impl std::fmt::Display for ReaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReaderError::Io(e) => write!(f, "IO error: {}", e),
            ReaderError::InvalidMagic(m) => write!(f, "Invalid TVCB magic: {:?}", m),
            ReaderError::UnsupportedVersion(v) => write!(f, "Unsupported TVCB version: {}", v),
            ReaderError::InvalidIndexOffset => write!(f, "Invalid index offset in header"),
            ReaderError::IndexOutOfBounds => write!(f, "Index extends beyond file"),
            ReaderError::NoAnchors => write!(f, "No anchors in file"),
            ReaderError::BarNotFound => write!(f, "Bar not found in index"),
            ReaderError::Sha256Mismatch => write!(f, "SHA256 digest mismatch"),
            ReaderError::Decode(e) => write!(f, "Decode error: {}", e),
        }
    }
}

impl std::error::Error for ReaderError {}

impl From<std::io::Error> for ReaderError {
    fn from(e: std::io::Error) -> Self {
        ReaderError::Io(e)
    }
}

impl From<DecodeError> for ReaderError {
    fn from(e: DecodeError) -> Self {
        ReaderError::Decode(e)
    }
}

impl From<ReaderError> for TvcbError {
    fn from(e: ReaderError) -> Self {
        match e {
            ReaderError::Io(e) => TvcbError::Io(e.to_string()),
            ReaderError::InvalidMagic(m) => TvcbError::InvalidMagic(m),
            ReaderError::UnsupportedVersion(v) => TvcbError::UnsupportedVersion(v),
            ReaderError::InvalidIndexOffset => TvcbError::InvalidIndexOffset,
            ReaderError::IndexOutOfBounds => TvcbError::Io("Index out of bounds".to_string()),
            ReaderError::NoAnchors => TvcbError::NoAnchors,
            ReaderError::BarNotFound => TvcbError::BarNotFound,
            ReaderError::Sha256Mismatch => TvcbError::Sha256Mismatch,
            ReaderError::Decode(_) => TvcbError::InvalidDeltaEncoding,
        }
    }
}

// =============================================================================
// TvcbReader
// =============================================================================

/// TVCB file reader with memory-mapped access and binary search.
///
/// Memory-maps the file for efficient random access. The SHA256 digest
/// is stored at EOF (last 32 bytes) and covers the entire file up to
/// but not including the digest.
pub struct TvcbReader {
    /// Memory-mapped file data.
    mmap: Mmap,
    /// File header.
    header: TvcbHeader,
    /// Anchor index entries for O(log n) seek.
    anchor_index: Vec<IndexEntry>,
}

impl TvcbReader {
    /// Open a TVCB file and memory-map it.
    pub fn open(path: &Path) -> Result<Self, ReaderError> {
        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file) }
            .map_err(|e| ReaderError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Mmap failed: {}", e)
            )))?;

        // Parse header
        if mmap.len() < HEADER_SIZE {
            return Err(ReaderError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "File too small for header",
            )));
        }

        let mut header_buf = [0u8; HEADER_SIZE];
        header_buf.copy_from_slice(&mmap[..HEADER_SIZE]);
        let header = bytes_to_header_tvcb(&header_buf);

        // Validate header
        if header.magic != *b"TVCB" {
            return Err(ReaderError::InvalidMagic(header.magic));
        }
        if header.version != 1 {
            return Err(ReaderError::UnsupportedVersion(header.version));
        }

        // Read anchor index
        let index_offset = header.index_offset as usize;
        if index_offset < HEADER_SIZE || index_offset >= mmap.len().saturating_sub(32) {
            return Err(ReaderError::InvalidIndexOffset);
        }

        let num_anchors = header.num_anchors as usize;
        let index_start = index_offset;
        let index_end = index_start + num_anchors * INDEX_ENTRY_SIZE;
        
        if index_end > mmap.len() - 32 {
            return Err(ReaderError::IndexOutOfBounds);
        }

        // Parse anchor index entries
        let mut anchor_index = Vec::with_capacity(num_anchors);
        for i in 0..num_anchors {
            let pos = index_start + i * INDEX_ENTRY_SIZE;
            let bar_number = u64::from_le_bytes([
                mmap[pos],
                mmap[pos + 1],
                mmap[pos + 2],
                mmap[pos + 3],
                mmap[pos + 4],
                mmap[pos + 5],
                mmap[pos + 6],
                mmap[pos + 7],
            ]);
            let byte_offset = u64::from_le_bytes([
                mmap[pos + 8],
                mmap[pos + 9],
                mmap[pos + 10],
                mmap[pos + 11],
                mmap[pos + 12],
                mmap[pos + 13],
                mmap[pos + 14],
                mmap[pos + 15],
            ]);
            anchor_index.push(IndexEntry::new(bar_number, byte_offset));
        }

        // Verify SHA256 (last 32 bytes are the digest)
        let digest_start = mmap.len() - 32;
        let computed_digest = {
            use sha2::{Digest, Sha256};
            let mut sha = Sha256::new();
            sha.update(&mmap[..digest_start]);
            sha.finalize()
        };

        let stored_digest = &mmap[digest_start..];
        if computed_digest.as_slice() != stored_digest {
            return Err(ReaderError::Sha256Mismatch);
        }

        Ok(Self {
            mmap,
            header,
            anchor_index,
        })
    }

    /// Get the file header.
    pub fn header(&self) -> &TvcbHeader {
        &self.header
    }

    /// Get the number of bars in the file.
    pub fn num_bars(&self) -> u64 {
        self.header.num_bars
    }

    /// Get the anchor interval.
    pub fn anchor_interval(&self) -> u32 {
        self.header.anchor_interval
    }

    /// Get the start time (nanoseconds).
    pub fn start_time_ns(&self) -> u64 {
        self.header.start_time_ns
    }

    /// Get the end time (nanoseconds).
    pub fn end_time_ns(&self) -> u64 {
        self.header.end_time_ns
    }

    /// Get the number of anchors.
    pub fn num_anchors(&self) -> u32 {
        self.header.num_anchors
    }

    /// Read an anchor bar at the given byte offset.
    pub fn read_anchor_at(&self, byte_offset: u64) -> Result<AnchorBar, ReaderError> {
        let pos = byte_offset as usize;
        if pos + ANCHOR_BAR_SIZE > self.mmap.len() {
            return Err(ReaderError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Anchor bar extends beyond file",
            )));
        }

        let mut buf = [0u8; ANCHOR_BAR_SIZE];
        buf.copy_from_slice(&self.mmap[pos..pos + ANCHOR_BAR_SIZE]);
        Ok(bytes_to_anchor_bar(&buf))
    }

    /// Read a bar at the given index (0-based within this file).
    pub fn read_bar_at_index(&self, bar_index: u64) -> Result<Bar, ReaderError> {
        self.decode_bar_at(bar_index)
    }

    /// Decode a bar at the given index by finding its anchor and decoding forward.
    fn decode_bar_at(&self, bar_index: u64) -> Result<Bar, ReaderError> {
        // Binary search: find greatest IndexEntry.bar_number <= bar_index
        let anchor_idx = {
            let mut left = 0;
            let mut right = self.anchor_index.len();
            while left < right {
                let mid = (left + right) / 2;
                if self.anchor_index[mid].bar_number <= bar_index {
                    left = mid + 1;
                } else {
                    right = mid;
                }
            }
            if left == 0 {
                return Err(ReaderError::NoAnchors);
            }
            left - 1
        };

        let anchor_entry = &self.anchor_index[anchor_idx];
        let anchor = self.read_anchor_at(anchor_entry.byte_offset)?;
        let mut current_bar = anchor_to_bar(&anchor);
        let mut current_offset = anchor_entry.byte_offset as usize + ANCHOR_BAR_SIZE;
        let mut current_bar_number = anchor_entry.bar_number + 1;

        // If target is the anchor itself, return it
        if anchor_entry.bar_number == bar_index {
            return Ok(current_bar);
        }

        // Decode delta bars forward until we reach target (use <= to decode delta at bar_index)
        while current_bar_number <= bar_index {
            let (next_bar, bytes_consumed) = self.decode_delta_at(current_offset, &current_bar)?;
            current_offset += bytes_consumed;
            current_bar_number += 1;
            current_bar = next_bar;
        }

        Ok(current_bar)
    }

    /// Decode a delta bar at the given byte offset.
    fn decode_delta_at(&self, byte_offset: usize, prev_bar: &Bar) -> Result<(Bar, usize), ReaderError> {
        let data = &self.mmap[byte_offset..];
        let (bar, bytes_consumed) = decode_delta_bar(data, prev_bar)?;
        Ok((bar, bytes_consumed))
    }

    /// Get the anchor index entries.
    pub fn anchor_index(&self) -> &[IndexEntry] {
        &self.anchor_index
    }
}

// =============================================================================
// BarIter — iterate across multiple TVCB files
// =============================================================================

/// Iterator that yields bars across multiple TVCB files, optionally filtered by time range.
pub struct BarIter {
    /// Files to iterate (in chronological order).
    files: Vec<PathBuf>,
    /// Start timestamp filter (inclusive).
    start_ts: u64,
    /// End timestamp filter (inclusive).
    end_ts: u64,
    /// Current file reader.
    current_reader: Option<TvcbReader>,
    /// Current file index.
    current_file_idx: usize,
    /// Current bar index within the current file.
    current_bar_idx: u64,
}

impl BarIter {
    /// Create a new iterator over the given files, filtering by time range.
    pub fn new(files: Vec<PathBuf>, start_ts: u64, end_ts: u64) -> Result<Self, ReaderError> {
        let mut iter = Self {
            files,
            start_ts,
            end_ts,
            current_reader: None,
            current_file_idx: 0,
            current_bar_idx: 0,
        };
        
        // Open first file
        iter.open_current_file()?;
        // Seek to start_ts if needed
        iter.seek_to_start()?;
        
        Ok(iter)
    }

    /// Open the current file and reset bar index.
    fn open_current_file(&mut self) -> Result<(), ReaderError> {
        if self.current_file_idx >= self.files.len() {
            self.current_reader = None;
            return Ok(());
        }

        let path = &self.files[self.current_file_idx];
        let reader = TvcbReader::open(&std::path::Path::new(path))?;
        self.current_reader = Some(reader);
        self.current_bar_idx = 0;
        Ok(())
    }

    /// Seek to start_ts within the current file.
    fn seek_to_start(&mut self) -> Result<(), ReaderError> {
        let reader = match self.current_reader.as_mut() {
            Some(r) => r,
            None => return Ok(()),
        };

        // If start_ts is before file's start, begin at bar 0
        if self.start_ts <= reader.start_time_ns() {
            self.current_bar_idx = 0;
            return Ok(());
        }

        // Binary search to find first bar with ts >= start_ts
        let num_bars = reader.num_bars() as u64;
        let mut left = 0u64;
        let mut right = num_bars;

        while left < right {
            let mid = (left + right) / 2;
            let bar = reader.decode_bar_at(mid)?;
            if bar.ts_event < self.start_ts {
                left = mid + 1;
            } else {
                right = mid;
            }
        }

        self.current_bar_idx = left;
        Ok(())
    }

    /// Move to the next file in the list.
    fn advance_to_next_file(&mut self) -> Result<(), ReaderError> {
        self.current_file_idx += 1;
        self.open_current_file()?;
        Ok(())
    }
}

impl Iterator for BarIter {
    type Item = Result<Bar, ReaderError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let reader = match self.current_reader.as_mut() {
                Some(r) => r,
                None => return None,
            };

            let num_bars = reader.num_bars() as u64;

            // Check if we've exhausted current file
            if self.current_bar_idx >= num_bars {
                // Move to next file
                if let Err(e) = self.advance_to_next_file() {
                    return Some(Err(e));
                }
                continue;
            }

            // Read current bar
            let bar = match reader.read_bar_at_index(self.current_bar_idx) {
                Ok(b) => b,
                Err(e) => {
                    self.current_bar_idx += 1;
                    return Some(Err(e));
                }
            };

            self.current_bar_idx += 1;

            // Apply time range filter
            if bar.ts_event < self.start_ts {
                // Before range, skip
                continue;
            }
            if bar.ts_event > self.end_ts {
                // Past range — but stay in current file since bars are monotonically increasing
                // Only advance if we're sure there are no more bars in range
                let remaining = reader.num_bars() as u64 - (self.current_bar_idx - 1);
                // If current bar is already past end_ts and there could still be more bars
                // in this file, we should keep checking. But if we know there are no more
                // files, we can stop.
                if self.current_file_idx >= self.files.len().saturating_sub(1) {
                    // Last file — don't bother checking further
                    return None;
                }
                // For multi-file case, continue checking but advance if needed
            }

            return Some(Ok(bar));
        }
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Convert an AnchorBar to a Bar.
fn anchor_to_bar(anchor: &AnchorBar) -> Bar {
    let sell_volume = anchor.volume.saturating_sub(anchor.buy_volume);
    Bar::new(
        anchor.ts_event,
        anchor.open,
        anchor.high,
        anchor.low,
        anchor.close,
        anchor.vwap,
        anchor.volume,
        anchor.buy_volume,
        sell_volume,
        anchor.tick_count,
    )
}

/// Parse a TVCB header from bytes.
fn bytes_to_header_tvcb(buf: &[u8; HEADER_SIZE]) -> TvcbHeader {
    let mut magic = [0u8; 4];
    magic.copy_from_slice(&buf[0..4]);
    let version = buf[4];
    let decimal_precision = buf[5];
    let anchor_interval = u32::from_le_bytes([buf[6], buf[7], buf[8], buf[9]]);
    let instrument_id = u32::from_le_bytes([buf[10], buf[11], buf[12], buf[13]]);
    let start_time_ns = u64::from_le_bytes([
        buf[14], buf[15], buf[16], buf[17], buf[18], buf[19], buf[20], buf[21],
    ]);
    let end_time_ns = u64::from_le_bytes([
        buf[22], buf[23], buf[24], buf[25], buf[26], buf[27], buf[28], buf[29],
    ]);
    let num_bars = u64::from_le_bytes([
        buf[30], buf[31], buf[32], buf[33], buf[34], buf[35], buf[36], buf[37],
    ]);
    let num_anchors = u32::from_le_bytes([buf[38], buf[39], buf[40], buf[41]]);
    let index_offset = u64::from_le_bytes([
        buf[42], buf[43], buf[44], buf[45], buf[46], buf[47], buf[48], buf[49],
    ]);
    let year = u64::from_le_bytes([
        buf[50], buf[51], buf[52], buf[53], buf[54], buf[55], buf[56], buf[57],
    ]);
    let timeframe_ns = u64::from_le_bytes([
        buf[58], buf[59], buf[60], buf[61], buf[62], buf[63], buf[64], buf[65],
    ]);
    let mut reserved = [0u8; 62];
    reserved.copy_from_slice(&buf[66..128]);

    TvcbHeader {
        magic,
        version,
        decimal_precision,
        anchor_interval,
        instrument_id,
        start_time_ns,
        end_time_ns,
        num_bars,
        num_anchors,
        index_offset,
        year,
        timeframe_ns,
        reserved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_open_invalid_magic() {
        let mut file = NamedTempFile::new().unwrap();
        // Write a header with wrong magic
        let mut header = [0u8; HEADER_SIZE];
        header[0..4].copy_from_slice(b"XXXX"); // wrong magic
        header[4] = 1; // version
        header[5] = 9; // precision
        std::io::Write::write_all(&mut file, &header).unwrap();

        let result = TvcbReader::open(file.path());
        assert!(matches!(result, Err(ReaderError::InvalidMagic(_))));
    }

    #[test]
    fn test_open_unsupported_version() {
        let mut file = NamedTempFile::new().unwrap();
        let mut header = [0u8; HEADER_SIZE];
        header[0..4].copy_from_slice(b"TVCB");
        header[4] = 99; // unsupported version
        header[5] = 9; // precision
        std::io::Write::write_all(&mut file, &header).unwrap();

        let result = TvcbReader::open(file.path());
        assert!(matches!(result, Err(ReaderError::UnsupportedVersion(99))));
    }

    #[test]
    fn test_seek_accuracy() {
        use crate::tvcb::writer::TvcbWriter;
        
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        drop(tmp);

        let anchor_interval = 10;
        let mut writer = TvcbWriter::new(&path, 0x12345678, anchor_interval, 9, 2024, 900_000_000_000).unwrap();

        // Write 100 bars
        let mut bars = Vec::new();
        for i in 0..100u64 {
            let ts = 1_000_000_000_000_000_000 + i * 900_000_000_000;
            let bar = Bar::from_floats(
                ts,
                100.0 + i as f64,
                101.0 + i as f64,
                99.0 + i as f64,
                100.5 + i as f64,
                100.4 + i as f64,
                1000.0 + i as f64 * 10.0,
                600.0 + i as f64 * 6.0,
                400.0 + i as f64 * 4.0,
                10 + i as u32,
                9,
            );
            writer.write_bar(&bar).unwrap();
            bars.push(bar);
        }

        writer.finalize().unwrap();

        // Read back and verify seek accuracy
        let reader = TvcbReader::open(&path).unwrap();
        
        // Seek to random bars and verify timestamps
        for &idx in &[0u64, 50, 99] {
            let bar = reader.read_bar_at_index(idx).unwrap();
            assert_eq!(bar.ts_event, bars[idx as usize].ts_event, "bar {} ts mismatch", idx);
            assert_eq!(bar.open, bars[idx as usize].open, "bar {} open mismatch", idx);
            assert_eq!(bar.high, bars[idx as usize].high, "bar {} high mismatch", idx);
            assert_eq!(bar.low, bars[idx as usize].low, "bar {} low mismatch", idx);
            assert_eq!(bar.close, bars[idx as usize].close, "bar {} close mismatch", idx);
            assert_eq!(bar.volume, bars[idx as usize].volume, "bar {} volume mismatch", idx);
        }
    }

    #[test]
    fn test_cross_file_iteration() {
        use crate::tvcb::writer::TvcbWriter;
        
        // Create first file: 2022.tvcb with 10 bars
        let tmp1 = NamedTempFile::new().unwrap();
        let path1 = tmp1.path().to_path_buf();
        drop(tmp1);

        let mut writer1 = TvcbWriter::new(&path1, 0x12345678, 10, 9, 2022, 900_000_000_000).unwrap();
        for i in 0..10u64 {
            let ts = 1_640_000_000_000_000_000 + i * 900_000_000_000; // 2022-ish
            let bar = Bar::from_floats(
                ts, 100.0 + i as f64, 101.0 + i as f64, 99.0 + i as f64,
                100.5 + i as f64, 100.4 + i as f64, 1000.0, 600.0, 400.0, 10, 9,
            );
            writer1.write_bar(&bar).unwrap();
        }
        writer1.finalize().unwrap();

        // Create second file: 2023.tvcb with 10 bars
        let tmp2 = NamedTempFile::new().unwrap();
        let path2 = tmp2.path().to_path_buf();
        drop(tmp2);

        let mut writer2 = TvcbWriter::new(&path2, 0x12345678, 10, 9, 2023, 900_000_000_000).unwrap();
        for i in 0..10u64 {
            let ts = 1_680_000_000_000_000_000 + i * 900_000_000_000; // 2023-ish
            let bar = Bar::from_floats(
                ts, 105.0 + i as f64, 106.0 + i as f64, 104.0 + i as f64,
                105.5 + i as f64, 105.4 + i as f64, 1100.0, 660.0, 440.0, 12, 9,
            );
            writer2.write_bar(&bar).unwrap();
        }
        writer2.finalize().unwrap();

        // Create BarIter spanning both files
        let files = vec![path1, path2];
        let start_ts = 1_640_000_000_000_000_000; // start of 2022
        let end_ts = 1_680_000_000_000_000_000 + 20 * 900_000_000_000; // well past end of 2023
        
        let iter = BarIter::new(files, start_ts, end_ts).unwrap();
        let bars: Vec<Bar> = iter.filter_map(|r| r.ok()).collect();
        
        assert_eq!(bars.len(), 20, "expected 20 bars across 2 files, got {}", bars.len());
        
        // Verify first bar
        assert_eq!(bars[0].ts_event, 1_640_000_000_000_000_000);
        // Verify last bar
        assert_eq!(bars[19].ts_event, 1_680_000_000_000_000_000 + 9 * 900_000_000_000);
    }

    #[test]
    fn test_bariter_time_filter() {
        use crate::tvcb::writer::TvcbWriter;
        
        // Create first file with 20 bars
        let tmp1 = NamedTempFile::new().unwrap();
        let path1 = tmp1.path().to_path_buf();
        drop(tmp1);

        let mut writer = TvcbWriter::new(&path1, 0x12345678, 10, 9, 2024, 900_000_000_000).unwrap();
        for i in 0..20u64 {
            let ts = 1_700_000_000_000_000_000 + i * 900_000_000_000;
            let bar = Bar::from_floats(
                ts, 100.0 + i as f64, 101.0 + i as f64, 99.0 + i as f64,
                100.5 + i as f64, 100.4 + i as f64, 1000.0, 600.0, 400.0, 10, 9,
            );
            writer.write_bar(&bar).unwrap();
        }
        writer.finalize().unwrap();

        // Create iterator with narrow time range (bars 5-10)
        let start_ts = 1_700_000_000_000_000_000 + 5 * 900_000_000_000;
        let end_ts = 1_700_000_000_000_000_000 + 10 * 900_000_000_000;
        
        let iter = BarIter::new(vec![path1], start_ts, end_ts).unwrap();
        let bars: Vec<Bar> = iter.filter_map(|r| r.ok()).collect();
        
        assert_eq!(bars.len(), 6, "expected 6 bars in range [5,10], got {}", bars.len());
        assert_eq!(bars[0].ts_event, start_ts);
        assert_eq!(bars[5].ts_event, end_ts);
    }
}
