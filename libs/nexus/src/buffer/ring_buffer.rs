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
        if header.version != 2 {
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
        let avg_tick_ns = duration_ns.checked_div(num_ticks).unwrap_or(1000);

        let estimated_tick = ((target_ns - self.header.start_time_ns) / avg_tick_ns)
            .min(self.header.num_ticks.saturating_sub(1));

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
        let mut start_slot = left.saturating_sub(1);
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

            // Check if current tick_index is an anchor (look ahead in anchor_index)
            let is_anchor = if start_slot + 1 < self.anchor_index.len() {
                tick_index == self.anchor_index[start_slot + 1].tick_index
            } else {
                false
            };

            if is_anchor {
                // This tick is an anchor — advance to it
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

        // At this point, we broke out of the loop when last_tick.timestamp_ns >= target_ns.
        // tick_index has been incremented AFTER decoding the current tick.
        // So tick_index - 1 is the index of last_tick.
        Ok((offset, tick_index - 1, last_tick))
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
    /// Returns the decoded tick and bytes consumed (4 for base, 12 for overflow).
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
            // 14-byte overflow delta
            // Layout: [0xFF][4B ts_i32 zigzag][4B price_i32 zigzag][4B size_i32 zigzag][2B pad][1B side+flags]
            if byte_offset + 14 > self.mmap.len() {
                return Err(RingBufferError::SeekFailed(
                    "Overflow delta beyond bounds".into(),
                ));
            }

            let mut ts_buf = [0u8; 8];
            ts_buf[..4].copy_from_slice(&self.mmap[byte_offset + 1..byte_offset + 5]);
            let ts_delta_raw = u64::from_le_bytes(ts_buf);
            let price_extra_raw = i32::from_le_bytes([
                self.mmap[byte_offset + 5],
                self.mmap[byte_offset + 6],
                self.mmap[byte_offset + 7],
                self.mmap[byte_offset + 8],
            ]);
            let size_extra_raw = i32::from_le_bytes([
                self.mmap[byte_offset + 9],
                self.mmap[byte_offset + 10],
                self.mmap[byte_offset + 11],
                self.mmap[byte_offset + 12],
            ]);

            // zigzag_decode: i64 for ts (±2.1s overflow range), i32 for price/size.
            let zdec64 = |n: u64| -> i64 {
                let v = n as i64;
                (v >> 1) ^ (-(v & 1))
            };
            let zdec32 = |n: u32| -> i32 {
                let v = n as i32;
                (v >> 1) ^ (-(v & 1))
            };

            // ts is stored as zigzag(i32) microseconds. Decode and convert to nanoseconds.
            let timestamp_ns = (prev_tick.timestamp_ns as i64)
                .wrapping_add((zdec64(ts_delta_raw) as i64) * 1_000)
                as u64;
            let price_int = prev_tick.price_int + zdec32(price_extra_raw as u32) as i64;
            let size_int = prev_tick.size_int + zdec32(size_extra_raw as u32) as i64;

            // side (bit 0) and flags (bits 1-7) at byte 13
            let side = self.mmap[byte_offset + 13] & 1;
            let flags = (self.mmap[byte_offset + 13] >> 1) & 0x7F;

            Ok((
                TradeTick {
                    timestamp_ns,
                    price_int,
                    size_int,
                    side,
                    flags,
                                        sequence: prev_tick.sequence + 1,
                                    },
                                    14,
                                ))
                            } else {
            // 8-byte base delta
            if byte_offset + 8 > self.mmap.len() {
                return Err(RingBufferError::SeekFailed(
                    "Base delta beyond bounds".into(),
                ));
            }

            let mut packed_bytes = [0u8; 8];
            packed_bytes.copy_from_slice(&self.mmap[byte_offset..byte_offset + 8]);
            let packed = u64::from_le_bytes(packed_bytes);

            // 8-byte base layout (64 bits packed):
            //   bits 0-16:   ts_delta in 1µs units (17 bits, max 131ms)
            //   bits 17-34:  price_zigzag (18 bits)
            //   bits 35-61:  size_zigzag (27 bits)
            //   bit 62:      side (1 bit)
            //   bit 63:      flags (1 bit)
            const TIMESTAMP_DELTA_MASK: u64 = 0x1FFFF; // 17 bits
            const PRICE_ZIGZAG_MASK: u64 = 0x3FFFF;    // 18 bits
            const PRICE_ZIGZAG_SHIFT: u32 = 17;
            const SIZE_ZIGZAG_MASK: u64 = 0x7FFFFFF;   // 27 bits
            const SIZE_ZIGZAG_SHIFT: u32 = 35;

            let ts_delta_us = (packed & TIMESTAMP_DELTA_MASK) as u32;
            let price_zigzag_raw = (packed >> PRICE_ZIGZAG_SHIFT) & PRICE_ZIGZAG_MASK;
            let size_zigzag_raw = (packed >> SIZE_ZIGZAG_SHIFT) & SIZE_ZIGZAG_MASK;

            // Sign-extend 18-bit price_zigzag to i32
            let price_zigzag = if price_zigzag_raw & (1 << 17) != 0 {
                (price_zigzag_raw as i64 | 0xFFFFFC0000_i64) as i32
            } else {
                price_zigzag_raw as i32
            };
            // Sign-extend 27-bit size_zigzag to i32
            let size_zigzag = if size_zigzag_raw & (1 << 26) != 0 {
                (size_zigzag_raw as i64 | 0xFFFFFFFFF8000000_u64 as i64) as i32
            } else {
                size_zigzag_raw as i32
            };

            let price_delta = ((price_zigzag >> 1) ^ -(price_zigzag & 1)) as i64;
            let size_delta = ((size_zigzag >> 1) ^ -(size_zigzag & 1)) as i64;

            // Decode side (bit 62) and flags (bit 63) from packed
            let side = ((packed >> 62) & 1) as u8;
            let flags = ((packed >> 63) & 1) as u8;

            let timestamp_ns = prev_tick.timestamp_ns + (ts_delta_us as u64) * 1_000;
            let price_int = prev_tick.price_int + price_delta;
            let size_int = prev_tick.size_int + size_delta;

            Ok((
                TradeTick {
                    timestamp_ns,
                    price_int,
                    size_int,
                    side,
                    flags,
                    sequence: prev_tick.sequence + 1,
                },
                8,
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

    /// Create an iterator starting from a specific anchor position.
    /// The first_tick is the already-decoded anchor tick at byte_offset.
    /// RingIter takes ownership and returns it on the first next() call.
    /// anchor_slot is the index into anchor_index for the current anchor.
    pub fn iter_from(
        &self,
        byte_offset: usize,
        tick_index: u64,
        first_tick: TradeTick,
        anchor_slot: usize,
    ) -> RingIter<'_> {
        RingIter::from_position(self, byte_offset, tick_index, first_tick, anchor_slot)
    }
}

// =============================================================================
// RingIter
// =============================================================================

/// Zero-copy sequential iterator over TradeTicks from a RingBuffer.
///
/// Design:
/// - `current_offset` always points to the next unread byte in the tick stream
/// - `last_tick` is always the previously returned tick (used for delta decode)
/// - `anchor_slot` is the anchor_index entry for the CURRENT (most recently returned) anchor
/// - When `current_tick_index` crosses an anchor boundary (tick_index % anchor_interval == 0),
///   we re-decode from `anchor_index[anchor_slot + 1]` to resync the delta stream
///
/// The key invariant: after returning any tick, `current_offset` points to where the
/// NEXT tick's data begins. For anchor ticks that's `offset + ANCHOR_TICK_SIZE` (past anchor
/// data to first delta). For delta ticks that's `offset + consumed` (past delta to next item).
///
/// Construction modes:
/// - `new()`: start at first anchor, first next() returns anchor tick
/// - `range()`: start at seeked position, first next() returns anchor tick
/// - `from_position()`: start at already-decoded anchor, first next() returns anchor tick
///   (caller already decoded anchor separately; RingIter takes ownership and returns it)
#[derive(Debug)]
pub struct RingIter<'a> {
    buffer: &'a RingBuffer,
    /// Offset of the NEXT byte to read. After returning an anchor: offset + ANCHOR_TICK_SIZE.
    /// After returning a delta: offset + consumed. Always points to next unread data.
    current_offset: usize,
    /// Global tick index of the NEXT tick to be returned (before increment).
    /// Starts at 0 for new(), at seek position for range/from_position.
    current_tick_index: u64,
    /// The previously returned tick — used as reference for delta decoding.
    last_tick: TradeTick,
    /// Upper bound timestamp (non-inclusive). Iteration stops when ts >= end_ns.
    end_ns: u64,
    /// Index into buffer.anchor_index for the CURRENT anchor (the anchor whose
    /// data we've most recently returned or skipped past).
    /// When current_tick_index is in [0, 1023], anchor_slot=0.
    /// When current_tick_index is in [1024, 2047], anchor_slot=1.
    /// Updated when we cross an anchor boundary during iteration.
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
            anchor_slot: 0, // anchor_index[0] is the first anchor we returned
        }
    }

