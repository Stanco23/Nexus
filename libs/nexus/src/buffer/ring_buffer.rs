//! RingBuffer — zero-copy TVC file access with binary search.
//!
//! # Overview
//! - `RingBuffer`: single-file, `Arc<Mmap>`, per-file binary search on anchors
//! - `RingIter`: zero-copy sequential iteration over decoded TradeTicks
//! - Merged anchor index across files built once at startup (not per-iteration)
//!
//! # Nautilus Source
//! `persistence/tvc_mmap_loader.py` (mmap patterns, seek logic)

use memmap2::Mmap;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use crate::instrument::InstrumentId;
use tvc::types::{ANCHOR_TICK_SIZE, HEADER_SIZE, INDEX_ENTRY_SIZE};
use tvc::{AnchorIndexEntry, TradeTick, TvcHeader};

/// Errors for RingBuffer operations.
#[derive(Debug)]
pub enum RingBufferError {
    Io(std::io::Error),
    InvalidHeader(String),
    TickNotFound(u64),
    NoAnchors,
    SeekFailed(String),
}

impl std::fmt::Display for RingBufferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RingBufferError::Io(e) => write!(f, "IO error: {}", e),
            RingBufferError::InvalidHeader(s) => write!(f, "Invalid header: {}", s),
            RingBufferError::TickNotFound(idx) => write!(f, "Tick {} not found", idx),
            RingBufferError::NoAnchors => write!(f, "No anchors in file"),
            RingBufferError::SeekFailed(s) => write!(f, "Seek failed: {}", s),
        }
    }
}

impl std::error::Error for RingBufferError {}

impl From<std::io::Error> for RingBufferError {
    fn from(e: std::io::Error) -> Self {
        RingBufferError::Io(e)
    }
}

// =============================================================================
// RingBuffer
// =============================================================================

/// Zero-copy TVC file access via memory-mapped I/O.
///
/// Wraps a single TVC3 file with:
/// - `Arc<Mmap>` for zero-copy access across threads
/// - Per-file binary search on anchors for O(log n) seek
/// - Sequential iteration via `RingIter`
#[derive(Debug, Clone)]
pub struct RingBuffer {
    /// Memory-mapped file data — shared across threads.
    mmap: Arc<Mmap>,
    /// File header parsed from the mmap.
    header: TvcHeader,
    /// Anchor index entries — binary searchable.
    anchor_index: Vec<AnchorIndexEntry>,
    /// Instrument ID for this buffer.
    instrument_id: InstrumentId,
    /// Anchor interval (ticks per anchor).
    anchor_interval: u32,
}

