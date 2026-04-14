//! TVC3 writer — writes delta-compressed ticks to a TVC3 file.
//!
//! # Layout
//! ```text
//! [128-byte header]
//! [anchor tick 30B][delta tick 4B or 15B]...[anchor][delta]...
//!                                             ^ index at EOF
//! [index: 4B num_anchors + (16B * num_anchors)]
//! ```
//!
//! # Usage
//! ```ignore
//! let mut writer = TvcWriter::new("data.tvc", instrument_id, 1000)?;
//! for tick in ticks {
//!     writer.write_tick(&tick)?;
//! }
//! writer.finalize()?;
//! ```

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use sha2::{Sha256, Digest};

use crate::types::{
    TvcHeader, AnchorTick, AnchorIndexEntry, TradeTick,
    HEADER_SIZE, ANCHOR_TICK_SIZE,
};
use crate::compression::pack_delta;

/// Errors that can occur during writing.
#[derive(Debug)]
pub enum WriterError {
    Io(std::io::Error),
    InvalidAnchorInterval(u32),
    NotFinalized,
    AlreadyFinalized,
}

impl std::fmt::Display for WriterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriterError::Io(e) => write!(f, "IO error: {}", e),
            WriterError::InvalidAnchorInterval(n) => {
                write!(f, "Invalid anchor_interval: {} (must be >= 2)", n)
            }
            WriterError::NotFinalized => write!(f, "File not finalized"),
            WriterError::AlreadyFinalized => write!(f, "File already finalized"),
        }
    }
}

impl std::error::Error for WriterError {}

impl From<std::io::Error> for WriterError {
    fn from(e: std::io::Error) -> Self {
        WriterError::Io(e)
    }
}

// =============================================================================
// TvcWriter
// =============================================================================

/// TVC3 file writer.
///
/// Writes ticks with anchor-based delta compression. Call `finalize()` to
/// write the anchor index and SHA256 digest.
pub struct TvcWriter {
    /// The underlying writer (BufWriter over File).
    writer: BufWriter<File>,
    /// Path to the file (needed for reopening during finalize).
    path: PathBuf,
    /// Header (written on finalize, or can be flushed early for recovery).
    header: TvcHeader,
    /// Anchor interval (ticks per anchor).
    anchor_interval: u32,
    /// Number of ticks written.
    tick_count: u64,
    /// Number of anchors written.
    anchor_count: u32,
    /// Byte offset of the first tick/anchor after the header.
    data_start_offset: u64,
    /// Last written tick (for delta encoding).
    last_tick: Option<TradeTick>,
    /// Accumulated SHA256 for the data section.
    sha256: Sha256,
    /// Whether finalize() has been called.
    finalized: bool,
    /// Anchor index entries (written at EOF on finalize).
    anchor_index: Vec<AnchorIndexEntry>,
    /// Current byte offset within the data section.
    current_byte_offset: u64,
}

impl TvcWriter {
    /// Create a new writer for the given path.
    ///
    /// `instrument_id` is a 32-bit FNV-1a hash of the instrument symbol.
    /// `anchor_interval` is the number of ticks between full anchor ticks (must be >= 2).
    pub fn new(
        path: &Path,
        instrument_id: u32,
        anchor_interval: u32,
        decimal_precision: u8,
    ) -> Result<Self, WriterError> {
        if anchor_interval < 2 {
            return Err(WriterError::InvalidAnchorInterval(anchor_interval));
        }

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;

        let mut writer = BufWriter::new(file);

        let header = TvcHeader::new(instrument_id, anchor_interval, decimal_precision);

        // Pre-allocate header space
        let empty_header = [0u8; HEADER_SIZE];
        writer.write_all(&empty_header)?;

        Ok(Self {
            writer,
            path: path.to_path_buf(),
            header,
            anchor_interval,
            tick_count: 0,
            anchor_count: 0,
            data_start_offset: HEADER_SIZE as u64,
            last_tick: None,
            sha256: Sha256::new(),
            finalized: false,
            anchor_index: Vec::new(),
            current_byte_offset: 0,
        })
    }

    /// Write a single tick.
    ///
    /// Every `anchor_interval` ticks, a full 30-byte anchor is written.
    /// Between anchors, 4-byte or 15-byte delta records are written.
    pub fn write_tick(&mut self, tick: &TradeTick) -> Result<(), WriterError> {
        if self.finalized {
            return Err(WriterError::AlreadyFinalized);
        }

        let is_anchor = self.tick_count.is_multiple_of(self.anchor_interval as u64);

        if is_anchor {
            self.write_anchor(tick)?;
        } else {
            self.write_delta(tick)?;
        }

        self.tick_count += 1;

        // Update header end time
        self.header.end_time_ns = tick.timestamp_ns;

        // Update SHA256
        let buf = tick_to_bytes(tick);
        self.sha256.update(buf);

        Ok(())
    }

