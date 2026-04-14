//! TVC3 binary format type definitions.
//!
//! Layout references:
//! - Header: 128 bytes `#[repr(C, packed)]`
//! - AnchorTick: 30 bytes `#[repr(C, packed)]`
//! - AnchorIndexEntry: 16 bytes `#[repr(C, packed)]` (matches Nautilus production)
//!
//! Nautilus source: `persistence/tvc_cython_loader.pyx` lines 55-69
//! Nautilus source: `persistence/tvc_mmap_loader.py` lines 52-85

use static_assertions::const_assert;
use std::fmt;

// =============================================================================
// Constants
// =============================================================================

pub const HEADER_SIZE: usize = 128;
pub const ANCHOR_TICK_SIZE: usize = 30;
pub const INDEX_ENTRY_SIZE: usize = 16;
pub const OVERFLOW_ESCAPE: u8 = 0xFF;

// Bit field positions in 4-byte base delta
pub const TIMESTAMP_DELTA_BITS: u32 = 20;
pub const TIMESTAMP_DELTA_MASK: u32 = 0xFFFFF; // bits 0-19
pub const PRICE_ZIGZAG_BITS: u32 = 18;
pub const PRICE_ZIGZAG_MASK: u32 = 0x7FFFF; // bits 0-17 of the field
pub const PRICE_ZIGZAG_SHIFT: u32 = 20;
pub const SIDE_SHIFT: u32 = 38;
pub const FLAGS_SHIFT: u32 = 39;

// Timestamp overflow: 15 bits extra at bit 20
pub const TIMESTAMP_EXTRA_SHIFT: u32 = 20;

// FIXED_SCALAR for standard precision
pub const FIXED_SCALAR: i64 = 1_000_000_000;
pub const PRICE_PRECISION: u8 = 9;
pub const SIZE_PRECISION: u8 = 6;

// =============================================================================
// TvcHeader — 128 bytes
// =============================================================================

/// TVC3 file header — 128 bytes `#[repr(C, packed)]`
///
/// Byte layout (matching Nautilus `tvc_cython_loader.pyx` lines 174-185):
/// ```text
/// 0-3:   magic (4B: b"TVC3")
/// 4:     version (1B: u8 = 1)
/// 5:     decimal_precision (1B: u8)
/// 6-9:   anchor_interval (4B: u32)
/// 10-13: instrument_id (4B: u32, FNV-1a hash)
/// 14-21: start_time_ns (8B: u64)
/// 22-29: end_time_ns (8B: u64)
/// 30-37: num_ticks (8B: u64)
/// 38-41: num_anchors (4B: u32)
/// 42-49: index_offset (8B: u64, byte offset of index at EOF)
/// 50-127: reserved (78B: u8)
/// ```
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct TvcHeader {
    pub magic: [u8; 4],           // 0-3: b"TVC3"
    pub version: u8,                // 4: must be 1
    pub decimal_precision: u8,      // 5: price decimal places
    pub anchor_interval: u32,       // 6-9: ticks per anchor
    pub instrument_id: u32,        // 10-13: FNV-1a hash of symbol
    pub start_time_ns: u64,         // 14-21: first tick timestamp
    pub end_time_ns: u64,           // 22-29: last tick timestamp
    pub num_ticks: u64,             // 30-37: total tick count
    pub num_anchors: u32,           // 38-41: number of anchors
    pub index_offset: u64,          // 42-49: byte offset of index at EOF
    pub reserved: [u8; 78],         // 50-127: zeros
}

const_assert!(std::mem::size_of::<TvcHeader>() == HEADER_SIZE);

impl TvcHeader {
    /// TVC3 magic bytes: b"TVC3"
    pub const MAGIC: [u8; 4] = *b"TVC3";
    pub const SUPPORTED_VERSION: u8 = 1;

    /// Create a new header with default values.
    pub fn new(instrument_id: u32, anchor_interval: u32, decimal_precision: u8) -> Self {
        Self {
            magic: Self::MAGIC,
            version: Self::SUPPORTED_VERSION,
            decimal_precision,
            anchor_interval,
            instrument_id,
            start_time_ns: 0,
            end_time_ns: 0,
            num_ticks: 0,
            num_anchors: 0,
            index_offset: 0,
            reserved: [0u8; 78],
        }
    }

