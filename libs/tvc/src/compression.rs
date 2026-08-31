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
    AnchorTick, DecodedTick, TradeTick, ANCHOR_TICK_SIZE, BASE_DELTA_SIZE, OVERFLOW_DELTA_SIZE,
    OVERFLOW_ESCAPE, TIMESTAMP_DELTA_MASK,
};
use crate::types::{
    PRICE_ZIGZAG_MASK, PRICE_ZIGZAG_SHIFT, SIZE_ZIGZAG_MASK, SIZE_ZIGZAG_SHIFT,
};

/// Maximum timestamp delta that fits in the base-path µs field.
/// 17 bits in 1µs units = 131,071 µs ≈ 131 milliseconds.
pub const MAX_TIMESTAMP_DELTA_US: u32 = TIMESTAMP_DELTA_MASK; // 0x1FFFF = 131,071 µs

/// 18-bit signed zigzag range.
pub const PRICE_ZIGZAG_MIN: i32 = -(131_072);
pub const PRICE_ZIGZAG_MAX: i32 = 131_071;

/// 27-bit signed zigzag range for size delta.
pub const SIZE_ZIGZAG_MIN: i32 = -(1 << 26);
pub const SIZE_ZIGZAG_MAX: i32 = (1 << 26) - 1;

// =============================================================================
// Zigzag encoding
// =============================================================================

/// Encode a signed i64 as zigzag into u64.
///
/// Use this for the overflow path where ts_delta can exceed i32 range (up to ±2.1s
/// before saturation). For ts_delta in the ±i32 range, prefer the u32 version which
/// is identical.
#[inline]
pub fn zigzag_encode_i64(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)) as u64
}

/// Decode a zigzag u64 back to signed i64.
#[inline]
pub fn zigzag_decode_i64(n: u64) -> i64 {
    let n = n as i64;
    (n >> 1) ^ -(n & 1)
}

/// Encode a signed i32 as zigzag u32.
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
/// Returns true if the encoder should use the overflow path:
/// - timestamp delta > 131 seconds
/// - price delta out of 18-bit zigzag range
/// - size delta out of 27-bit zigzag range
/// (side and flags no longer trigger overflow; they fit in base bits 62-63.)
#[inline]
pub fn needs_overflow(
    ts_delta_us: u32,
    price_delta: i32,
    size_delta: i32,
) -> bool {
    if ts_delta_us > MAX_TIMESTAMP_DELTA_US {
        return true;
    }
    if !(PRICE_ZIGZAG_MIN..=PRICE_ZIGZAG_MAX).contains(&price_delta) {
        return true;
    }
    if !(SIZE_ZIGZAG_MIN..=SIZE_ZIGZAG_MAX).contains(&size_delta) {
        return true;
    }
    false
}

// =============================================================================
// Pack delta (tick -> bytes)
// =============================================================================

/// Result of packing a delta between two ticks.
#[derive(Debug, Clone)]
pub enum PackedDelta {
    /// 8-byte base delta record (64 bits packed): 17 ts_ms + 18 price + 27 size + 2 side/flags.
    Base([u8; 8]),
    /// 14-byte overflow delta record: [0xFF][4B ts_i32][4B price_i32][4B size_i32][1B side+flags]
    Overflow([u8; 14]),
}