impl RingBuffer {
    /// Open a TVC3 file and memory-map it.
    pub fn open(path: &Path, instrument_id: InstrumentId) -> Result<Self, RingBufferError> {
        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file) }?;
        let mmap = Arc::new(mmap);

        if mmap.len() < HEADER_SIZE {
            return Err(RingBufferError::InvalidHeader("File too small".into()));
        }

        let header = Self::parse_header(&mmap)?;
        let anchor_index = Self::build_anchor_index(&mmap, &header)?;

        Ok(Self {
            mmap,
            header,
            anchor_index,
            instrument_id,
            anchor_interval: header.anchor_interval,
        })
    }

    /// Parse TvcHeader from memory-mapped data.
    fn parse_header(mmap: &Mmap) -> Result<TvcHeader, RingBufferError> {
        let mut buf = [0u8; HEADER_SIZE];
        buf.copy_from_slice(&mmap[..HEADER_SIZE]);
        let header = tvc::writer::bytes_to_header(&buf);

        if header.magic != *b"TVC3" {
            return Err(RingBufferError::InvalidHeader("Invalid TVC magic".into()));
        }
        if header.version != 1 {
            return Err(RingBufferError::InvalidHeader("Unsupported version".into()));
        }

        Ok(header)
    }

    /// Build the anchor index from the mmap.
    fn build_anchor_index(
        mmap: &Mmap,
        header: &TvcHeader,
    ) -> Result<Vec<AnchorIndexEntry>, RingBufferError> {
        let index_offset = header.index_offset as usize;
        if index_offset == 0 || index_offset > mmap.len() {
            return Err(RingBufferError::InvalidHeader(
                "Invalid index offset".into(),
            ));
        }

        // Read num_anchors from the first 4 bytes at index_offset
        if index_offset + 4 > mmap.len() {
            return Err(RingBufferError::InvalidHeader(
                "Index offset too close to end of file".into(),
            ));
        }
        let num_anchors = u32::from_le_bytes([
            mmap[index_offset],
            mmap[index_offset + 1],
            mmap[index_offset + 2],
            mmap[index_offset + 3],
        ]) as usize;

        if num_anchors == 0 {
            return Err(RingBufferError::NoAnchors);
        }

        // Index entries start after the 4-byte num_anchors
        let index_start = index_offset + 4;
        let index_end = index_start + num_anchors * INDEX_ENTRY_SIZE;
        if index_end > mmap.len() {
            return Err(RingBufferError::InvalidHeader(
                "Index extends beyond file".into(),
            ));
        }

        let mut anchors = Vec::with_capacity(num_anchors);
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
            anchors.push(AnchorIndexEntry::new(tick_index, byte_offset));
        }

        Ok(anchors)
    }

    /// Get the instrument ID for this buffer.
    pub fn instrument_id(&self) -> InstrumentId {
        self.instrument_id.clone()
    }

    /// Get the file header.
    pub fn header(&self) -> &TvcHeader {
        &self.header
    }

    /// Get the number of ticks in this file.
    pub fn num_ticks(&self) -> u64 {
        self.header.num_ticks
    }

    /// Get the number of anchors in this file.
    pub fn num_anchors(&self) -> u32 {
        self.header.num_anchors
    }

    /// Get the anchor interval.
    pub fn anchor_interval(&self) -> u32 {
        self.anchor_interval
    }

    /// Get the start time (first tick timestamp).
    pub fn start_time_ns(&self) -> u64 {
        self.header.start_time_ns
    }

    /// Get the end time (last tick timestamp).
    pub fn end_time_ns(&self) -> u64 {
        self.header.end_time_ns
    }

    /// Get the anchor index entries.
    pub fn anchor_index(&self) -> &[AnchorIndexEntry] {
        &self.anchor_index
    }

    /// Get the memory-mapped data reference.
    pub fn mmap_data(&self) -> &[u8] {
        &self.mmap
    }

    /// Binary search to find the byte offset for the given tick_index.
    /// Returns (byte_offset, tick_index_of_anchor) for the anchor at or before the target.
    pub fn seek_to_tick(&self, tick_index: u64) -> Result<(usize, u64), RingBufferError> {
        let mut left = 0;
        let mut right = self.anchor_index.len();

        while left < right {
            let mid = (left + right) / 2;
            if self.anchor_index[mid].tick_index <= tick_index {
                left = mid + 1;
            } else {
                right = mid;
            }
        }

        if left == 0 {
            return Err(RingBufferError::TickNotFound(tick_index));
        }

        let entry = &self.anchor_index[left - 1];
        Ok((entry.byte_offset as usize, entry.tick_index))
    }

    /// Binary search to find the anchor at or before the given timestamp.
    ///
    /// Returns (byte_offset, tick_index, decoded_tick) for the anchor.
    /// Uses estimated tick position + anchor walk to efficiently locate the target.
    pub fn seek_to_time_ns(&self, target_ns: u64) -> Result<(usize, u64, TradeTick), RingBufferError> {
        // Quick bounds check
        if target_ns < self.header.start_time_ns {
            return Err(RingBufferError::TickNotFound(target_ns));
        }

        let duration_ns = self.header.end_time_ns.saturating_sub(self.header.start_time_ns);
        let num_ticks = self.header.num_ticks.saturating_sub(1);
        let avg_tick_ns = if num_ticks > 0 {
            duration_ns / num_ticks
        } else {
            1000
        };

        let estimated_tick = if avg_tick_ns > 0 {
            ((target_ns - self.header.start_time_ns) / avg_tick_ns)
                .min(self.header.num_ticks.saturating_sub(1))
        } else {
            0
        };

        // Binary search on anchor tick_index to find anchor <= estimated_tick
        let mut left = 0;
        let mut right = self.anchor_index.len();

        while left < right {
            let mid = (left + right) / 2;
            if self.anchor_index[mid].tick_index <= estimated_tick {
                left = mid + 1;
            } else {
                right = mid;
            }
        }

        // left is first entry where tick_index > estimated_tick
        // So left-1 is the anchor we want (at or before estimated position)
        let mut start_slot = left.saturating_sub(1).max(0);
        let mut offset = self.anchor_index[start_slot].byte_offset as usize;
        let mut tick_index = self.anchor_index[start_slot].tick_index;

        // Decode anchor at this position
        let mut last_tick = self.decode_anchor_at(offset)?;

        // If anchor timestamp already > target_ns, we're past — return this anchor
        if last_tick.timestamp_ns > target_ns {
            return Ok((offset, tick_index, last_tick));
        }

        // Advance offset past the anchor we just decoded
        offset += ANCHOR_TICK_SIZE;
        tick_index += 1;

        // Walk forward through deltas/anchors until timestamp >= target_ns or end of buffer
        loop {
            if tick_index >= self.header.num_ticks {
                break;
            }
            if last_tick.timestamp_ns >= target_ns {
                break;
            }

            // Check if next tick is an anchor slot
            let next_anchor_tick = ((tick_index / self.anchor_interval as u64) + 1) * self.anchor_interval as u64;

            if tick_index + 1 == next_anchor_tick && start_slot + 1 < self.anchor_index.len() {
                // Next tick is an anchor — advance to it
                start_slot += 1;
                offset = self.anchor_index[start_slot].byte_offset as usize;
                last_tick = self.decode_anchor_at(offset)?;
                tick_index += 1;
            } else {
                // Delta tick
                let result = self.decode_delta_at(offset, &last_tick);
                match result {
                    Ok((tick, consumed)) => {
                        offset += consumed;
                        last_tick = tick;
                        tick_index += 1;
                    }
                    Err(_) => break,
                }
            }
        }

        Ok((offset, tick_index, last_tick))
    }

    /// Get the first anchor byte offset (start of data after header).
    pub fn first_anchor_offset(&self) -> usize {
        HEADER_SIZE
    }

    /// Decode an anchor tick at the given byte offset.
    pub fn decode_anchor_at(&self, byte_offset: usize) -> Result<TradeTick, RingBufferError> {
        if byte_offset + ANCHOR_TICK_SIZE > self.mmap.len() {
            return Err(RingBufferError::SeekFailed("Beyond file bounds".into()));
        }

        let timestamp_ns = u64::from_le_bytes([
            self.mmap[byte_offset],
            self.mmap[byte_offset + 1],
            self.mmap[byte_offset + 2],
            self.mmap[byte_offset + 3],
            self.mmap[byte_offset + 4],
            self.mmap[byte_offset + 5],
            self.mmap[byte_offset + 6],
            self.mmap[byte_offset + 7],
        ]);
        let price_int = i64::from_le_bytes([
            self.mmap[byte_offset + 8],
            self.mmap[byte_offset + 9],
            self.mmap[byte_offset + 10],
            self.mmap[byte_offset + 11],
            self.mmap[byte_offset + 12],
            self.mmap[byte_offset + 13],
            self.mmap[byte_offset + 14],
            self.mmap[byte_offset + 15],
        ]);
        let size_int = i64::from_le_bytes([
            self.mmap[byte_offset + 16],
            self.mmap[byte_offset + 17],
            self.mmap[byte_offset + 18],
            self.mmap[byte_offset + 19],
            self.mmap[byte_offset + 20],
            self.mmap[byte_offset + 21],
            self.mmap[byte_offset + 22],
            self.mmap[byte_offset + 23],
        ]);
        let side = self.mmap[byte_offset + 24];
        let flags = self.mmap[byte_offset + 25];
        let sequence = u32::from_le_bytes([
            self.mmap[byte_offset + 26],
            self.mmap[byte_offset + 27],
            self.mmap[byte_offset + 28],
            self.mmap[byte_offset + 29],
        ]);

        Ok(TradeTick {
            timestamp_ns,
            price_int,
            size_int,
            side,
            flags,
            sequence,
        })
    }

    /// Decode a delta tick at the given byte offset using the provided reference tick.
    /// Returns the decoded tick and bytes consumed (4 for base, 15 for overflow).
    pub fn decode_delta_at(
        &self,
        byte_offset: usize,
        prev_tick: &TradeTick,
    ) -> Result<(TradeTick, usize), RingBufferError> {
        // Check against the index offset - if we're at or past the index, no more ticks
        if byte_offset >= self.header.index_offset as usize {
            return Err(RingBufferError::SeekFailed("Beyond tick data".into()));
        }

        if byte_offset >= self.mmap.len() {
            return Err(RingBufferError::SeekFailed("Beyond file bounds".into()));
        }

        let first_byte = self.mmap[byte_offset];

        if first_byte == 0xFF {
            // 15-byte overflow delta
            if byte_offset + 15 > self.mmap.len() {
                return Err(RingBufferError::SeekFailed(
                    "Overflow delta beyond bounds".into(),
                ));
            }

            let ts_extra_raw =
                u16::from_le_bytes([self.mmap[byte_offset + 1], self.mmap[byte_offset + 2]]);
            let price_extra = i32::from_le_bytes([
                self.mmap[byte_offset + 3],
                self.mmap[byte_offset + 4],
                self.mmap[byte_offset + 5],
                self.mmap[byte_offset + 6],
            ]);
            let size_extra = i32::from_le_bytes([
                self.mmap[byte_offset + 7],
                self.mmap[byte_offset + 8],
                self.mmap[byte_offset + 9],
                self.mmap[byte_offset + 10],
            ]);

            const TIMESTAMP_EXTRA_SHIFT: u32 = 20;
            const OVERFLOW_SIDE_SHIFT: u32 = 20;
            const OVERFLOW_FLAGS_SHIFT: u32 = 21;
            const TIMESTAMP_DELTA_MASK: u32 = 0xFFFFF; // 20 bits, matches types.rs

            let extra_bits = (((ts_extra_raw >> 1) & 0x7FFF) as u64) << TIMESTAMP_EXTRA_SHIFT;
            let base_packed = u32::from_le_bytes([
                self.mmap[byte_offset + 11],
                self.mmap[byte_offset + 12],
                self.mmap[byte_offset + 13],
                self.mmap[byte_offset + 14],
            ]);
            let ts_base = (base_packed & TIMESTAMP_DELTA_MASK) as u64;

            let timestamp_ns = prev_tick.timestamp_ns + extra_bits + ts_base;
            let price_int = prev_tick.price_int + price_extra as i64;
            let size_int = prev_tick.size_int + size_extra as i64;
            let side = ((base_packed >> OVERFLOW_SIDE_SHIFT) & 1) as u8;
            let flags = ((base_packed >> OVERFLOW_FLAGS_SHIFT) & 1) as u8;

            Ok((
                TradeTick {
                    timestamp_ns,
                    price_int,
                    size_int,
                    side,
                    flags,
                    sequence: prev_tick.sequence + 1,
                },
                15,
            ))
        } else {
            // 4-byte base delta
            if byte_offset + 4 > self.mmap.len() {
                return Err(RingBufferError::SeekFailed(
                    "Base delta beyond bounds".into(),
                ));
            }

            let packed = u32::from_le_bytes([
                self.mmap[byte_offset],
                self.mmap[byte_offset + 1],
                self.mmap[byte_offset + 2],
                self.mmap[byte_offset + 3],
            ]);

            const TIMESTAMP_DELTA_MASK: u32 = 0xFFFFF; // 20 bits, matches types.rs
            const PRICE_ZIGZAG_MASK: u32 = 0x7FFFF; // 19 bits, matches types.rs
            const PRICE_ZIGZAG_SHIFT: u32 = 20; // matches types.rs

            let ts_delta = packed & TIMESTAMP_DELTA_MASK;
            let price_zigzag_raw = (packed >> PRICE_ZIGZAG_SHIFT) & PRICE_ZIGZAG_MASK;

            // Sign-extend from 18 bits to i64 for proper arithmetic
            let price_zigzag = if price_zigzag_raw & (1 << 17) != 0 {
                (price_zigzag_raw as i64 | 0xFFFFFC0000_i64) as i32
            } else {
                price_zigzag_raw as i32
            };

            // Zigzag decode: (n >> 1) ^ -(n & 1)
            let price_delta = ((price_zigzag >> 1) ^ -(price_zigzag & 1)) as i64;

            let timestamp_ns = prev_tick.timestamp_ns + ts_delta as u64;
            let price_int = prev_tick.price_int + price_delta;
            let size_int = prev_tick.size_int;
            let side = prev_tick.side;
            let flags = prev_tick.flags;

            Ok((
                TradeTick {
                    timestamp_ns,
                    price_int,
                    size_int,
                    side,
                    flags,
                    sequence: prev_tick.sequence + 1,
                },
                4,
            ))
        }
    }

    /// Iterate all ticks sequentially from start.
    pub fn iter(&self) -> RingIter<'_> {
        RingIter::new(self)
    }

    /// Iterate ticks within a time range [start_ns, end_ns].
    pub fn iter_range(&self, start_ns: u64, end_ns: u64) -> RingIter<'_> {
        RingIter::range(self, start_ns, end_ns)
    }
}

