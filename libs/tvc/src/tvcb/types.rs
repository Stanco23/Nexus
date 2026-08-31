//! TVCB binary format type definitions.
//!
//! Layout references:
//! - TvcbHeader: 128 bytes `#[repr(C, packed)]`
//! - AnchorBar: 72 bytes `#[repr(C, packed)]`
//! - IndexEntry: 16 bytes `#[repr(C, packed)]`

use static_assertions::const_assert;
use std::fmt;

// =============================================================================
// Constants
// =============================================================================

/// Size of TvcbHeader in bytes.
pub const HEADER_SIZE: usize = 128;
/// Size of AnchorBar in bytes.
pub const ANCHOR_BAR_SIZE: usize = 72;
/// Size of IndexEntry in bytes.
pub const INDEX_ENTRY_SIZE: usize = 16;

/// TVCB magic bytes.
pub const TVCB_MAGIC: [u8; 4] = *b"TVCB";
/// Supported TVCB format version.
pub const TVCB_VERSION: u8 = 1;

/// Default decimal precision for price fields (nanodollars).
pub const DECIMAL_PRECISION: u8 = 9;

/// Fixed scalar for standard precision (10^9).
pub const FIXED_SCALAR: i64 = 1_000_000_000;

// =============================================================================
// TvcbHeader — 128 bytes
// =============================================================================

/// TVCB file header — 128 bytes `#[repr(C, packed)]`
///
/// Byte layout:
/// ```text
/// 0-3:     magic (4B: b"TVCB")
/// 4:       version (1B: u8 = 1)
/// 5:       decimal_precision (1B: u8, price decimal places, default 9)
/// 6-9:     anchor_interval (4B: u32, bars per anchor)
/// 10-13:   instrument_id (4B: u32, FNV-1a hash of symbol)
/// 14-21:   start_time_ns (8B: u64, first bar ts_event)
/// 22-29:   end_time_ns (8B: u64, last bar ts_event)
/// 30-37:   num_bars (8B: u64, total bar count)
/// 38-41:   num_anchors (4B: u32, anchor count)
/// 42-49:   index_offset (8B: u64, byte offset where index begins)
/// 50-57:   year (8B: u64, calendar year this file covers)
/// 58-65:   timeframe_ns (8B: u64, bar period in nanoseconds)
/// 66-127:  reserved (62B: u8, zeros)
/// ```
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct TvcbHeader {
    pub magic: [u8; 4],           // 0-3: b"TVCB"
    pub version: u8,               // 4: must be 1
    pub decimal_precision: u8,    // 5: price decimal places (9 = nanodollars)
    pub anchor_interval: u32,     // 6-9: bars per anchor
    pub instrument_id: u32,       // 10-13: FNV-1a hash of symbol
    pub start_time_ns: u64,       // 14-21: first bar ts_event
    pub end_time_ns: u64,         // 22-29: last bar ts_event
    pub num_bars: u64,            // 30-37: total bar count
    pub num_anchors: u32,         // 38-41: number of anchors
    pub index_offset: u64,         // 42-49: byte offset of index at EOF
    pub year: u64,                // 50-57: calendar year
    pub timeframe_ns: u64,        // 58-65: bar period in nanoseconds
    pub reserved: [u8; 62],       // 66-127: zeros
}

const_assert!(std::mem::size_of::<TvcbHeader>() == HEADER_SIZE);
const_assert!(std::mem::align_of::<TvcbHeader>() == 1);

impl TvcbHeader {
    /// Create a new header with default values.
    pub fn new(
        instrument_id: u32,
        anchor_interval: u32,
        decimal_precision: u8,
        year: u64,
        timeframe_ns: u64,
    ) -> Self {
        Self {
            magic: TVCB_MAGIC,
            version: TVCB_VERSION,
            decimal_precision,
            anchor_interval,
            instrument_id,
            start_time_ns: 0,
            end_time_ns: 0,
            num_bars: 0,
            num_anchors: 0,
            index_offset: 0,
            year,
            timeframe_ns,
            reserved: [0u8; 62],
        }
    }

    /// Validate header magic and version.
    pub fn validate(&self) -> Result<(), TvcbError> {
        if self.magic != TVCB_MAGIC {
            return Err(TvcbError::InvalidMagic(self.magic));
        }
        if self.version != TVCB_VERSION {
            return Err(TvcbError::UnsupportedVersion(self.version));
        }
        // Note: decimal_precision, anchor_interval, and index_offset validation
        // are performed during file read in TvcbReader::open()
        Ok(())
    }
}