    /// Validate header magic and version.
    pub fn validate(&self) -> Result<(), HeaderError> {
        if self.magic != Self::MAGIC {
            return Err(HeaderError::InvalidMagic(self.magic));
        }
        if self.version != Self::SUPPORTED_VERSION {
            return Err(HeaderError::UnsupportedVersion(self.version));
        }
        Ok(())
    }
}

impl fmt::Display for TvcHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let instrument_id = self.instrument_id;
        let num_ticks = self.num_ticks;
        let num_anchors = self.num_anchors;
        let anchor_interval = self.anchor_interval;
        write!(f, "TvcHeader {{ instrument_id: {}, num_ticks: {}, num_anchors: {}, anchor_interval: {} }}", instrument_id, num_ticks, num_anchors, anchor_interval)
    }
}

#[derive(Debug, Clone)]
pub enum HeaderError {
    InvalidMagic([u8; 4]),
    UnsupportedVersion(u8),
}

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HeaderError::InvalidMagic(m) => {
                write!(f, "Invalid TVC magic: {:?}", m)
            }
            HeaderError::UnsupportedVersion(v) => {
                write!(f, "Unsupported TVC version: {}", v)
            }
        }
    }
}

impl std::error::Error for HeaderError {}

// =============================================================================
// AnchorTick — 30 bytes
// =============================================================================

/// Full anchor tick — 30 bytes `#[repr(C, packed)]`
///
/// Stored every `anchor_interval` ticks as a reference point for delta decoding.
///
/// Byte layout:
/// ```text
/// 0-7:  timestamp_ns (8B: u64)
/// 8-15: price_int (8B: u64, fixed-point integer)
/// 16-23: size_int (8B: u64, fixed-point integer)
/// 24:   side (1B: u8, 0=Buy, 1=Sell)
/// 25:   flags (1B: u8)
/// 26-29: sequence (4B: u32, tick index within file)
/// ```
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnchorTick {
    pub timestamp_ns: u64,  // 0-7
    pub price_int: i64,      // 8-15: signed for zigzag delta
    pub size_int: i64,       // 16-23: signed for delta
    pub side: u8,           // 24: 0=Buy, 1=Sell
    pub flags: u8,          // 25: 1=trade
    pub sequence: u32,     // 26-29: tick index
}

const_assert!(std::mem::size_of::<AnchorTick>() == ANCHOR_TICK_SIZE);

impl AnchorTick {
    /// Create a new anchor tick.
    pub fn new(timestamp_ns: u64, price_int: i64, size_int: i64, side: u8, flags: u8, sequence: u32) -> Self {
        Self {
            timestamp_ns,
            price_int,
            size_int,
            side,
            flags,
            sequence,
        }
    }
}

impl fmt::Display for AnchorTick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ts = self.timestamp_ns;
        let price = self.price_int;
        let size = self.size_int;
        let side = self.side;
        let seq = self.sequence;
        write!(f, "AnchorTick {{ ts: {}, price: {}, size: {}, side: {}, seq: {} }}", ts, price, size, side, seq)
    }
}

// =============================================================================
// AnchorIndexEntry — 16 bytes
// =============================================================================

/// Anchor index entry — 16 bytes `#[repr(C, packed)]`
///
/// This is NOT 24 bytes. Nautilus production uses 16 bytes (tick_index + byte_offset).
/// TVC-OMC incorrectly used 24 bytes with redundant timestamp.
///
/// Byte layout:
/// ```text
/// 0-7:  tick_index (8B: u64, cumulative tick number)
/// 8-15: byte_offset (8B: u64, byte position of anchor within file)
/// ```
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnchorIndexEntry {
    pub tick_index: u64,   // 0-7: tick number within file
    pub byte_offset: u64, // 8-15: byte offset of anchor in file
}

const_assert!(std::mem::size_of::<AnchorIndexEntry>() == INDEX_ENTRY_SIZE);

impl AnchorIndexEntry {
    pub fn new(tick_index: u64, byte_offset: u64) -> Self {
        Self { tick_index, byte_offset }
    }
}