// =============================================================================
// RingIter
// =============================================================================

/// Zero-copy sequential iterator over TradeTicks from a RingBuffer.
///
/// Iterates through the memory-mapped data, decoding anchors and deltas.
/// Does not copy the underlying mmap data — only decodes into TradeTicks.
#[derive(Debug)]
pub struct RingIter<'a> {
    buffer: &'a RingBuffer,
    current_offset: usize,
    current_tick_index: u64,
    last_tick: TradeTick,
    end_ns: u64,
    started: bool,
    next_anchor_tick: u64,
    anchor_slot: usize,
}

impl<'a> RingIter<'a> {
    /// Create a new iterator starting from the first tick.
    fn new(buffer: &'a RingBuffer) -> Self {
        let first_offset = buffer.first_anchor_offset();
        let first_tick = buffer
            .decode_anchor_at(first_offset)
            .expect("Failed to decode first anchor");

        Self {
            buffer,
            current_offset: first_offset,
            current_tick_index: 0,
            last_tick: first_tick,
            end_ns: u64::MAX,
            started: false,
            next_anchor_tick: buffer.anchor_interval() as u64,
            anchor_slot: 1,
        }
    }

    /// Create an iterator for a time range [start_ns, end_ns].
    fn range(buffer: &'a RingBuffer, start_ns: u64, end_ns: u64) -> Self {
        let (offset, tick_index, first_tick) = buffer
            .seek_to_time_ns(start_ns)
            .expect("Failed to seek to start_ns");

        let anchor_interval = buffer.anchor_interval() as u64;
        let current_anchor_index = tick_index / anchor_interval;
        let next_anchor_tick = (current_anchor_index + 1) * anchor_interval;

        Self {
            buffer,
            current_offset: offset,
            current_tick_index: tick_index,
            last_tick: first_tick,
            end_ns,
            started: false,
            next_anchor_tick,
            anchor_slot: current_anchor_index as usize + 1,
        }
    }

