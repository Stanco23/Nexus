//! TVCB writer — writes delta-compressed bars to a TVCB file.
//!
//! # Layout
//! ```text
//! [128-byte header]
//! [anchor bar 72B][delta bar ...][anchor bar 72B][delta bar...]...
//!                                                           ^ index at EOF
//! [index: 16B * num_anchors]
//! [32B SHA256 digest]
//! ```

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::tvcb::encoding::{encode_delta_bar, DecodeError};
use crate::tvcb::types::{
    anchor_bar_to_bytes, bytes_to_anchor_bar,
    IndexEntry, TvcbHeader, HEADER_SIZE, ANCHOR_BAR_SIZE, INDEX_ENTRY_SIZE,
    Bar, AnchorBar,
};

// =============================================================================
// Error types
// =============================================================================

/// Errors that can occur during TVCB writing.
#[derive(Debug)]
pub enum WriterError {
    Io(std::io::Error),
    InvalidAnchorInterval(u32),
    NotFinalized,
    AlreadyFinalized,
    Encode(DecodeError),
}

impl std::fmt::Display for WriterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriterError::Io(e) => write!(f, "IO error: {}", e),
            WriterError::InvalidAnchorInterval(n) => {
                write!(f, "Invalid anchor_interval: {} (must be >= 1)", n)
            }
            WriterError::NotFinalized => write!(f, "File not finalized"),
            WriterError::AlreadyFinalized => write!(f, "File already finalized"),
            WriterError::Encode(e) => write!(f, "Encode error: {}", e),
        }
    }
}

impl std::error::Error for WriterError {}

impl From<std::io::Error> for WriterError {
    fn from(e: std::io::Error) -> Self {
        WriterError::Io(e)
    }
}

impl From<DecodeError> for WriterError {
    fn from(e: DecodeError) -> Self {
        WriterError::Encode(e)
    }
}

// =============================================================================
// TvcbWriter
// =============================================================================

/// TVCB file writer.
///
/// Writes bars with anchor-based delta compression. Call `finalize()` to
/// write the anchor index and SHA256 digest.
pub struct TvcbWriter {
    /// The underlying writer (BufWriter over File).
    writer: BufWriter<File>,
    /// Path to the file.
    path: PathBuf,
    /// Header.
    header: TvcbHeader,
    /// Anchor interval (bars per anchor).
    anchor_interval: u32,
    /// Number of bars written.
    bar_count: u64,
    /// Number of anchors written.
    anchor_count: u32,
    /// Byte offset of the first data after the header.
    data_start_offset: u64,
    /// Last written bar (for delta encoding).
    last_bar: Option<Bar>,
    /// Accumulated SHA256 for the data section.
    sha256: Sha256,
    /// Whether finalize() has been called.
    finalized: bool,
    /// Anchor index entries (written at EOF on finalize).
    anchor_index: Vec<IndexEntry>,
    /// Current byte offset within the data section.
    current_byte_offset: u64,
    /// Timestamp of first bar written.
    first_bar_ts: Option<u64>,
}

impl TvcbWriter {
    /// Create a new writer for the given path.
    ///
    /// `instrument_id` is a 32-bit FNV-1a hash of the instrument symbol.
    /// `anchor_interval` is the number of bars between full anchor bars (must be >= 1).
    pub fn new(
        path: &Path,
        instrument_id: u32,
        anchor_interval: u32,
        decimal_precision: u8,
        year: u64,
        timeframe_ns: u64,
    ) -> Result<Self, WriterError> {
        if anchor_interval == 0 {
            return Err(WriterError::InvalidAnchorInterval(anchor_interval));
        }

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;

        let mut writer = BufWriter::new(file);

        let header = TvcbHeader::new(
            instrument_id,
            anchor_interval,
            decimal_precision,
            year,
            timeframe_ns,
        );

        // Pre-allocate header space
        let empty_header = [0u8; HEADER_SIZE];
        writer.write_all(&empty_header)?;

        Ok(Self {
            writer,
            path: path.to_path_buf(),
            header,
            anchor_interval,
            bar_count: 0,
            anchor_count: 0,
            data_start_offset: HEADER_SIZE as u64,
            last_bar: None,
            sha256: Sha256::new(),
            finalized: false,
            anchor_index: Vec::new(),
            current_byte_offset: 0,
            first_bar_ts: None,
        })
    }