/// Pack a delta between the previous tick and the next tick.
///
/// Returns `PackedDelta::Base` (8 bytes) or `PackedDelta::Overflow` (14 bytes).
/// Base path covers ts_delta up to 131 seconds (17 bits in 1ms units) and
/// stores full size, price, side, and flags. Overflow is reserved for ticks where
/// ts_delta exceeds 131s, or price/size deltas exceed 18/27-bit zigzag range.
pub fn pack_delta(prev_tick: &TradeTick, next_tick: &TradeTick) -> PackedDelta {
    let full_ts_delta_ns = next_tick.timestamp_ns.saturating_sub(prev_tick.timestamp_ns);
    let ts_delta_us = (full_ts_delta_ns / 1_000) as u32;
    let price_delta = next_tick.price_int - prev_tick.price_int;
    let price_delta_i32 = price_delta as i32;
    let size_delta = next_tick.size_int - prev_tick.size_int;
    let size_delta_i32 = size_delta as i32;

    let needs_overflow = ts_delta_us > MAX_TIMESTAMP_DELTA_US
        || !(PRICE_ZIGZAG_MIN..=PRICE_ZIGZAG_MAX).contains(&price_delta_i32)
        || !(SIZE_ZIGZAG_MIN..=SIZE_ZIGZAG_MAX).contains(&size_delta_i32);

    if needs_overflow {
        // 14-byte overflow layout:
        //   byte 0:    0xFF escape marker
        //   bytes 1-4: ts_extra u32 zigzag in MICROSECONDS (covers ±2147s range)
        //   bytes 5-8: price_extra i32 zigzag (full ns precision nano-units)
        //   bytes 9-12: size_extra i32 zigzag (full ns precision nano-units)
        //   byte 13:   side (bit 0) | flags (bits 1-7)
        //
        // ts in microseconds (not nanoseconds) because the 4-byte u32 field can
        // only hold ±2.1s in ns but ±2147s in µs. This is lossless for any
        // realistic market data gap. Sub-µs precision is invisible to strategies.
        let ts_delta_us = full_ts_delta_ns / 1_000;
        let ts_extra = zigzag_encode(ts_delta_us as i32);
        let price_extra = zigzag_encode(price_delta_i32);
        let size_extra = zigzag_encode(size_delta_i32);

        let mut bytes = [0u8; 14];
        bytes[0] = OVERFLOW_ESCAPE;
        bytes[1..5].copy_from_slice(&ts_extra.to_le_bytes()[..]);
        bytes[5..9].copy_from_slice(&price_extra.to_le_bytes()[..]);
        bytes[9..13].copy_from_slice(&size_extra.to_le_bytes()[..]);
        bytes[13] = (next_tick.side & 1) | ((next_tick.flags & 0x7F) << 1);

        PackedDelta::Overflow(bytes)
    } else {
        // Base: 8 bytes (64 bits packed)
        // bits 0-16:   ts_delta in 1ms units (17 bits, max 131 seconds)
        // bits 17-34:  price_zigzag (18 bits, zigzag i32, ±131k)
        // bits 35-61:  size_zigzag (27 bits, zigzag i32, ±67M nano-BTC = ±$6,300 at BTC=94k)
        // bit 62:      side (1 bit)
        // bit 63:      flags (1 bit)
        let price_zigzag = zigzag_encode(price_delta_i32) & PRICE_ZIGZAG_MASK;
        let size_zigzag = zigzag_encode(size_delta_i32) & SIZE_ZIGZAG_MASK;
        let side_bit = (next_tick.side as u64) & 1;
        let flags_bit = (next_tick.flags as u64) & 1;
        let packed = (ts_delta_us as u64)
            | ((price_zigzag as u64) << PRICE_ZIGZAG_SHIFT)
            | ((size_zigzag as u64) << SIZE_ZIGZAG_SHIFT)
            | (side_bit << 62)
            | (flags_bit << 63);
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&packed.to_le_bytes()[..8]);
        PackedDelta::Base(buf)
    }
}

// =============================================================================
// Unpack delta (bytes -> tick)
// =============================================================================