    /// Get current tick without advancing.
    pub fn peek(&self) -> Option<&TradeTick> {
        if self.current_tick_index < self.buffer.num_ticks() {
            Some(&self.last_tick)
        } else {
            None
        }
    }
}

impl<'a> Iterator for RingIter<'a> {
    type Item = TradeTick;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_tick_index >= self.buffer.num_ticks() {
            return None;
        }

        // Check end_ns before returning
        // For iter(): end_ns = u64::MAX, use > check (original behavior)
        // For iter_range(): end_ns = specific value, use >= check (exclusive end)
        let past_end = if self.end_ns == u64::MAX {
            self.last_tick.timestamp_ns > self.end_ns
        } else {
            self.last_tick.timestamp_ns >= self.end_ns
        };
        if past_end {
            return None;
        }

        // On first iteration, return the initial tick
        // For iter(): the initial tick is always an anchor, advance by ANCHOR_TICK_SIZE
        // For iter_range(): the initial tick could be an anchor OR a delta, don't advance
        if !self.started {
            self.started = true;
            // Only advance if this is a full iteration (end_ns == u64::MAX)
            if self.end_ns == u64::MAX {
                self.current_offset += ANCHOR_TICK_SIZE;
                self.current_tick_index += 1;
                self.anchor_slot = 1;
            }
            // For range iteration, don't advance - the seek already positioned us correctly

            return Some(self.last_tick);
        }