    /// Write a single bar.
    ///
    /// Every `anchor_interval` bars, a full 72-byte anchor is written.
    /// Between anchors, variable-length delta records are written.
    pub fn write_bar(&mut self, bar: &Bar) -> Result<(), WriterError> {
        if self.finalized {
            return Err(WriterError::AlreadyFinalized);
        }

        let bar_number = self.bar_count as u32;

        // Write anchor at interval boundaries
        let is_anchor = bar_number % self.anchor_interval == 0;

        if is_anchor {
            self.write_anchor_bar(bar, bar_number)?;
        } else {
            self.write_delta_bar(bar)?;
        }

        self.bar_count += 1;

        // Update header end time
        self.header.end_time_ns = bar.ts_event;

        // Set start time on first bar
        if self.first_bar_ts.is_none() {
            self.first_bar_ts = Some(bar.ts_event);
            self.header.start_time_ns = bar.ts_event;
        }

        Ok(())
    }

    /// Write a full 72-byte anchor bar.
    fn write_anchor_bar(&mut self, bar: &Bar, bar_number: u32) -> Result<(), WriterError> {
        // Record anchor in index
        let byte_offset = self.data_start_offset + self.current_byte_offset;
        self.anchor_index
            .push(IndexEntry::new(self.bar_count, byte_offset));
        self.anchor_count += 1;

        // Convert to AnchorBar and write
        let anchor = AnchorBar::from_bar(bar, bar_number);
        let bytes = anchor_bar_to_bytes(&anchor);
        
        // Update SHA256
        self.sha256.update(&bytes);
        
        self.writer.write_all(&bytes)?;
        self.current_byte_offset += ANCHOR_BAR_SIZE as u64;

        self.last_bar = Some(*bar);
        Ok(())
    }

    /// Write a delta bar (variable length).
    fn write_delta_bar(&mut self, bar: &Bar) -> Result<(), WriterError> {
        let prev = self
            .last_bar
            .ok_or_else(|| std::io::Error::other("No previous bar for delta encoding"))?;

        let bytes = encode_delta_bar(&prev, bar);
        
        // Update SHA256
        self.sha256.update(&bytes);
        
        self.writer.write_all(&bytes)?;
        self.current_byte_offset += bytes.len() as u64;

        self.last_bar = Some(*bar);
        Ok(())
    }

    /// Finalize the file: write header, anchor index, and SHA256.
    ///
    /// After finalization, no more bars can be written.
    pub fn finalize(mut self) -> Result<[u8; 32], WriterError> {
        if self.finalized {
            return Err(WriterError::AlreadyFinalized);
        }
        self.finalized = true;

        // Update header
        self.header.num_bars = self.bar_count;
        self.header.num_anchors = self.anchor_count;

        // Flush any pending writes
        self.writer.flush()?;

        // Get file position before writing index
        let index_offset = self.writer.stream_position()?;
        self.header.index_offset = index_offset;

        // Write anchor index entries (16 bytes each)
        for entry in &self.anchor_index {
            self.writer.write_all(&entry.bar_number.to_le_bytes())?;
            self.writer.write_all(&entry.byte_offset.to_le_bytes())?;
        }

        self.writer.flush()?;

        // Get the inner file and sync
        let mut file = self
            .writer
            .into_inner()
            .map_err(|e| WriterError::Io(e.into()))?;
        file.sync_all()?;

        // Build the FINAL header bytes with updated fields for SHA computation
        let final_header_bytes = header_to_bytes_internal(&self.header);

        // Compute SHA256 over final_header + all data + index
        let mut file = OpenOptions::new().read(true).open(&self.path)?;
        file.read_exact(&mut [0u8; HEADER_SIZE])?; // skip old header
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        drop(file);

        let mut sha = Sha256::new();
        sha.update(&final_header_bytes);
        sha.update(&data);
        let digest = sha.finalize();

        // Write final header at beginning (with updated num_bars, num_anchors, index_offset)
        let mut file = OpenOptions::new().write(true).open(&self.path)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&final_header_bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);

        // Write digest at end
        let mut file = OpenOptions::new().append(true).open(&self.path)?;
        file.write_all(&digest)?;
        file.flush()?;

        Ok(digest.into())
    }
}

// =============================================================================
// Byte conversion helpers
// =============================================================================

