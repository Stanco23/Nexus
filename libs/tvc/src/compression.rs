//! Delta compression for TVC3 tick encoding.
//!
//! ## 4-byte Base Delta (when ts_delta ≤ 2^20 and price fits in 18-bit zigzag)
//! ```text
//! bits 0-19:  timestamp_delta (20 bits, max ~1.05ms at ns precision)
//! bits 20-37: price_zigzag  (18 bits, zigzag-encoded i32)
//! bit 38:     side         (1 bit: 0=Buy, 1=Sell)
//! bit 39:     flags        (1 bit: 1=trade)
//! ```
//!
//! ## 8-byte Overflow Delta (when ts_delta > 20 bits or price exceeds 18 bits or side/flags change)
//! ```text
//! byte 0:     0xFF         (overflow escape)
//! bytes 1-2:  ts_extra     (2B: upper 16 bits of timestamp beyond 20-bit base)
//! bytes 3-6:  price_extra  (4B: i32, full signed price delta)
//! byte 7:     size_extra   (1B: i8, signed size delta)
//! ```
//!
//! Average: ~5-7 bytes/tick depending on overflow frequency
//! Anchor interval: 1024 ticks

use crate::types::{
    AnchorTick, DecodedTick, TradeTick, ANCHOR_TICK_SIZE, BASE_DELTA_SIZE,
    OVERFLOW_DELTA_SIZE, OVERFLOW_ESCAPE, TIMESTAMP_DELTA_MASK, TIMESTAMP_EXTRA_SHIFT,
};
use crate::types::{PRICE_ZIGZAG_MASK, PRICE_ZIGZAG_SHIFT};

/// Maximum timestamp delta that fits in 20 bits.
pub const MAX_TIMESTAMP_DELTA: u32 = TIMESTAMP_DELTA_MASK; // 0xFFFFF = 1,048,575

// =============================================================================
// Zigzag encoding
// =============================================================================

/// Encode a signed i32 into an unsigned zigzag u32.
#[inline]
pub fn zigzag_encode(n: i32) -> u32 {
    ((n << 1) ^ (n >> 31)) as u32
}

/// Decode a zigzag u32 back to signed i32.
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
    if ts_delta > MAX_TIMESTAMP_DELTA {
        return true;
    }
    if !(-(131_072)..=131_071).contains(&price_delta) {
        return true;
    }
    if new_side != prev_side || new_flags != prev_flags {
        return true;
    }
    false
}

/// The 18-bit signed zigzag range.
pub const PRICE_ZIGZAG_MIN: i32 = -(131_072);
pub const PRICE_ZIGZAG_MAX: i32 = 131_071;

// =============================================================================
// Pack delta (tick -> bytes)
// =============================================================================

/// Result of packing a delta between two ticks.
#[derive(Debug, Clone)]
pub enum PackedDelta {
    /// 4-byte base delta record.
    Base([u8; 4]),
    /// 12-byte overflow delta record: [0xFF][2B ts_extra][8B i64 price_extra][1B size+side]
    Overflow([u8; 12]),
}