impl fmt::Display for TvcbHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Copy fields to avoid creating references to packed struct fields
        let instrument_id = self.instrument_id;
        let num_bars = self.num_bars;
        let num_anchors = self.num_anchors;
        let anchor_interval = self.anchor_interval;
        let year = self.year;
        let timeframe_ns = self.timeframe_ns;
        write!(
            f,
            "TvcbHeader {{ instrument_id: {}, num_bars: {}, num_anchors: {}, anchor_interval: {}, year: {}, timeframe_ns: {} }}",
            instrument_id,
            num_bars,
            num_anchors,
            anchor_interval,
            year,
            timeframe_ns
        )
    }
}

// =============================================================================
// AnchorBar — 72 bytes
// =============================================================================

/// Full anchor bar — 72 bytes `#[repr(C, packed)]`
///
/// Stored every `anchor_interval` bars as a reference point for delta decoding.
///
/// Byte layout:
/// ```text
/// 0-7:    ts_event (8B: u64, bar open time in nanoseconds)
/// 8-15:   open (8B: i64, absolute price in nanounits)
/// 16-23:  high (8B: i64, absolute price in nanounits)
/// 24-31:  low (8B: i64, absolute price in nanounits)
/// 32-39:  close (8B: i64, absolute price in nanounits)
/// 40-47:  vwap (8B: i64, absolute price in nanounits)
/// 48-55:  volume (8B: i64, total volume)
/// 56-63:  buy_volume (8B: i64, buy-side volume)
/// 64-67:  tick_count (4B: u32, ticks comprising this bar)
/// 68-71:  bar_number (4B: u32, sequential bar index within file)
/// ```
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct AnchorBar {
    pub ts_event: u64,    // 0-7: bar open time (nanoseconds, UTC)
    pub open: i64,         // 8-15: absolute price (nanounits)
    pub high: i64,        // 16-23: absolute price (nanounits)
    pub low: i64,         // 24-31: absolute price (nanounits)
    pub close: i64,       // 32-39: absolute price (nanounits)
    pub vwap: i64,         // 40-47: absolute price (nanounits)
    pub volume: i64,      // 48-55: total volume
    pub buy_volume: i64,  // 56-63: buy-side volume
    pub tick_count: u32,  // 64-67: ticks comprising this bar
    pub bar_number: u32,  // 68-71: sequential bar index within file
}

const_assert!(std::mem::size_of::<AnchorBar>() == ANCHOR_BAR_SIZE);
const_assert!(std::mem::align_of::<AnchorBar>() == 1);

impl AnchorBar {
    /// Create a new anchor bar from a bar.
    pub fn from_bar(bar: &Bar, bar_number: u32) -> Self {
        Self {
            ts_event: bar.ts_event,
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            vwap: bar.vwap,
            volume: bar.volume,
            buy_volume: bar.buy_volume,
            tick_count: bar.tick_count,
            bar_number,
        }
    }
}

// =============================================================================
// IndexEntry — 16 bytes
// =============================================================================

/// Anchor index entry — 16 bytes `#[repr(C, packed)]`
///
/// Maps bar_number to byte offset for O(1) random access.
///
/// Byte layout:
/// ```text
/// 0-7:    bar_number (8B: u64, sequential bar index)
/// 8-15:   byte_offset (8B: u64, byte position of anchor in file)
/// ```
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct IndexEntry {
    pub bar_number: u64,   // 0-7: sequential bar index
    pub byte_offset: u64, // 8-15: byte position of anchor in file
}

const_assert!(std::mem::size_of::<IndexEntry>() == INDEX_ENTRY_SIZE);
const_assert!(std::mem::align_of::<IndexEntry>() == 1);

impl IndexEntry {
    /// Create a new index entry.
    pub fn new(bar_number: u64, byte_offset: u64) -> Self {
        Self {
            bar_number,
            byte_offset,
        }
    }
}

// =============================================================================
// Bar (in-memory representation)
// =============================================================================

/// In-memory bar representation used for reading/writing.
///
/// Price fields are stored as fixed-point integers (nanounits).
/// Volume fields are stored as integers (units).
#[derive(Debug, Clone, Copy)]
pub struct Bar {
    pub ts_event: u64,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub vwap: i64,
    pub volume: i64,
    pub buy_volume: i64,
    pub sell_volume: i64,
    pub tick_count: u32,
}

