//! Delta compression for TVC3 tick encoding.
//!
//! Matches Nautilus `tvc_cython_loader.pyx` lines 60-77 and
//! `tvc_mmap_loader.py` lines 66-100 exactly.
//!
//! ## 4-byte Base Delta
//! ```text
//! bits 0-19:  timestamp_delta (20 bits, max ~524ms at ns precision)
//! bits 20-37: price_zigzag  (18 bits, zigzag-encoded i32)
//! bit 38:     side         (1 bit: 0=Buy, 1=Sell)
//! bit 39:     flags        (1 bit: 1=trade)
//! ```
//!
//! ## 15-byte Overflow Delta
//! ```text
//! byte 0:     0xFF         (overflow escape)
//! bytes 1-2:  ts_extra     (2B: upper 15 bits of 48-bit timestamp field)
//! bytes 3-6:  price_extra  (4B: i32, full signed price delta)
//! bytes 7-10: size_extra   (4B: i32, full signed size delta)
//! bytes 11-14: base_cont  (4B: same bit layout as base delta, timestamp base + side/flags)
//! ```

use crate::types::{AnchorTick, DecodedTick, TradeTick, ANCHOR_TICK_SIZE, OVERFLOW_ESCAPE};
use crate::types::{
    PRICE_ZIGZAG_MASK, PRICE_ZIGZAG_SHIFT, TIMESTAMP_DELTA_MASK, TIMESTAMP_EXTRA_SHIFT,
};

/// Maximum timestamp delta that fits in 20 bits.
pub const MAX_TIMESTAMP_DELTA: u32 = TIMESTAMP_DELTA_MASK; // 0xFFFFF = 1,048,575

// =============================================================================
// Zigzag encoding
// =============================================================================

/// Encode a signed i32 into an unsigned zigzag u32.
///
/// Nautilus: `(n << 1) ^ (n >> 31)`
#[inline]
pub fn zigzag_encode(n: i32) -> u32 {
    ((n << 1) ^ (n >> 31)) as u32
}

/// Decode a zigzag u32 back to signed i32.
///
/// Nautilus: `(n >> 1) ^ -(n & 1)`
#[inline]
pub fn zigzag_decode(n: u32) -> i32 {
    let n = n as i32;
    (n >> 1) ^ -(n & 1)
}

// =============================================================================
// Overflow detection
// =============================================================================

/// Returns true if the given delta requires overflow encoding.
///
/// Overflow needed when:
/// - timestamp_delta > 20 bits (MAX_TIMESTAMP_DELTA)
/// - price_delta doesn't fit in 18-bit signed range [-131072, 131071]
/// - side changed from previous
/// - flags changed from previous
#[inline]
pub fn needs_overflow(
    prev_side: u8,
    prev_flags: u8,
    ts_delta: u32,
    price_delta: i32,
    new_side: u8,
    new_flags: u8,
) -> bool {
    // Timestamp overflow: delta exceeds 20 bits
    if ts_delta > MAX_TIMESTAMP_DELTA {
        return true;
    }

    // Price overflow: 18-bit signed range
    if !(-(131_072)..=131_071).contains(&price_delta) {
        return true;
    }

    // Side or flags changed
    if new_side != prev_side || new_flags != prev_flags {
        return true;
    }

    false
}

/// The 18-bit signed zigzag range.
pub const PRICE_ZIGZAG_MIN: i32 = -(131_072); // -(2^17)
pub const PRICE_ZIGZAG_MAX: i32 = 131_071; // 2^17 - 1

// =============================================================================
// Pack delta (tick -> bytes)
// =============================================================================

/// Result of packing a delta between two ticks.
#[derive(Debug, Clone)]
pub enum PackedDelta {
    /// 4-byte base delta record.
    Base([u8; 4]),
    /// 15-byte overflow delta record.
    Overflow([u8; 15]),
}