/// Pack a delta between the previous tick and the next tick.
///
/// Returns `PackedDelta::Base` (4 bytes) or `PackedDelta::Overflow` (8 bytes).
pub fn pack_delta(prev_tick: &TradeTick, next_tick: &TradeTick) -> PackedDelta {
    let full_ts_delta = next_tick.timestamp_ns - prev_tick.timestamp_ns;
    let price_delta = next_tick.price_int - prev_tick.price_int; // i64 for overflow
    let price_delta_i32 = price_delta as i32; // for base encoding check
    let ts_delta = full_ts_delta as u32;

    let needs_overflow = prev_tick.side != next_tick.side
        || prev_tick.flags != next_tick.flags
        || full_ts_delta > MAX_TIMESTAMP_DELTA as u64
        || !(PRICE_ZIGZAG_MIN..=PRICE_ZIGZAG_MAX).contains(&price_delta_i32);

    if needs_overflow {
        // Overflow encoding splits timestamp delta:
        // - If ts_delta < 2^15 (32768): store full delta in ts_extra (marker=0)
        // - If ts_delta >= 2^15: store upper bits in ts_extra (marker=1)
        // This preserves small deltas when overflow is triggered by side/price change.
        let ts_delta_u64 = full_ts_delta;
        let ts_delta_u32 = ts_delta_u64 as u32; // safe since MAX_TIMESTAMP_DELTA < u32::MAX

        let ts_extra_raw: u16;
        if ts_delta_u32 < 0x8000 {
            // Small delta: store directly with marker=0
            ts_extra_raw = (ts_delta_u32 << 1) as u16; // marker bit 0
        } else {
            // Large delta: store upper bits with marker=1
            // Encode: ts_extra_raw = (extra_bits & 0x7FFF) | 0x8000
            // Decode: ts_extra = (ts_extra_raw & 0x7FFF) << TIMESTAMP_EXTRA_SHIFT
            let extra_bits = (ts_delta_u64 >> TIMESTAMP_EXTRA_SHIFT) as u32;
            ts_extra_raw = ((extra_bits & 0x7FFF) as u16) | 0x8000_u16;
        }

        // price_extra: 64-bit signed delta (i64 to support ±1M delta at 1e6 precision)
        let price_extra = price_delta;

        // size_extra: 1-byte with sign-magnitude encoding:
        // bit 7 = side (0 or 1)
        // bit 6 = size sign (1 = negative, 0 = positive)
        // bits 0-5 = size magnitude (0-63)
        // Range: -127 to +63 (clamp beyond)
        let size_delta = (next_tick.size_int - prev_tick.size_int) as i32;
        let size_clamped = size_delta.clamp(-127, 63) as i8;
        let size_sign = if size_clamped < 0 { 1u8 } else { 0u8 };
        let size_magnitude = (size_clamped.abs() as u8) & 0x3F; // 6 bits for magnitude
        let size_byte = (((next_tick.side & 1) as u8) << 7)
            | (size_sign << 6)
            | size_magnitude;

        // Pack into 12 bytes: [0xFF][ts_extra 2B][price_extra i64][size_byte]
        let mut bytes = [0u8; 12];
        bytes[0] = OVERFLOW_ESCAPE;
        bytes[1] = (ts_extra_raw & 0xFF) as u8;
        bytes[2] = ((ts_extra_raw >> 8) & 0xFF) as u8;
        bytes[3..11].copy_from_slice(&price_extra.to_le_bytes()[..]);
        bytes[11] = size_byte;

        PackedDelta::Overflow(bytes)
    } else {
        // Base: 4 bytes
        let price_zigzag = zigzag_encode(price_delta_i32) & PRICE_ZIGZAG_MASK;
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
#[inline]
pub fn unpack_base_delta(bytes: &[u8; 4], prev_tick: &TradeTick, sequence: u32) -> DecodedTick {
    let packed = u32::from_le_bytes(*bytes);

    let ts_delta = packed & TIMESTAMP_DELTA_MASK;
    let price_zigzag_raw = (packed >> PRICE_ZIGZAG_SHIFT) & PRICE_ZIGZAG_MASK;

    // Sign-extend from 18 bits to i32 for proper zigzag decode
    let price_zigzag = if price_zigzag_raw & (1 << 17) != 0 {
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
        bytes_consumed: BASE_DELTA_SIZE,
    }
}

/// Decode an overflow (12-byte) delta record.
#[inline]
pub fn unpack_overflow_delta(
    bytes: &[u8; 12],
    prev_tick: &TradeTick,
    sequence: u32,
) -> DecodedTick {
    // byte 0 = 0xFF already verified by caller
    // Layout: [0xFF][2B ts_extra][8B price_extra i64][1B size+side]
    let ts_extra_raw = u16::from_le_bytes([bytes[1], bytes[2]]);
    let price_extra = i64::from_le_bytes([bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10]]);
    let size_byte = bytes[11];

    // ts_extra encoding:
    // - marker=0 (bit 15 = 0): ts_extra = ts_extra_raw >> 1 (direct small delta, < 32768)
    // - marker=1 (bit 15 = 1): ts_extra = (ts_extra_raw & 0x7FFF) << TIMESTAMP_EXTRA_SHIFT
    let ts_extra_raw = u16::from_le_bytes([bytes[1], bytes[2]]);
    let ts_extra = if (ts_extra_raw & 0x8000) == 0 {
        // Small delta stored directly
        (ts_extra_raw >> 1) as u64
    } else {
        // Upper bits of large delta:
        // Encode: ts_extra_raw = (extra_bits & 0x7FFF) | 0x8000
        // Decode: extract lower 15 bits and shift up
        //         ts_extra = (ts_extra_raw & 0x7FFF) << TIMESTAMP_EXTRA_SHIFT
        ((ts_extra_raw & 0x7FFF) as u64) << TIMESTAMP_EXTRA_SHIFT
    };

    let timestamp_ns = prev_tick.timestamp_ns + ts_extra;
    let price_int = prev_tick.price_int + price_extra as i64;

    // Decode side from bit 7
    let side = (size_byte >> 7) as u8;
    // Decode size from bits 6 (sign) and 0-5 (magnitude)
    // size_sign = bit 6, size_mag = bits 0-5
    let size_sign = if (size_byte & 0x40) != 0 { -1 } else { 1 };
    let size_magnitude = (size_byte & 0x3F) as i32;
    let size_int = prev_tick.size_int + (size_sign * size_magnitude) as i64;

    let flags = prev_tick.flags; // overflow preserves flags from prev

    DecodedTick {
        timestamp_ns,
        price_int,
        size_int,
        side,
        flags,
        sequence,
        bytes_consumed: OVERFLOW_DELTA_SIZE,
    }
}

/// Decode a delta record at the given byte position.
pub fn unpack_delta_at(
    data: &[u8],
    pos: usize,
    prev_tick: &TradeTick,
    sequence: u32,
) -> Result<DecodedTick, CompressionError> {
    if data[pos] == OVERFLOW_ESCAPE {
        if data.len() < pos + OVERFLOW_DELTA_SIZE {
            return Err(CompressionError::UnexpectedEndOfFile);
        }
        let mut bytes = [0u8; 12];
        bytes.copy_from_slice(&data[pos..pos + 12]);
        Ok(unpack_overflow_delta(&bytes, prev_tick, sequence))
    } else {
        if data.len() < pos + BASE_DELTA_SIZE {
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
        data[pos], data[pos + 1], data[pos + 2], data[pos + 3],
        data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7],
    ]);
    let price_int = i64::from_le_bytes([
        data[pos + 8], data[pos + 9], data[pos + 10], data[pos + 11],
        data[pos + 12], data[pos + 13], data[pos + 14], data[pos + 15],
    ]);
    let size_int = i64::from_le_bytes([
        data[pos + 16], data[pos + 17], data[pos + 18], data[pos + 19],
        data[pos + 20], data[pos + 21], data[pos + 22], data[pos + 23],
    ]);
    let side = data[pos + 24];
    let flags = data[pos + 25];
    let sequence = u32::from_le_bytes([
        data[pos + 26], data[pos + 27], data[pos + 28], data[pos + 29],
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
            CompressionError::InvalidTimestampDelta => write!(f, "Invalid timestamp delta encoding"),
        }
    }
}