        // Check if the current position is at an anchor (we've already decoded it, checking from current position)
        let next_idx = self.anchor_slot;
        if next_idx < self.buffer.anchor_index().len()
            && self.current_tick_index == self.buffer.anchor_index()[next_idx].tick_index
        {
            match self.buffer.decode_anchor_at(self.current_offset) {
                Ok(tick) => {
                    self.last_tick = tick;
                    self.current_offset += ANCHOR_TICK_SIZE;
                    self.current_tick_index += 1;
                    self.anchor_slot += 1;

                    if self.last_tick.timestamp_ns > self.end_ns {
                        return None;
                    }
                    return Some(self.last_tick);
                }
                Err(_) => return None,
            }
        }

        // Decode next delta tick
        let (tick, consumed) = match self
            .buffer
            .decode_delta_at(self.current_offset, &self.last_tick)
        {
            Ok(result) => result,
            Err(_) => return None,
        };

        self.last_tick = tick;
        self.current_offset += consumed;
        self.current_tick_index += 1;

        if self.current_tick_index == self.next_anchor_tick {
            self.next_anchor_tick += self.buffer.anchor_interval() as u64;
        }

        Some(self.last_tick)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = (self.buffer.num_ticks() - self.current_tick_index) as usize;
        (remaining, Some(remaining))
    }
}

impl<'a> ExactSizeIterator for RingIter<'a> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instrument_id_in_buffer() {
        let id = InstrumentId::new("ETHUSDT", "BINANCE");
        assert_eq!(id.id, fnv1a_hash(b"ETHUSDT.BINANCE"));
    }

    fn fnv1a_hash(data: &[u8]) -> u32 {
        let mut hash: u32 = 0x811c9dc5;
        for byte in data {
            hash ^= *byte as u32;
            hash = hash.wrapping_mul(0x01000193);
        }
        hash
    }
}