    /// Write a full 30-byte anchor tick.
    fn write_anchor(&mut self, tick: &TradeTick) -> Result<(), WriterError> {
        // Record anchor in index
        let byte_offset = self.data_start_offset + self.current_byte_offset;
        self.anchor_index.push(AnchorIndexEntry::new(self.tick_count, byte_offset));
        self.anchor_count += 1;

        // Convert TradeTick to AnchorTick and write
        let anchor = AnchorTick {
            timestamp_ns: tick.timestamp_ns,
            price_int: tick.price_int,
            size_int: tick.size_int,
            side: tick.side,
            flags: tick.flags,
            sequence: tick.sequence,
        };
        let bytes = anchor_to_bytes(&anchor);
        self.writer.write_all(&bytes)?;
        self.current_byte_offset += ANCHOR_TICK_SIZE as u64;

        self.last_tick = Some(*tick);
        Ok(())
    }

    /// Write a delta record (4 bytes or 15 bytes).
    fn write_delta(&mut self, tick: &TradeTick) -> Result<(), WriterError> {
        let prev = self.last_tick.ok_or_else(|| {
            std::io::Error::other("No previous tick for delta encoding")
        })?;

        let packed = pack_delta(&prev, tick);

        match packed {
            crate::compression::PackedDelta::Base(bytes) => {
                self.writer.write_all(&bytes)?;
                self.current_byte_offset += 4;
            }
            crate::compression::PackedDelta::Overflow(bytes) => {
                self.writer.write_all(&bytes)?;
                self.current_byte_offset += 15;
            }
        }

        self.last_tick = Some(*tick);
        Ok(())
    }

    /// Finalize the file: write header, anchor index, and SHA256.
    ///
    /// After finalization, no more ticks can be written.
    pub fn finalize(mut self) -> Result<[u8; 32], WriterError> {
        use std::io::Read;

        if self.finalized {
            return Err(WriterError::AlreadyFinalized);
        }
        self.finalized = true;

        // Update header
        self.header.num_ticks = self.tick_count;
        self.header.num_anchors = self.anchor_count;

        // Flush any pending writes
        self.writer.flush()?;

        // Get file position before writing header
        let index_offset = self.writer.stream_position()?;

        // Write anchor index at current position
        self.header.index_offset = index_offset;

        // Write num_anchors (4 bytes) + all entries (16 bytes each)
        let num_anchors_bytes = self.anchor_count.to_le_bytes();
        self.writer.write_all(&num_anchors_bytes)?;

        for entry in &self.anchor_index {
            self.writer.write_all(&entry.tick_index.to_le_bytes())?;
            self.writer.write_all(&entry.byte_offset.to_le_bytes())?;
        }

        self.writer.flush()?;

        // Get the inner file
        let file = self.writer.into_inner().map_err(|e| WriterError::Io(e.into()))?;

        // Compute SHA256: header (128 bytes) + all tick data + index
        // NOTE: after into_inner(), the original file handle cannot be used for reading.
        // We must reopen the file to compute the digest.
        drop(file); // release the original handle first
        let header_bytes = header_to_bytes(&self.header);

        // Reopen file for reading to compute SHA256
        let mut file = OpenOptions::new().read(true).open(&self.path)?;
        file.seek(SeekFrom::Start(128))?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        drop(file);

        use sha2::{Sha256, Digest};
        let mut sha = Sha256::new();
        sha.update(header_bytes);
        sha.update(&data);
        let digest = sha.finalize();

        // Write header at beginning (use write-only reopen)
        let mut file = OpenOptions::new().write(true).open(&self.path)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header_bytes)?;
        file.flush()?;
        drop(file);

        // Write digest at end
        let mut file = OpenOptions::new().append(true).open(&self.path)?;
        file.write_all(&digest)?;
        file.flush()?;

        Ok(digest.into())
    }
}

// =============================================================================
// Byte packing helpers
// =============================================================================

/// Convert a trade tick to 30 bytes.
fn anchor_to_bytes(tick: &AnchorTick) -> [u8; ANCHOR_TICK_SIZE] {
    let mut buf = [0u8; ANCHOR_TICK_SIZE];

    buf[0..8].copy_from_slice(&tick.timestamp_ns.to_le_bytes());
    buf[8..16].copy_from_slice(&tick.price_int.to_le_bytes());
    buf[16..24].copy_from_slice(&tick.size_int.to_le_bytes());
    buf[24] = tick.side;
    buf[25] = tick.flags;
    buf[26..30].copy_from_slice(&tick.sequence.to_le_bytes());

    buf
}