impl std::error::Error for CompressionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zigzag_encode_decode() {
        for n in &[0i32, 1, -1, 127, -128, 131071, -131072, 1000000, -1000000] {
            let encoded = zigzag_encode(*n);
            let decoded = zigzag_decode(encoded);
            assert_eq!(decoded, *n, "zigzag roundtrip failed for {}", n);
        }
    }

    #[test]
    fn test_zigzag_known_values() {
        assert_eq!(zigzag_encode(0), 0);
        assert_eq!(zigzag_encode(1), 2);
        assert_eq!(zigzag_encode(-1), 1);
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
                assert_eq!(bytes.len(), 12);
                let decoded = unpack_overflow_delta(&bytes, &prev, 1);
                // With 21-bit shift quantization, error per overflow is at most ~2^21 = 2.1M ns
                // For 2B delta with 21-bit shift: extra_bits=953, decoded=953<<21=1998585856
                // error = 2B - 1998585856 = 1,414,144 ns
                let ts_diff = (decoded.timestamp_ns as i64 - next.timestamp_ns as i64).abs();
                assert!(ts_diff < 5_000_000, "ts_diff = {} (expected < 5M)", ts_diff);
                assert_eq!(decoded.price_int, next.price_int);
                assert_eq!(decoded.bytes_consumed, 12);
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
    fn test_60_second_interval_overflow() {
        // 60 seconds = 60_000_000_000 ns
        // This should trigger overflow since it exceeds 20-bit max (~1ms)
        let prev = TradeTick::new(1_000_000_000, 100_000_000_000i64, 1_000_000i64, 0, 1, 0);
        let next = TradeTick::new(61_000_000_000u64, 100_000_000_000i64, 1_000_000i64, 0, 1, 1);

        let packed = pack_delta(&prev, &next);
        match packed {
            PackedDelta::Overflow(bytes) => {
                assert_eq!(bytes.len(), 12);
                // Verify ts_extra encoding
                let ts_extra_raw = u16::from_le_bytes([bytes[1], bytes[2]]);
                assert!(ts_extra_raw & 0x8000 != 0); // marker bit set
            }
            PackedDelta::Base(_) => panic!("60s interval should overflow"),
        }
    }
}