/// Create an iterator for a time range [start_ns, end_ns].
    fn range(buffer: &'a RingBuffer, start_ns: u64, end_ns: u64) -> Self {
        let (anchor_offset, tick_index, first_tick) = buffer
            .seek_to_time_ns(start_ns)
            .expect("Failed to seek to start_ns");

        // The anchor that contains tick_index
        let anchor_interval = buffer.anchor_interval() as u64;
        let anchor_slot = (tick_index / anchor_interval) as usize;

        Self {
            buffer,
            // anchor_offset: current_offset points to the anchor tick data at seek position.
            // last_tick is the already-decoded anchor tick from seek_to_time_ns.
            // first next() uses is_anchor_tick to decide: if anchor data, return last_tick;
            // if delta data, decode_delta_at.
            current_offset: anchor_offset,
            current_tick_index: tick_index,
            last_tick: first_tick,
            end_ns,
            anchor_slot,
        }
    }

    /// Create an iterator starting from an already-decoded anchor position.
    /// The caller has decoded `first_tick` at `byte_offset` and consumed it separately.
    /// We take ownership: first next() returns first_tick, then iteration continues
    /// from byte_offset + ANCHOR_TICK_SIZE as normal.
    ///
    /// `anchor_slot` is the index into anchor_index for the current anchor.
    pub(crate) fn from_position(
        buffer: &'a RingBuffer,
        byte_offset: usize,
        tick_index: u64,
        first_tick: TradeTick,
        anchor_slot: usize,
    ) -> Self {
        Self {
            buffer,
            // Pass byte_offset (anchor start), first next() will advance past anchor correctly
            current_offset: byte_offset,
            current_tick_index: tick_index,
            last_tick: first_tick,
            end_ns: u64::MAX,
            anchor_slot,
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

    /// Common helper: check if next tick is beyond end_ns without advancing.
    fn is_beyond_end(&self, timestamp_ns: u64) -> bool {
        timestamp_ns >= self.end_ns
    }

    fn is_at_end(&self) -> bool {
        self.current_tick_index >= self.buffer.num_ticks()
    }
}

impl<'a> Iterator for RingIter<'a> {
    type Item = TradeTick;

    fn next(&mut self) -> Option<Self::Item> {
        // Exhausted tick stream
        if self.is_at_end() {
            return None;
        }

        let anchor_interval = self.buffer.anchor_interval() as u64;

// ─── Anchor boundary check ────────────────────────────────────────────
        // TVC3 layout: anchor k covers tick indices [k*1024, (k+1)*1024 - 1].
        // Anchor k's data lives at anchor_index[k].byte_offset — it encodes tick k*1024.
        // After anchor k's data, delta encoding continues until tick (k+1)*1024 - 1.
        // At tick (k+1)*1024 (the first tick of anchor k+1), we must re-decode from
        // anchor_index[k+1] because delta decoding from anchor k's data won't produce it.
        //
        // We detect anchor boundaries by comparing computed_anchor_slot to anchor_slot:
        // - computed_anchor_slot = current_tick_index / anchor_interval
        // - anchor_slot = the anchor whose data we last consumed
        //
        // When computed_anchor_slot > anchor_slot, we've entered the next anchor's range
        // and need to re-decode from the next anchor's position.
        //
        // The is_anchor_tick check below determines HOW to return the current tick:
        // if it's the first tick of an anchor, return via anchor data (no delta decode).
        // The crossing_boundary check above handles WHEN to switch anchor data sources.
        let computed_anchor_slot = (self.current_tick_index / anchor_interval) as usize;
        let crossing_boundary = computed_anchor_slot > self.anchor_slot;

        if crossing_boundary {
            // We're at the first tick of a new anchor. Look up its byte_offset from index.
            let target_slot = computed_anchor_slot;

            let anchor_idx = self.buffer.anchor_index();
            if let Some(entry) = anchor_idx.get(target_slot) {
                let anchor_offset = entry.byte_offset as usize;

                // Decode the anchor tick at this position
                match self.buffer.decode_anchor_at(anchor_offset) {
                    Ok(anchor_tick) => {
                        // Validate: timestamp must not go backward STRICTLY.
                        // Equal timestamps are valid (consecutive ticks at same ns is allowed).
                        if anchor_tick.timestamp_ns < self.last_tick.timestamp_ns {
                            return None; // Corrupt data, stop iteration
                        }
                        // anchor_slot committed to this new anchor
                        self.anchor_slot = target_slot;
                        self.current_offset = anchor_offset;
                        self.last_tick = anchor_tick;
                    }
                    Err(_) => return None,
                }
            } else {
                // No more anchors in index — stop
                return None;
            }
        }

        // ─── Return current tick ──────────────────────────────────────────────
        // current_offset points to the start of the current tick's data.
        // If it's an anchor (offset is at anchor slot), advance past ANCHOR_TICK_SIZE.
        // If it's a delta, decode and advance by consumed bytes.
        let current_offset = self.current_offset;

        // Determine if we're at an anchor or delta by checking if current tick_index
        // is a multiple of anchor_interval (i.e., the first tick in an anchor group).
        // For anchor ticks (tick_index % anchor_interval == 0), the data is ANCHOR_TICK_SIZE.
        // For delta ticks, we decode to find consumed bytes.
        let is_anchor_tick = self.current_tick_index % anchor_interval == 0;

        if is_anchor_tick {
            // We're at an anchor tick — return it and advance past anchor data
            let tick = self.last_tick;

            // Check end_ns boundary before returning
            if self.is_beyond_end(tick.timestamp_ns) {
                return None;
            }

            // Advance: past anchor data to the first delta (or next structure)
            self.current_offset = current_offset + ANCHOR_TICK_SIZE;
            self.current_tick_index += 1;

            Some(tick)
        } else {
            // Delta tick — decode at current_offset
            let (tick, consumed) = match self.buffer.decode_delta_at(current_offset, &self.last_tick) {
                Ok(result) => result,
                Err(_) => return None,
            };

            // Check end_ns boundary
            if self.is_beyond_end(tick.timestamp_ns) {
                return None;
            }

            self.last_tick = tick;
            self.current_offset = current_offset + consumed;
            self.current_tick_index += 1;

            Some(self.last_tick)
        }
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