impl fmt::Display for AnchorIndexEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tick = self.tick_index;
        let offset = self.byte_offset;
        write!(f, "AnchorIndexEntry {{ tick: {}, offset: {} }}", tick, offset)
    }
}

// =============================================================================
// TradeTick — in-memory representation
// =============================================================================

/// In-memory trade tick representation.
///
/// Price and size are stored as fixed-point integers:
/// - price_int = round(price * 10^decimal_precision)
/// - size_int = round(size * 10^SIZE_PRECISION)
///
/// `price_precision` comes from the TVC header's `decimal_precision` field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TradeTick {
    pub timestamp_ns: u64,
    pub price_int: i64,
    pub size_int: i64,
    pub side: u8,      // 0=Buy (aggressor is buy), 1=Sell
    pub flags: u8,     // 1=trade
    pub sequence: u32,
}

impl TradeTick {
    pub fn new(timestamp_ns: u64, price_int: i64, size_int: i64, side: u8, flags: u8, sequence: u32) -> Self {
        Self {
            timestamp_ns,
            price_int,
            size_int,
            side,
            flags,
            sequence,
        }
    }

    /// Convert price_int to floating point.
    pub fn price(&self, precision: u8) -> f64 {
        self.price_int as f64 / 10f64.powi(precision as i32)
    }

    /// Convert size_int to floating point.
    pub fn size(&self) -> f64 {
        self.size_int as f64 / 10f64.powi(SIZE_PRECISION as i32)
    }

    /// Create from floating point values.
    pub fn from_floats(
        timestamp_ns: u64,
        price: f64,
        size: f64,
        side: u8,
        precision: u8,
        sequence: u32,
    ) -> Self {
        let price_int = (price * 10f64.powi(precision as i32)).round() as i64;
        let size_int = (size * 10f64.powi(SIZE_PRECISION as i32)).round() as i64;
        Self {
            timestamp_ns,
            price_int,
            size_int,
            side,
            flags: 1, // always a trade
            sequence,
        }
    }
}

impl fmt::Display for TradeTick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TradeTick {{ ts: {}, price_int: {}, size_int: {}, side: {}, seq: {} }}",
            self.timestamp_ns, self.price_int, self.size_int, self.side, self.sequence
        )
    }
}

// =============================================================================
// PackedDelta — decoded delta record
// =============================================================================

/// Result of decoding a delta record.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecodedTick {
    pub timestamp_ns: u64,
    pub price_int: i64,
    pub size_int: i64,
    pub side: u8,
    pub flags: u8,
    pub sequence: u32,
    pub bytes_consumed: usize, // 4 for base, 15 for overflow
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_size() {
        assert_eq!(std::mem::size_of::<TvcHeader>(), HEADER_SIZE);
    }

    #[test]
    fn test_anchor_tick_size() {
        assert_eq!(std::mem::size_of::<AnchorTick>(), ANCHOR_TICK_SIZE);
    }

    #[test]
    fn test_index_entry_size() {
        assert_eq!(std::mem::size_of::<AnchorIndexEntry>(), INDEX_ENTRY_SIZE);
    }

    #[test]
    fn test_header_magic() {
        let h = TvcHeader::new(0, 1000, 9);
        assert_eq!(h.magic, *b"TVC3");
        assert_eq!(h.version, 1);
    }

    #[test]
    fn test_header_validate_ok() {
        let h = TvcHeader::new(0, 1000, 9);
        assert!(h.validate().is_ok());
    }

    #[test]
    fn test_header_validate_bad_magic() {
        let mut h = TvcHeader::new(0, 1000, 9);
        h.magic = *b"XXXX";
        assert!(matches!(h.validate(), Err(HeaderError::InvalidMagic(_))));
    }

    #[test]
    fn test_trade_tick_from_floats() {
        let tick = TradeTick::from_floats(1_000_000_000, 100.5, 0.1, 0, 2, 0);
        assert_eq!(tick.price_int, 10050); // 100.5 * 10^2
        assert_eq!(tick.size_int, 100_000); // 0.1 * 10^6
        assert_eq!(tick.price(2), 100.5);
    }
}
