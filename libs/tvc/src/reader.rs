//! TVC3 reader — memory-mapped reading with random access.
//!
//! # Usage
//! ```ignore
//! let reader = TvcReader::open("data.tvc")?;
//! let tick = reader.decode_tick_at(offset)?;
//! let pos = reader.seek_to_tick(tick_index)?;
//! ```

use memmap2::Mmap;
use std::fs::File;
use std::path::Path;

use crate::compression::{unpack_anchor_at, unpack_base_delta, unpack_overflow_delta};
use crate::types::{
    AnchorIndexEntry, AnchorTick, TradeTick, TvcHeader, HEADER_SIZE, INDEX_ENTRY_SIZE,
    OVERFLOW_ESCAPE,
};
use crate::writer::bytes_to_header;

#[derive(Debug)]
pub enum ReaderError {
    Io(std::io::Error),
    InvalidMagic([u8; 4]),
    UnsupportedVersion(u8),
    InvalidIndexOffset,
    IndexOutOfBounds,
    NoAnchors,
    TickNotFound,
    Sha256Mismatch,
}

impl std::fmt::Display for ReaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReaderError::Io(e) => write!(f, "IO error: {}", e),
            ReaderError::InvalidMagic(m) => write!(f, "Invalid TVC magic: {:?}", m),
            ReaderError::UnsupportedVersion(v) => write!(f, "Unsupported TVC version: {}", v),
            ReaderError::InvalidIndexOffset => write!(f, "Invalid index offset in header"),
            ReaderError::IndexOutOfBounds => write!(f, "Index extends beyond file"),
            ReaderError::NoAnchors => write!(f, "No anchors in file"),
            ReaderError::TickNotFound => write!(f, "Tick not found in index"),
            ReaderError::Sha256Mismatch => write!(f, "SHA256 digest mismatch"),
        }
    }
}

impl std::error::Error for ReaderError {}

impl From<std::io::Error> for ReaderError {
    fn from(e: std::io::Error) -> Self {
        ReaderError::Io(e)
    }
}

// =============================================================================
// TvcReader
// =============================================================================

/// TVC3 file reader with memory-mapped access and binary search.
///
/// Memory-maps the file for efficient random access. The SHA256 digest
/// is stored at EOF (last 32 bytes) and covers the entire file up to
/// but not including the digest.
pub struct TvcReader {
    /// Memory-mapped file data.
    mmap: Mmap,
    /// File header.
    header: TvcHeader,
    /// Anchor index entries for O(log n) seek.
    anchor_index: Vec<AnchorIndexEntry>,
    /// Reference tick for decoding deltas (last decoded anchor or delta).
    last_tick: TradeTick,
}