/// Serialize header to bytes.
fn header_to_bytes_internal(header: &TvcbHeader) -> [u8; HEADER_SIZE] {
    let mut buf = [0u8; HEADER_SIZE];

    buf[0..4].copy_from_slice(&header.magic);
    buf[4] = header.version;
    buf[5] = header.decimal_precision;
    buf[6..10].copy_from_slice(&header.anchor_interval.to_le_bytes());
    buf[10..14].copy_from_slice(&header.instrument_id.to_le_bytes());
    buf[14..22].copy_from_slice(&header.start_time_ns.to_le_bytes());
    buf[22..30].copy_from_slice(&header.end_time_ns.to_le_bytes());
    buf[30..38].copy_from_slice(&header.num_bars.to_le_bytes());
    buf[38..42].copy_from_slice(&header.num_anchors.to_le_bytes());
    buf[42..50].copy_from_slice(&header.index_offset.to_le_bytes());
    buf[50..58].copy_from_slice(&header.year.to_le_bytes());
    buf[58..66].copy_from_slice(&header.timeframe_ns.to_le_bytes());
    // buf[66..128] remain zero

    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_write_and_finalize() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        drop(tmp);

        let mut writer = TvcbWriter::new(&path, 0x12345678, 10, 9, 2024, 900_000_000_000).unwrap();

        // Write 15 bars (anchor at 0, 10)
        for i in 0..15u64 {
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
        }

        let digest = writer.finalize().unwrap();
        assert_eq!(digest.len(), 32);

        // Verify file exists and has content
        let metadata = std::fs::metadata(&path).unwrap();
        assert!(metadata.len() > 128 + 72 * 3); // header + anchors + deltas + index + digest
    }

    #[test]
    fn test_roundtrip_write_read() {
        use crate::tvcb::reader::TvcbReader;
        
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        drop(tmp);

        let anchor_interval = 10;
        let mut writer = TvcbWriter::new(&path, 0x12345678, anchor_interval, 9, 2024, 900_000_000_000).unwrap();

        // Write 25 bars (anchors at 0, 10, 20)
        let mut bars = Vec::new();
        for i in 0..25u64 {
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

        // Read back
        let reader = TvcbReader::open(&path).unwrap();
        assert_eq!(reader.num_bars(), 25);
        assert_eq!(reader.anchor_interval(), anchor_interval);
        
        // Read all bars
        for (i, expected) in bars.iter().enumerate() {
            let bar = reader.read_bar_at_index(i as u64).unwrap();
            assert_eq!(bar.ts_event, expected.ts_event, "bar {} ts_event mismatch", i);
            assert_eq!(bar.open, expected.open, "bar {} open mismatch", i);
            assert_eq!(bar.high, expected.high, "bar {} high mismatch", i);
            assert_eq!(bar.low, expected.low, "bar {} low mismatch", i);
            assert_eq!(bar.close, expected.close, "bar {} close mismatch", i);
            assert_eq!(bar.vwap, expected.vwap, "bar {} vwap mismatch", i);
            assert_eq!(bar.volume, expected.volume, "bar {} volume mismatch", i);
            assert_eq!(bar.buy_volume, expected.buy_volume, "bar {} buy_volume mismatch", i);
            assert_eq!(bar.sell_volume, expected.sell_volume, "bar {} sell_volume mismatch", i);
        }
    }

    #[test]
    fn test_delta_encoding_in_writer() {
        // Test that delta encoding is correct by checking the roundtrip
        let prev = Bar::new(
            1_000_000_000_000_000_000,
            100_000_000_000,
            101_000_000_000,
            99_000_000_000,
            100_500_000_000,
            100_400_000_000,
            1_000_000,
            600_000,
            400_000,
            10,
        );
        let next = Bar::new(
            1_000_000_900_000_000_000,
            100_100_000_000,
            101_100_000_000,
            99_100_000_000,
            100_600_000_000,
            100_500_000_000,
            1_001_000,
            600_600,
            400_400,
            11,
        );

        // Encode using the encoding module
        let encoded = encode_delta_bar(&prev, &next);
        
        // Decode back
        use crate::tvcb::encoding::decode_delta_bar;
        let (decoded, bytes_consumed) = decode_delta_bar(&encoded, &prev).unwrap();
        
        assert_eq!(bytes_consumed, encoded.len());
        assert_eq!(decoded.ts_event, next.ts_event);
        assert_eq!(decoded.open, next.open);
        assert_eq!(decoded.high, next.high);
        assert_eq!(decoded.low, next.low);
        assert_eq!(decoded.close, next.close);
        assert_eq!(decoded.vwap, next.vwap);
        assert_eq!(decoded.volume, next.volume);
        assert_eq!(decoded.buy_volume, next.buy_volume);
        assert_eq!(decoded.sell_volume, next.sell_volume);
    }
}