/// Pack a delta between the previous tick and the next tick.
///
/// Returns `PackedDelta::Base` (4 bytes) or `PackedDelta::Overflow` (15 bytes).
///
/// `prev_sequence` is the tick index of the previous tick (anchor or delta).
pub fn pack_delta(prev_tick: &TradeTick, next_tick: &TradeTick) -> PackedDelta {
    let full_ts_delta = next_tick.timestamp_ns - prev_tick.timestamp_ns;
    let price_delta = (next_tick.price_int - prev_tick.price_int) as i32;
    let _side_changed = next_tick.side != prev_tick.side;
    let _flags_changed = next_tick.flags != prev_tick.flags;
    let ts_delta = full_ts_delta as u32;

    // Check if overflow encoding is needed
    // Note: must check full_ts_delta (u64) > MAX_TIMESTAMP_DELTA, not ts_delta (u32)
    let needs_overflow = prev_tick.side != next_tick.side
        || prev_tick.flags != next_tick.flags
        || full_ts_delta > MAX_TIMESTAMP_DELTA as u64
        || !(PRICE_ZIGZAG_MIN..=PRICE_ZIGZAG_MAX).contains(&price_delta);

    if needs_overflow {
        // ts_extra: top 15 bits of the upper 48-bit timestamp field
        // We compute the full 64-bit delta, split into extra_bits and base_bits
        let full_ts_delta = next_tick.timestamp_ns - prev_tick.timestamp_ns;
        let extra_bits = (full_ts_delta >> TIMESTAMP_EXTRA_SHIFT) & 0x7FFF;
        let base_bits = (full_ts_delta & TIMESTAMP_DELTA_MASK as u64) as u32;

        // ts_extra_raw: top bit must be 0 (it's just data), bottom 15 bits = extra_bits
        let ts_extra_raw = (extra_bits << 1) as u16; // shift to leave room for overflow signal bit

        // price_extra: full 32-bit signed delta
        let price_extra = price_delta;

        // size_extra: full 32-bit signed delta
        let size_extra = (next_tick.size_int - prev_tick.size_int) as i32;

        // Base continuation: only timestamp base + side/flags
        // Use bits 20-21 for side/flags (outside the 20-bit ts_base range at bits 0-19)
        const OVERFLOW_SIDE_SHIFT: u32 = 20;
        const OVERFLOW_FLAGS_SHIFT: u32 = 21;
        let packed_base = base_bits
            | ((next_tick.side as u32) << OVERFLOW_SIDE_SHIFT)
            | ((next_tick.flags as u32) << OVERFLOW_FLAGS_SHIFT);

        // Pack into 15 bytes
        let mut bytes = [0u8; 15];
        bytes[0] = OVERFLOW_ESCAPE;
        bytes[1] = (ts_extra_raw & 0xFF) as u8;
        bytes[2] = ((ts_extra_raw >> 8) & 0xFF) as u8;
        bytes[3..7].copy_from_slice(&price_extra.to_le_bytes());
        bytes[7..11].copy_from_slice(&size_extra.to_le_bytes());
        bytes[11..15].copy_from_slice(&packed_base.to_le_bytes());

        PackedDelta::Overflow(bytes)
    } else {
        // Base: 4 bytes
        // Note: side and flags are NOT encoded in base delta (bits 38-39 exceed 32 bits)
        // The decoder will preserve them from the previous tick
        let price_zigzag = zigzag_encode(price_delta) & PRICE_ZIGZAG_MASK;
        let packed = ts_delta | (price_zigzag << PRICE_ZIGZAG_SHIFT);

        let mut buf = [0u8; 4];
        buf.copy_from_slice(&packed.to_le_bytes());
        PackedDelta::Base(buf)
    }
}

// =============================================================================
// Unpack delta (bytes -> tick)
// =============================================================================

/// Decode a base (4-byte) delta record.
///
/// Returns decoded tick fields plus bytes consumed (4).
#[inline]
pub fn unpack_base_delta(bytes: &[u8; 4], prev_tick: &TradeTick, sequence: u32) -> DecodedTick {
    let packed = u32::from_le_bytes(*bytes);

    let ts_delta = packed & TIMESTAMP_DELTA_MASK;
    let price_zigzag_raw = (packed >> PRICE_ZIGZAG_SHIFT) & PRICE_ZIGZAG_MASK;

    // Sign-extend from 18 bits to i64 for proper arithmetic
    let price_zigzag = if price_zigzag_raw & (1 << 17) != 0 {
        // Set upper 14 bits to 1: ~0x3FFFF = 0xFFFFC00000
        (price_zigzag_raw as i64 | 0xFFFFFC0000_i64) as i32
    } else {
        price_zigzag_raw as i32
    };

    let price_delta = zigzag_decode(price_zigzag as u32);

    DecodedTick {
        timestamp_ns: prev_tick.timestamp_ns + ts_delta as u64,
        price_int: prev_tick.price_int + price_delta as i64,
        size_int: prev_tick.size_int, // base delta carries no size change
        side: prev_tick.side,         // base delta preserves side from prev
        flags: prev_tick.flags,       // base delta preserves flags from prev
        sequence,
        bytes_consumed: 4,
    }
}