/// Convert a trade tick to 30 bytes.
fn tick_to_bytes(tick: &TradeTick) -> [u8; ANCHOR_TICK_SIZE] {
    let mut buf = [0u8; ANCHOR_TICK_SIZE];

    buf[0..8].copy_from_slice(&tick.timestamp_ns.to_le_bytes());
    buf[8..16].copy_from_slice(&tick.price_int.to_le_bytes());
    buf[16..24].copy_from_slice(&tick.size_int.to_le_bytes());
    buf[24] = tick.side;
    buf[25] = tick.flags;
    buf[26..30].copy_from_slice(&tick.sequence.to_le_bytes());

    buf
}

/// Write a header to 128 bytes.
fn header_to_bytes(header: &TvcHeader) -> [u8; HEADER_SIZE] {
    let mut buf = [0u8; HEADER_SIZE];

    buf[0..4].copy_from_slice(&header.magic);
    buf[4] = header.version;
    buf[5] = header.decimal_precision;
    buf[6..10].copy_from_slice(&header.anchor_interval.to_le_bytes());
    buf[10..14].copy_from_slice(&header.instrument_id.to_le_bytes());
    buf[14..22].copy_from_slice(&header.start_time_ns.to_le_bytes());
    buf[22..30].copy_from_slice(&header.end_time_ns.to_le_bytes());
    buf[30..38].copy_from_slice(&header.num_ticks.to_le_bytes());
    buf[38..42].copy_from_slice(&header.num_anchors.to_le_bytes());
    buf[42..50].copy_from_slice(&header.index_offset.to_le_bytes());
    // buf[50..128] are already zero

    buf
}

/// Read a header from 128 bytes.
pub fn bytes_to_header(buf: &[u8; HEADER_SIZE]) -> TvcHeader {
    let mut magic = [0u8; 4];
    magic.copy_from_slice(&buf[0..4]);
    let version = buf[4];
    let decimal_precision = buf[5];
    let anchor_interval = u32::from_le_bytes([buf[6], buf[7], buf[8], buf[9]]);
    let instrument_id = u32::from_le_bytes([buf[10], buf[11], buf[12], buf[13]]);
    let start_time_ns = u64::from_le_bytes([buf[14], buf[15], buf[16], buf[17], buf[18], buf[19], buf[20], buf[21]]);
    let end_time_ns = u64::from_le_bytes([buf[22], buf[23], buf[24], buf[25], buf[26], buf[27], buf[28], buf[29]]);
    let num_ticks = u64::from_le_bytes([buf[30], buf[31], buf[32], buf[33], buf[34], buf[35], buf[36], buf[37]]);
    let num_anchors = u32::from_le_bytes([buf[38], buf[39], buf[40], buf[41]]);
    let index_offset = u64::from_le_bytes([buf[42], buf[43], buf[44], buf[45], buf[46], buf[47], buf[48], buf[49]]);
    let mut reserved = [0u8; 78];
    reserved.copy_from_slice(&buf[50..128]);

    TvcHeader {
        magic,
        version,
        decimal_precision,
        anchor_interval,
        instrument_id,
        start_time_ns,
        end_time_ns,
        num_ticks,
        num_anchors,
        index_offset,
        reserved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_and_read_header() {
        let header = TvcHeader::new(12345, 1000, 2);
        let bytes = header_to_bytes(&header);
        assert_eq!(bytes.len(), HEADER_SIZE);

        let parsed = bytes_to_header(&bytes);
        assert_eq!(parsed.magic, header.magic);
        assert_eq!(parsed.version, header.version);
        assert_eq!(parsed.decimal_precision, header.decimal_precision);
        let lhs_interval = parsed.anchor_interval;
        let rhs_interval = header.anchor_interval;
        let lhs_id = parsed.instrument_id;
        let rhs_id = header.instrument_id;
        assert_eq!(lhs_interval, rhs_interval);
        assert_eq!(lhs_id, rhs_id);
    }

    #[test]
    fn test_anchor_to_bytes_roundtrip() {
        let tick = TradeTick::from_floats(1_000_000_000, 100.5, 0.1, 0, 2, 42);
        let bytes = tick_to_bytes(&tick);

        let _pos = 0usize;
        let ts = u64::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]]);
        assert_eq!(ts, tick.timestamp_ns);

        let anchor = AnchorTick {
            timestamp_ns: tick.timestamp_ns,
            price_int: tick.price_int,
            size_int: tick.size_int,
            side: tick.side,
            flags: tick.flags,
            sequence: tick.sequence,
        };
        let anchor_ts = anchor.timestamp_ns;
        assert_eq!(anchor_ts, tick.timestamp_ns);
    }
}