/// Decode a base (8-byte) delta record.
///
/// Bit layout (matching the encoder):
///   bits 0-16:   ts_delta in 1ms units (17 bits, max 131 seconds)
///   bits 17-34:  price_zigzag (18 bits)
///   bits 35-61:  size_zigzag (27 bits)
///   bit 62:      side (1 bit)
///   bit 63:      flags (1 bit)
#[inline]
pub fn unpack_base_delta(bytes: &[u8; 8], prev_tick: &TradeTick, sequence: u32) -> DecodedTick {
    let packed = u64::from_le_bytes(*bytes);

    let ts_delta_us = (packed & TIMESTAMP_DELTA_MASK as u64) as u32;
    let price_zigzag_raw = (packed >> PRICE_ZIGZAG_SHIFT) & (PRICE_ZIGZAG_MASK as u64);
    let size_zigzag_raw = (packed >> SIZE_ZIGZAG_SHIFT) & (SIZE_ZIGZAG_MASK as u64);

    // Sign-extend 18-bit price_zigzag to i32 for zigzag decode
    let price_zigzag = if price_zigzag_raw & (1 << 17) != 0 {
        (price_zigzag_raw as i64 | 0xFFFFFC0000_i64) as i32
    } else {
        price_zigzag_raw as i32
    };
    // Sign-extend 27-bit size_zigzag to i32 for zigzag decode
    let size_zigzag = if size_zigzag_raw & (1 << 26) != 0 {
        (size_zigzag_raw as i64 | 0xFFFFFFFFF8000000_u64 as i64) as i32
    } else {
        size_zigzag_raw as i32
    };

    let price_delta = zigzag_decode(price_zigzag as u32);
    let size_delta = zigzag_decode(size_zigzag as u32);

    // Decode side (bit 62) and flags (bit 63) from packed
    let side = ((packed >> 62) & 1) as u8;
    let flags = ((packed >> 63) & 1) as u8;

    DecodedTick {
        timestamp_ns: prev_tick.timestamp_ns + (ts_delta_us as u64) * 1_000,
        price_int: prev_tick.price_int + price_delta as i64,
        size_int: prev_tick.size_int + size_delta as i64,
        side,
        flags,
        sequence,
        bytes_consumed: BASE_DELTA_SIZE,
    }
}