impl Bar {
    /// Create a new bar.
    pub fn new(
        ts_event: u64,
        open: i64,
        high: i64,
        low: i64,
        close: i64,
        vwap: i64,
        volume: i64,
        buy_volume: i64,
        sell_volume: i64,
        tick_count: u32,
    ) -> Self {
        Self {
            ts_event,
            open,
            high,
            low,
            close,
            vwap,
            volume,
            buy_volume,
            sell_volume,
            tick_count,
        }
    }

    /// Create from floating point values.
    pub fn from_floats(
        ts_event: u64,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        vwap: f64,
        volume: f64,
        buy_volume: f64,
        sell_volume: f64,
        tick_count: u32,
        precision: u8,
    ) -> Self {
        let mul = 10_f64.powi(precision as i32);
        Self {
            ts_event,
            open: (open * mul).round() as i64,
            high: (high * mul).round() as i64,
            low: (low * mul).round() as i64,
            close: (close * mul).round() as i64,
            vwap: (vwap * mul).round() as i64,
            volume: (volume * 1e6).round() as i64,
            buy_volume: (buy_volume * 1e6).round() as i64,
            sell_volume: (sell_volume * 1e6).round() as i64,
            tick_count,
        }
    }

    /// Convert to floating point with the given precision.
    pub fn to_floats(&self, precision: u8) -> (f64, f64, f64, f64, f64, f64, f64, f64, f64) {
        let divisor = 10_f64.powi(precision as i32);
        (
            self.open as f64 / divisor,
            self.high as f64 / divisor,
            self.low as f64 / divisor,
            self.close as f64 / divisor,
            self.vwap as f64 / divisor,
            self.volume as f64 / 1e6,
            self.buy_volume as f64 / 1e6,
            self.sell_volume as f64 / 1e6,
            self.tick_count as f64,
        )
    }
}

// =============================================================================
// DeltaBar (encoded representation for encoding/decoding)
// =============================================================================

/// Decoded delta bar result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecodedBar {
    pub ts_event: u64,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub vwap: i64,
    pub volume: i64,
    pub buy_volume: i64,
    pub sell_volume: i64,
    pub tick_count: u32,
}

// =============================================================================
// Error types
// =============================================================================

/// Errors that can occur during TVCB operations.
#[derive(Debug)]
pub enum TvcbError {
    InvalidMagic([u8; 4]),
    UnsupportedVersion(u8),
    InvalidDecimalPrecision(u8),
    InvalidAnchorInterval(u32),
    InvalidIndexOffset,
    Io(String),
    Sha256Mismatch,
    NoAnchors,
    BarNotFound,
    UnexpectedEndOfFile,
    InvalidDeltaEncoding,
}

impl std::fmt::Display for TvcbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TvcbError::InvalidMagic(m) => {
                write!(f, "Invalid TVCB magic: {:?}", m)
            }
            TvcbError::UnsupportedVersion(v) => {
                write!(f, "Unsupported TVCB version: {}", v)
            }
            TvcbError::InvalidDecimalPrecision(p) => {
                write!(f, "Invalid decimal precision: {}", p)
            }
            TvcbError::InvalidAnchorInterval(n) => {
                write!(f, "Invalid anchor interval: {}", n)
            }
            TvcbError::InvalidIndexOffset => {
                write!(f, "Invalid index offset in header")
            }
            TvcbError::Io(e) => write!(f, "IO error: {}", e),
            TvcbError::Sha256Mismatch => write!(f, "SHA256 digest mismatch"),
            TvcbError::NoAnchors => write!(f, "No anchors in file"),
            TvcbError::BarNotFound => write!(f, "Bar not found in index"),
            TvcbError::UnexpectedEndOfFile => write!(f, "Unexpected end of file"),
            TvcbError::InvalidDeltaEncoding => write!(f, "Invalid delta encoding"),
        }
    }
}

impl std::error::Error for TvcbError {}

impl From<std::io::Error> for TvcbError {
    fn from(e: std::io::Error) -> Self {
        TvcbError::Io(e.to_string())
    }
}

// =============================================================================
// Byte conversion helpers
// =============================================================================

/// Convert a TvcbHeader to bytes.
pub fn header_to_bytes(header: &TvcbHeader) -> [u8; HEADER_SIZE] {
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
    // buf[66..128] are already zero from the [0u8; 62] initialization

    buf
}