impl TvcReader {
    /// Open a TVC3 file and memory-map it.
    pub fn open(path: &Path) -> Result<Self, ReaderError> {
        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file)? };

        // Parse header
        if mmap.len() < HEADER_SIZE {
            return Err(ReaderError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "File too small for header",
            )));
        }

        let mut header_buf = [0u8; HEADER_SIZE];
        header_buf.copy_from_slice(&mmap[..HEADER_SIZE]);
        let header = bytes_to_header(&header_buf);

        // Validate header
        if header.magic != *b"TVC3" {
            return Err(ReaderError::InvalidMagic(header.magic));
        }
        if header.version != 1 {
            return Err(ReaderError::UnsupportedVersion(header.version));
        }

        // Read anchor index
        let index_offset = header.index_offset as usize;
        if index_offset > mmap.len() - 4 {
            return Err(ReaderError::InvalidIndexOffset);
        }

        // Read num_anchors
        let num_anchors = u32::from_le_bytes([
            mmap[index_offset],
            mmap[index_offset + 1],
            mmap[index_offset + 2],
            mmap[index_offset + 3],
        ]) as usize;

        let index_start = index_offset + 4;
        let index_end = index_start + num_anchors * INDEX_ENTRY_SIZE;
        if index_end > mmap.len() {
            return Err(ReaderError::IndexOutOfBounds);
        }

        // Parse anchor index entries
        let mut anchor_index = Vec::with_capacity(num_anchors);
        for i in 0..num_anchors {
            let pos = index_start + i * INDEX_ENTRY_SIZE;
            let tick_index = u64::from_le_bytes([
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
            anchor_index.push(AnchorIndexEntry::new(tick_index, byte_offset));
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

        // Get the first anchor tick as our reference
        let first_anchor = unpack_anchor_at(&mmap, HEADER_SIZE).map_err(|_| {
            ReaderError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Failed to decode first anchor",
            ))
        })?;

        let last_tick = TradeTick {
            timestamp_ns: first_anchor.timestamp_ns,
            price_int: first_anchor.price_int,
            size_int: first_anchor.size_int,
            side: first_anchor.side,
            flags: first_anchor.flags,
            sequence: first_anchor.sequence,
        };

        Ok(Self {
            mmap,
            header,
            anchor_index,
            last_tick,
        })
    }

    /// Get the file header.
    pub fn header(&self) -> &TvcHeader {
        &self.header
    }

    /// Get the number of ticks in the file.
    pub fn num_ticks(&self) -> u64 {
        self.header.num_ticks
    }

    /// Get the number of anchors in the file.
    pub fn num_anchors(&self) -> u32 {
        self.header.num_anchors
    }

    /// Binary search to find the byte offset of the anchor for the given tick_index.
    /// Returns the byte offset of the anchor entry for the tick.
    pub fn seek_to_tick(&mut self, tick_index: u64) -> Result<u64, ReaderError> {
        let mut left = 0;
        let mut right = self.anchor_index.len();

        while left < right {
            let mid = (left + right) / 2;
            let entry = &self.anchor_index[mid];
            if entry.tick_index <= tick_index {
                left = mid + 1;
            } else {
                right = mid;
            }
        }

        if left == 0 {
            return Err(ReaderError::TickNotFound);
        }

        let entry = &self.anchor_index[left - 1];
        let mut current_offset = entry.byte_offset as usize;
        let mut current_tick_index = entry.tick_index;

        // Decode the anchor to prime `last_tick`
        let anchor =
            crate::compression::unpack_anchor_at(&self.mmap, current_offset).map_err(|_| {
                ReaderError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Failed to decode anchor",
                ))
            })?;
        self.last_tick = TradeTick {
            timestamp_ns: anchor.timestamp_ns,
            price_int: anchor.price_int,
            size_int: anchor.size_int,
            side: anchor.side,
            flags: anchor.flags,
            sequence: anchor.sequence,
        };

        if current_tick_index == tick_index {
            return Ok(current_offset as u64);
        }

        current_offset += crate::types::ANCHOR_TICK_SIZE;
        current_tick_index += 1;

        while current_tick_index < tick_index {
            let data = &self.mmap[current_offset..];
            if data[0] == crate::types::OVERFLOW_ESCAPE {
                if data.len() < 15 {
                    return Err(ReaderError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "",
                    )));
                }
                let mut bytes = [0u8; 15];
                bytes.copy_from_slice(&data[..15]);
                let decoded = crate::compression::unpack_overflow_delta(
                    &bytes,
                    &self.last_tick,
                    self.last_tick.sequence + 1,
                );
                self.last_tick = TradeTick {
                    timestamp_ns: decoded.timestamp_ns,
                    price_int: decoded.price_int,
                    size_int: decoded.size_int,
                    side: decoded.side,
                    flags: decoded.flags,
                    sequence: decoded.sequence,
                };
                current_offset += 15;
            } else {
                if data.len() < 4 {
                    return Err(ReaderError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "",
                    )));
                }
                let mut bytes = [0u8; 4];
                bytes.copy_from_slice(&data[..4]);
                let decoded = crate::compression::unpack_base_delta(
                    &bytes,
                    &self.last_tick,
                    self.last_tick.sequence + 1,
                );
                self.last_tick = TradeTick {
                    timestamp_ns: decoded.timestamp_ns,
                    price_int: decoded.price_int,
                    size_int: decoded.size_int,
                    side: decoded.side,
                    flags: decoded.flags,
                    sequence: decoded.sequence,
                };
                current_offset += 4;
            }
            current_tick_index += 1;
        }

        Ok(current_offset as u64)
    }

    /// Decode a tick at the given byte offset.
    /// Returns the decoded tick and the sequence number.
    ///
    /// This auto-detects anchor vs delta: if the position is at an anchor
    /// (identified by the anchor index), it decodes an anchor; otherwise
    /// it decodes a delta (4B base or 15B overflow).
    pub fn decode_tick_at(&mut self, byte_offset: usize) -> Result<TradeTick, ReaderError> {
        let data = &self.mmap[byte_offset..];

        if data[0] == OVERFLOW_ESCAPE {
            // 15-byte overflow delta
            let mut bytes = [0u8; 15];
            bytes.copy_from_slice(&data[..15]);
            let decoded =
                unpack_overflow_delta(&bytes, &self.last_tick, self.last_tick.sequence + 1);
            self.last_tick = TradeTick {
                timestamp_ns: decoded.timestamp_ns,
                price_int: decoded.price_int,
                size_int: decoded.size_int,
                side: decoded.side,
                flags: decoded.flags,
                sequence: decoded.sequence,
            };
            Ok(self.last_tick)
        } else {
            // Could be base delta (4B) or anchor (30B with non-0xFF first byte)
            // Check if first byte could be a valid timestamp delta - base deltas
            // always have timestamp_delta <= MAX_TIMESTAMP_DELTA, and anchor
            // timestamps are typically much larger.
            // Since we can't easily tell from one value, check against our
            // known anchor positions.
            let is_anchor = self
                .anchor_index
                .iter()
                .any(|e| e.byte_offset == byte_offset as u64);

            if is_anchor {
                // Decode as anchor
                let anchor = unpack_anchor_at(data, 0).map_err(|_| {
                    ReaderError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Failed to decode anchor",
                    ))
                })?;
                self.last_tick = TradeTick {
                    timestamp_ns: anchor.timestamp_ns,
                    price_int: anchor.price_int,
                    size_int: anchor.size_int,
                    side: anchor.side,
                    flags: anchor.flags,
                    sequence: anchor.sequence,
                };
                Ok(self.last_tick)
            } else {
                // 4-byte base delta
                let mut bytes = [0u8; 4];
                bytes.copy_from_slice(&data[..4]);
                let decoded =
                    unpack_base_delta(&bytes, &self.last_tick, self.last_tick.sequence + 1);
                self.last_tick = TradeTick {
                    timestamp_ns: decoded.timestamp_ns,
                    price_int: decoded.price_int,
                    size_int: decoded.size_int,
                    side: decoded.side,
                    flags: decoded.flags,
                    sequence: decoded.sequence,
                };
                Ok(self.last_tick)
            }
        }
    }

    /// Decode an anchor tick at the given byte offset.
    pub fn decode_anchor(&self, byte_offset: usize) -> Result<AnchorTick, ReaderError> {
        unpack_anchor_at(&self.mmap, byte_offset).map_err(|_| {
            ReaderError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Failed to decode anchor",
            ))
        })
    }

    /// Get the anchor index entries.
    pub fn anchor_index(&self) -> &[AnchorIndexEntry] {
        &self.anchor_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_open_invalid_magic() {
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        // Write a header with wrong magic
        let mut header = [0u8; HEADER_SIZE];
        header[0..4].copy_from_slice(b"XXXX"); // wrong magic
        header[4] = 1; // version
        header[5] = 9; // precision
        file.write_all(&header).unwrap();

        let result = TvcReader::open(file.path());
        assert!(matches!(result, Err(ReaderError::InvalidMagic(_))));
    }
}