/// Decode an overflow (15-byte) delta record.
///
/// Returns decoded tick fields plus bytes consumed (15).
#[inline]
pub fn unpack_overflow_delta(
    bytes: &[u8; 15],
    prev_tick: &TradeTick,
    sequence: u32,
) -> DecodedTick {
    // byte 0 = 0xFF already verified by caller
    let ts_extra_raw = u16::from_le_bytes([bytes[1], bytes[2]]);
    let price_extra = i32::from_le_bytes([bytes[3], bytes[4], bytes[5], bytes[6]]);
    let size_extra = i32::from_le_bytes([bytes[7], bytes[8], bytes[9], bytes[10]]);

    // Extract timestamp extra bits (top 15 bits of 48-bit field, excluding the 0 overflow bit)
    let extra_bits = (((ts_extra_raw >> 1) & 0x7FFF) as u64) << TIMESTAMP_EXTRA_SHIFT;

    // Base continuation (overflow format uses bits 20/21 for side/flags)
    const OVERFLOW_SIDE_SHIFT: u32 = 20;
    const OVERFLOW_FLAGS_SHIFT: u32 = 21;
    let base_packed = u32::from_le_bytes([bytes[11], bytes[12], bytes[13], bytes[14]]);
    let ts_base = base_packed & TIMESTAMP_DELTA_MASK;

    let timestamp_ns = prev_tick.timestamp_ns + extra_bits + ts_base as u64;
    let price_int = prev_tick.price_int + price_extra as i64;
    let size_int = prev_tick.size_int + size_extra as i64;
    let side = ((base_packed >> OVERFLOW_SIDE_SHIFT) & 1) as u8;
    let flags = ((base_packed >> OVERFLOW_FLAGS_SHIFT) & 1) as u8;

    DecodedTick {
        timestamp_ns,
        price_int,
        size_int,
        side,
        flags,
        sequence,
        bytes_consumed: 15,
    }
}

/// Decode a delta record at the given byte position.
///
/// Automatically detects base vs overflow based on first byte.
pub fn unpack_delta_at(
    data: &[u8],
    pos: usize,
    prev_tick: &TradeTick,
    sequence: u32,
) -> Result<DecodedTick, CompressionError> {
    if data[pos] == OVERFLOW_ESCAPE {
        if data.len() < pos + 15 {
            return Err(CompressionError::UnexpectedEndOfFile);
        }
        let mut bytes = [0u8; 15];
        bytes.copy_from_slice(&data[pos..pos + 15]);
        Ok(unpack_overflow_delta(&bytes, prev_tick, sequence))
    } else {
        if data.len() < pos + 4 {
            return Err(CompressionError::UnexpectedEndOfFile);
        }
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&data[pos..pos + 4]);
        Ok(unpack_base_delta(&bytes, prev_tick, sequence))
    }
}

/// Decode an anchor tick at the given byte offset in data.
pub fn unpack_anchor_at(data: &[u8], pos: usize) -> Result<AnchorTick, CompressionError> {
    if data.len() < pos + ANCHOR_TICK_SIZE {
        return Err(CompressionError::UnexpectedEndOfFile);
    }

    let timestamp_ns = u64::from_le_bytes([
        data[pos],
        data[pos + 1],
        data[pos + 2],
        data[pos + 3],
        data[pos + 4],
        data[pos + 5],
        data[pos + 6],
        data[pos + 7],
    ]);
    let price_int = i64::from_le_bytes([
        data[pos + 8],
        data[pos + 9],
        data[pos + 10],
        data[pos + 11],
        data[pos + 12],
        data[pos + 13],
        data[pos + 14],
        data[pos + 15],
    ]);
    let size_int = i64::from_le_bytes([
        data[pos + 16],
        data[pos + 17],
        data[pos + 18],
        data[pos + 19],
        data[pos + 20],
        data[pos + 21],
        data[pos + 22],
        data[pos + 23],
    ]);
    let side = data[pos + 24];
    let flags = data[pos + 25];
    let sequence = u32::from_le_bytes([
        data[pos + 26],
        data[pos + 27],
        data[pos + 28],
        data[pos + 29],
    ]);

    Ok(AnchorTick {
        timestamp_ns,
        price_int,
        size_int,
        side,
        flags,
        sequence,
    })
}

// =============================================================================
// Error types
// =============================================================================

#[derive(Debug, Clone)]
pub enum CompressionError {
    UnexpectedEndOfFile,
    InvalidOverflowRecord,
    InvalidTimestampDelta,
}

impl std::fmt::Display for CompressionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompressionError::UnexpectedEndOfFile => {
                write!(f, "Unexpected end of data while decoding delta")
            }
            CompressionError::InvalidOverflowRecord => write!(f, "Invalid overflow record"),
            CompressionError::InvalidTimestampDelta => {
                write!(f, "Invalid timestamp delta encoding")
            }
        }
    }
}