/// Decode an overflow (14-byte) delta record.
#[inline]
pub fn unpack_overflow_delta(
    bytes: &[u8; 14],
    prev_tick: &TradeTick,
    sequence: u32,
) -> DecodedTick {
    // byte 0 = 0xFF already verified by caller
    // Layout: [0xFF][4B ts_u32 zigzag µs][4B price_i32 zigzag][4B size_i32 zigzag][1B side+flags]
    let mut ts_buf = [0u8; 8];
    ts_buf[..4].copy_from_slice(&bytes[1..5]);
    let ts_delta_raw = u64::from_le_bytes(ts_buf);
    let price_extra_raw = i32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]);
    let size_extra_raw = i32::from_le_bytes([bytes[9], bytes[10], bytes[11], bytes[12]]);

    // ts is stored as zigzag(i32) microseconds. Decode and convert to nanoseconds.
    let ts_delta_us = zigzag_decode(ts_delta_raw as u32);
    let timestamp_ns = (prev_tick.timestamp_ns as i64)
        .wrapping_add((ts_delta_us as i64) * 1_000)
        as u64;
    let price_int = prev_tick.price_int + zigzag_decode(price_extra_raw as u32) as i64;
    let size_int = prev_tick.size_int + zigzag_decode(size_extra_raw as u32) as i64;

    // Decode side (bit 0) and flags (bits 1-7) from final byte
    let side = bytes[13] & 1;
    let flags = (bytes[13] >> 1) & 0x7F;

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
        let mut bytes = [0u8; 14];
        bytes.copy_from_slice(&data[pos..pos + 14]);
        Ok(unpack_overflow_delta(&bytes, prev_tick, sequence))
    } else {
        if data.len() < pos + BASE_DELTA_SIZE {
            return Err(CompressionError::UnexpectedEndOfFile);
        }
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&data[pos..pos + BASE_DELTA_SIZE]);
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
        // Within limits: no overflow
        assert!(!needs_overflow(1_000, 100, 100)); // ts=1ms, small price/size
        // Timestamp exceeds 17-bit µs range (max 131071 µs = 131ms)
        assert!(needs_overflow(200_000, 100, 100));
        // Price exceeds 18-bit signed zigzag
        assert!(needs_overflow(1_000, 200_000, 100));
        // Size exceeds 27-bit signed zigzag
        assert!(needs_overflow(1_000, 100, 200_000_000));
    }

    #[test]
    fn test_pack_unpack_base_delta() {
        // ts_delta = 1ms = 1_000_000 ns (small enough for 17-bit ms encoding)
        let prev = TradeTick::new(1_700_000_000_000, 100_000_000_000i64, 1_000_000i64, 0, 1, 0);
        let next = TradeTick::new(1_700_001_000_000, 100_000_000_100i64, 1_000_100i64, 0, 1, 1);

        let packed = pack_delta(&prev, &next);
        match packed {
            PackedDelta::Base(bytes) => {
                let decoded = unpack_base_delta(&bytes, &prev, 1);
                assert_eq!(decoded.timestamp_ns, next.timestamp_ns);
                assert_eq!(decoded.price_int, next.price_int);
                assert_eq!(decoded.size_int, next.size_int);
                assert_eq!(decoded.bytes_consumed, 8);
                assert_eq!(decoded.side, next.side);
                assert_eq!(decoded.flags, next.flags);
            }
            PackedDelta::Overflow(_) => panic!("Expected base delta, got overflow"),
        }
    }

    #[test]
    fn test_pack_unpack_overflow_delta() {
        // Overflow path is for ts_delta > 131 seconds (base limit).
        // Use 200-second gap to ensure overflow triggers.
        // 200_000_000_000 ns = 200_000 ms > 131_071 ms.
        let prev = TradeTick::new(1_700_000_000_000, 100_000_000_000i64, 1_000_000i64, 0, 1, 0);
        // 200s = 200_000_000_000 ns added.
        let next = TradeTick::new(1_900_000_000_000u64, 100_000_000_500i64, 1_000_500i64, 1, 0, 1);

        let packed = pack_delta(&prev, &next);
        match packed {
            PackedDelta::Overflow(bytes) => {
                assert_eq!(bytes[0], OVERFLOW_ESCAPE);
                assert_eq!(bytes.len(), 14);
                let decoded = unpack_overflow_delta(&bytes, &prev, 1);
                assert_eq!(decoded.timestamp_ns, next.timestamp_ns);
                assert_eq!(decoded.price_int, next.price_int);
                assert_eq!(decoded.size_int, next.size_int);
                assert_eq!(decoded.side, next.side);
                assert_eq!(decoded.flags, next.flags);
                assert_eq!(decoded.bytes_consumed, 14);
            }
            PackedDelta::Base(_) => panic!("Expected overflow delta, got base"),
        }
    }

    #[test]
    fn test_base_path_carries_side_and_flags() {
        // Regression: base path encodes side+flags at bits 62-63.
        let prev = TradeTick::new(1_700_000_000_000, 100_000_000_000i64, 1_000_000i64, 0, 1, 0);
        let next = TradeTick::new(1_700_001_000_000, 100_000_000_100i64, 1_000_100i64, 1, 0, 1);

        let packed = pack_delta(&prev, &next);
        match packed {
            PackedDelta::Base(bytes) => {
                let decoded = unpack_base_delta(&bytes, &prev, 1);
                assert_eq!(decoded.side, 1, "side=1 should round-trip through base path");
                assert_eq!(decoded.flags, 0, "flags=0 should round-trip through base path");
                assert_eq!(decoded.size_int, next.size_int);
            }
            PackedDelta::Overflow(_) => panic!("Side/flags change should use base path, not overflow"),
        }
    }

    #[test]
    fn test_200_second_interval_overflow() {
        // 200 seconds = 200_000 ms > 131_071 ms (17-bit limit) — triggers overflow.
        let prev = TradeTick::new(1_700_000_000_000, 100_000_000_000i64, 1_000_000i64, 0, 1, 0);
        let next = TradeTick::new(1_900_000_000_000u64, 100_000_000_000i64, 1_000_000i64, 0, 1, 1);

        let packed = pack_delta(&prev, &next);
        match packed {
            PackedDelta::Overflow(bytes) => {
                assert_eq!(bytes.len(), 14);
                let decoded = unpack_overflow_delta(&bytes, &prev, 1);
                assert_eq!(decoded.timestamp_ns, next.timestamp_ns);
                assert_eq!(decoded.price_int, next.price_int);
            }
            PackedDelta::Base(_) => panic!("200s interval should overflow"),
        }
    }
}