/// Parse a TvcbHeader from bytes.
pub fn bytes_to_header(buf: &[u8; HEADER_SIZE]) -> TvcbHeader {
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

/// Convert an AnchorBar to bytes.
pub fn anchor_bar_to_bytes(bar: &AnchorBar) -> [u8; ANCHOR_BAR_SIZE] {
    let mut buf = [0u8; ANCHOR_BAR_SIZE];

    buf[0..8].copy_from_slice(&bar.ts_event.to_le_bytes());
    buf[8..16].copy_from_slice(&bar.open.to_le_bytes());
    buf[16..24].copy_from_slice(&bar.high.to_le_bytes());
    buf[24..32].copy_from_slice(&bar.low.to_le_bytes());
    buf[32..40].copy_from_slice(&bar.close.to_le_bytes());
    buf[40..48].copy_from_slice(&bar.vwap.to_le_bytes());
    buf[48..56].copy_from_slice(&bar.volume.to_le_bytes());
    buf[56..64].copy_from_slice(&bar.buy_volume.to_le_bytes());
    buf[64..68].copy_from_slice(&bar.tick_count.to_le_bytes());
    buf[68..72].copy_from_slice(&bar.bar_number.to_le_bytes());

    buf
}

/// Parse an AnchorBar from bytes.
pub fn bytes_to_anchor_bar(buf: &[u8; ANCHOR_BAR_SIZE]) -> AnchorBar {
    let ts_event = u64::from_le_bytes([
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
    ]);
    let open = i64::from_le_bytes([
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
    ]);
    let high = i64::from_le_bytes([
        buf[16], buf[17], buf[18], buf[19], buf[20], buf[21], buf[22], buf[23],
    ]);
    let low = i64::from_le_bytes([
        buf[24], buf[25], buf[26], buf[27], buf[28], buf[29], buf[30], buf[31],
    ]);
    let close = i64::from_le_bytes([
        buf[32], buf[33], buf[34], buf[35], buf[36], buf[37], buf[38], buf[39],
    ]);
    let vwap = i64::from_le_bytes([
        buf[40], buf[41], buf[42], buf[43], buf[44], buf[45], buf[46], buf[47],
    ]);
    let volume = i64::from_le_bytes([
        buf[48], buf[49], buf[50], buf[51], buf[52], buf[53], buf[54], buf[55],
    ]);
    let buy_volume = i64::from_le_bytes([
        buf[56], buf[57], buf[58], buf[59], buf[60], buf[61], buf[62], buf[63],
    ]);
    let tick_count = u32::from_le_bytes([buf[64], buf[65], buf[66], buf[67]]);
    let bar_number = u32::from_le_bytes([buf[68], buf[69], buf[70], buf[71]]);

    AnchorBar {
        ts_event,
        open,
        high,
        low,
        close,
        vwap,
        volume,
        buy_volume,
        tick_count,
        bar_number,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_size() {
        assert_eq!(std::mem::size_of::<TvcbHeader>(), HEADER_SIZE);
        assert_eq!(std::mem::size_of::<TvcbHeader>(), 128);
    }

    #[test]
    fn test_anchor_bar_size() {
        assert_eq!(std::mem::size_of::<AnchorBar>(), ANCHOR_BAR_SIZE);
        assert_eq!(std::mem::size_of::<AnchorBar>(), 72);
    }

    #[test]
    fn test_index_entry_size() {
        assert_eq!(std::mem::size_of::<IndexEntry>(), INDEX_ENTRY_SIZE);
        assert_eq!(std::mem::size_of::<IndexEntry>(), 16);
    }

    #[test]
    fn test_header_magic() {
        let h = TvcbHeader::new(0, 1000, 9, 2024, 900_000_000_000);
        assert_eq!(h.magic, *b"TVCB");
        assert_eq!(h.version, 1);
    }

    #[test]
    fn test_header_validate_ok() {
        let h = TvcbHeader::new(0, 1000, 9, 2024, 900_000_000_000);
        assert!(h.validate().is_ok());
    }

    #[test]
    fn test_header_validate_bad_magic() {
        let mut h = TvcbHeader::new(0, 1000, 9, 2024, 900_000_000_000);
        h.magic = *b"XXXX";
        assert!(matches!(h.validate(), Err(TvcbError::InvalidMagic(_))));
    }

    #[test]
    fn test_header_validate_bad_version() {
        let mut h = TvcbHeader::new(0, 1000, 9, 2024, 900_000_000_000);
        h.version = 99;
        assert!(matches!(h.validate(), Err(TvcbError::UnsupportedVersion(_))));
    }

    #[test]
    fn test_header_roundtrip() {
        let header = TvcbHeader::new(0x12345678, 2920, 9, 2024, 900_000_000_000);
        let bytes = header_to_bytes(&header);
        let parsed = bytes_to_header(&bytes);
        // Copy fields to locals to avoid creating references to packed struct fields
        let (parsed_magic, parsed_version, parsed_precision, parsed_anchor, parsed_instrument,
             parsed_year, parsed_timeframe) = (
            parsed.magic, parsed.version, parsed.decimal_precision,
            parsed.anchor_interval, parsed.instrument_id, parsed.year, parsed.timeframe_ns
        );
        let (header_magic, header_version, header_precision, header_anchor, header_instrument,
             header_year, header_timeframe) = (
            header.magic, header.version, header.decimal_precision,
            header.anchor_interval, header.instrument_id, header.year, header.timeframe_ns
        );
        assert_eq!(parsed_magic, header_magic);
        assert_eq!(parsed_version, header_version);
        assert_eq!(parsed_precision, header_precision);
        assert_eq!(parsed_anchor, header_anchor);
        assert_eq!(parsed_instrument, header_instrument);
        assert_eq!(parsed_year, header_year);
        assert_eq!(parsed_timeframe, header_timeframe);
    }

    #[test]
    fn test_anchor_bar_to_bytes_roundtrip() {
        let bar = AnchorBar {
            ts_event: 1_000_000_000_000_000_000,
            open: 100_000_000_000,
            high: 101_000_000_000,
            low: 99_000_000_000,
            close: 100_500_000_000,
            vwap: 100_400_000_000,
            volume: 1_000_000,
            buy_volume: 600_000,
            tick_count: 42,
            bar_number: 10,
        };
        let bytes = anchor_bar_to_bytes(&bar);
        let parsed = bytes_to_anchor_bar(&bytes);
        // Copy fields to locals to avoid creating references to packed struct fields
        let (p_ts, p_open, p_high, p_low, p_close, p_vwap, p_vol, p_buy, p_tc, p_bn) = (
            parsed.ts_event, parsed.open, parsed.high, parsed.low, parsed.close,
            parsed.vwap, parsed.volume, parsed.buy_volume, parsed.tick_count, parsed.bar_number
        );
        let (b_ts, b_open, b_high, b_low, b_close, b_vwap, b_vol, b_buy, b_tc, b_bn) = (
            bar.ts_event, bar.open, bar.high, bar.low, bar.close,
            bar.vwap, bar.volume, bar.buy_volume, bar.tick_count, bar.bar_number
        );
        assert_eq!(p_ts, b_ts);
        assert_eq!(p_open, b_open);
        assert_eq!(p_high, b_high);
        assert_eq!(p_low, b_low);
        assert_eq!(p_close, b_close);
        assert_eq!(p_vwap, b_vwap);
        assert_eq!(p_vol, b_vol);
        assert_eq!(p_buy, b_buy);
        assert_eq!(p_tc, b_tc);
        assert_eq!(p_bn, b_bn);
    }

    #[test]
    fn test_bar_from_floats() {
        let bar = Bar::from_floats(
            1_000_000_000_000_000_000,
            100.0,
            101.0,
            99.0,
            100.5,
            100.4,
            1.0,
            0.6,
            0.4,
            42,
            9,
        );
        assert_eq!(bar.open, 100_000_000_000);
        assert_eq!(bar.high, 101_000_000_000);
        assert_eq!(bar.low, 99_000_000_000);
        assert_eq!(bar.close, 100_500_000_000);
        assert_eq!(bar.vwap, 100_400_000_000);
        assert_eq!(bar.volume, 1_000_000);
        assert_eq!(bar.buy_volume, 600_000);
        assert_eq!(bar.sell_volume, 400_000);
        assert_eq!(bar.tick_count, 42);
    }

    #[test]
    fn test_bar_to_floats() {
        let bar = Bar::new(
            1_000_000_000_000_000_000,
            100_000_000_000,
            101_000_000_000,
            99_000_000_000,
            100_500_000_000,
            100_400_000_000,
            1_000_000,
            600_000,
            400_000,
            42,
        );
        let (open, high, low, close, vwap, vol, buy, sell, tc) = bar.to_floats(9);
        assert!((open - 100.0).abs() < 1e-6);
        assert!((high - 101.0).abs() < 1e-6);
        assert!((low - 99.0).abs() < 1e-6);
        assert!((close - 100.5).abs() < 1e-6);
        assert!((vwap - 100.4).abs() < 1e-6);
        assert!((vol - 1.0).abs() < 1e-6);
        assert!((buy - 0.6).abs() < 1e-6);
        assert!((sell - 0.4).abs() < 1e-6);
        assert!((tc - 42.0).abs() < 1e-6);
    }
}