impl std::error::Error for CompressionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zigzag_encode_decode() {
        // Test roundtrip for various values
        for n in &[0i32, 1, -1, 127, -128, 131071, -131072, 1000000, -1000000] {
            let encoded = zigzag_encode(*n);
            let decoded = zigzag_decode(encoded);
            assert_eq!(decoded, *n, "zigzag roundtrip failed for {}", n);
        }
    }

    #[test]
    fn test_zigzag_known_values() {
        // From Nautilus test patterns
        assert_eq!(zigzag_encode(0), 0);
        assert_eq!(zigzag_encode(1), 2);
        assert_eq!(zigzag_encode(-1), 1);
        assert_eq!(zigzag_encode(131071), 262142); // 2*131071
        assert_eq!(zigzag_decode(0), 0);
        assert_eq!(zigzag_decode(2), 1);
        assert_eq!(zigzag_decode(1), -1);
    }

    #[test]
    fn test_overflow_detection() {
        // Within 20-bit timestamp, 18-bit price, same side → no overflow
        assert!(!needs_overflow(0, 1, 1000, 100, 0, 1));

        // Timestamp exceeds 20 bits
        assert!(needs_overflow(0, 1, 2_000_000, 100, 0, 1));

        // Price exceeds 18-bit signed
        assert!(needs_overflow(0, 1, 1000, 200_000, 0, 1));

        // Side changed
        assert!(needs_overflow(0, 1, 1000, 100, 1, 1));
    }

    #[test]
    fn test_pack_unpack_base_delta() {
        let prev = TradeTick::new(1_000_000_000, 100_000_000_000i64, 1_000_000i64, 0, 1, 0);
        let next = TradeTick::new(1_000_000_100, 100_000_000_100i64, 1_000_000i64, 0, 1, 1);

        let packed = pack_delta(&prev, &next);
        match packed {
            PackedDelta::Base(bytes) => {
                let decoded = unpack_base_delta(&bytes, &prev, 1);
                assert_eq!(decoded.timestamp_ns, next.timestamp_ns);
                assert_eq!(decoded.price_int, next.price_int);
                assert_eq!(decoded.size_int, next.size_int);
                assert_eq!(decoded.side, next.side);
                assert_eq!(decoded.bytes_consumed, 4);
            }
            PackedDelta::Overflow(_) => panic!("Expected base delta, got overflow"),
        }
    }

    #[test]
    fn test_pack_unpack_overflow_delta() {
        // Large timestamp jump (> 20 bits)
        let prev = TradeTick::new(1_000_000_000, 100_000_000_000i64, 1_000_000i64, 0, 1, 0);
        let next = TradeTick::new(3_000_000_000u64, 100_000_000_000i64, 1_000_000i64, 0, 1, 1);

        let packed = pack_delta(&prev, &next);
        match packed {
            PackedDelta::Overflow(bytes) => {
                assert_eq!(bytes[0], OVERFLOW_ESCAPE);
                let decoded = unpack_overflow_delta(&bytes, &prev, 1);
                assert_eq!(decoded.timestamp_ns, next.timestamp_ns);
                assert_eq!(decoded.price_int, next.price_int);
                assert_eq!(decoded.bytes_consumed, 15);
            }
            PackedDelta::Base(_) => panic!("Expected overflow delta, got base"),
        }
    }

    #[test]
    fn test_side_change_overflow() {
        let prev = TradeTick::new(1_000_000_000, 100_000_000_000i64, 1_000_000i64, 0, 1, 0);
        let next = TradeTick::new(1_000_000_100, 100_000_000_100i64, 1_000_000i64, 1, 1, 1);

        let packed = pack_delta(&prev, &next);
        match packed {
            PackedDelta::Overflow(_) => {}
            PackedDelta::Base(_) => panic!("Side change should trigger overflow"),
        }
    }

    #[test]
    fn test_price_change_no_size_change() {
        // Base delta: price changes but size stays same
        let prev = TradeTick::new(1_000_000_000, 100_000_000_000i64, 1_000_000i64, 0, 1, 0);
        let next = TradeTick::new(1_000_000_050, 100_000_000_050i64, 1_000_000i64, 0, 1, 1);

        let packed = pack_delta(&prev, &next);
        match packed {
            PackedDelta::Base(bytes) => {
                let decoded = unpack_base_delta(&bytes, &prev, 1);
                assert_eq!(decoded.size_int, prev.size_int); // size unchanged
            }
            PackedDelta::Overflow(_) => panic!("Expected base"),
        }
    }
}
