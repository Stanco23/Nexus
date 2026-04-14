//! Binance WebSocket adapter.
//!
//! Normalizes Binance WebSocket trade streams to `NormalizedTick` format.
//! Handles timestamp precision differences between SPOT (ms/μs) and USDT-M (ms).
//!
//! # Binance WebSocket Streams
//! - SPOT: `wss://stream.binance.com:9443/ws/<symbol>@trade`
//! - USDT-M futures: `wss://fstream.binance.com/ws/<symbol>@trade`
//!
//! # Timestamp Precision
//! - Before 2025-01-01: milliseconds (ms)
//! - After 2025-01-01: microseconds (μs)
//!
//! # Message Format (Binance trade)
//! ```json
//! {
//!   "e": "trade",           // Event type
//!   "E": 1672515782136,    // Event time (ms)
//!   "s": "BTCUSDT",        // Symbol
//!   "t": 12345,            // Trade ID
//!   "p": "50000.00",       // Price
//!   "q": "0.01",           // Quantity
//!   "T": 1672515782135,    // Trade time (ms)
//!   "m": true,             // Is buyer maker
//!   "M": true              // Is match
//! }
//! ```

use serde::Deserialize;
use tvc::TradeTick;

/// Binance venue type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinanceVenue {
    /// Binance SPOT markets
    Spot,
    /// Binance USDT-M linear futures
    UsdtFutures,
}

/// A normalized tick from Binance.
#[derive(Debug, Clone, Copy)]
pub struct NormalizedTick {
    pub timestamp_ns: u64,
    pub price: f64,
    pub size: f64,
    pub side: u8, // 0=buy (aggressor is buy), 1=sell (aggressor is sell)
    pub trade_id: u64,
}

impl NormalizedTick {
    /// Convert to a TVC `TradeTick` with given decimal precision.
    pub fn to_trade_tick(self, precision: u8, sequence: u32) -> TradeTick {
        TradeTick::from_floats(
            self.timestamp_ns,
            self.price,
            self.size,
            self.side,
            precision,
            sequence,
        )
    }
}

/// Binance trade message from WebSocket.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BinanceTradeMsg {
    #[serde(rename = "e")]
    event_type: String,
    #[serde(rename = "E")]
    event_time_ms: u64,
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "t")]
    trade_id: u64,
    #[serde(rename = "p")]
    price_str: String,
    #[serde(rename = "q")]
    quantity_str: String,
    #[serde(rename = "T")]
    trade_time_ms: u64,
    #[serde(rename = "m")]
    is_buyer_maker: bool,
    #[serde(rename = "M")]
    _is_match: bool,
}

/// Binance adapter for WebSocket trade ingestion.
///
/// # Example
/// ```ignore
/// let adapter = BinanceAdapter::new("BTCUSDT", BinanceVenue::Spot);
/// let mut writer = TvcWriter::new(path, instrument_id, 1000, 9)?;
///
/// adapter.parse_and_process(msg_bytes, |tick| {
///     writer.write_tick(&tick)?;
///     Ok(())
/// })?;
/// ```
pub struct BinanceAdapter {
    symbol: String,
    #[allow(dead_code)]
    venue: BinanceVenue,
}

impl BinanceAdapter {
    pub fn new(symbol: &str, venue: BinanceVenue) -> Self {
        Self {
            symbol: symbol.to_uppercase(),
            venue,
        }
    }

    /// Parse a raw Binance WebSocket message and process the tick.
    ///
    /// Returns the number of ticks processed (0 or 1).
    pub fn parse_and_process<F>(
        &self,
        msg: &[u8],
        mut f: F,
    ) -> Result<usize, Box<dyn std::error::Error>>
    where
        F: FnMut(TradeTick) -> Result<(), std::io::Error>,
    {
        let msg_str = std::str::from_utf8(msg).map_err(|_| ParseError::InvalidUtf8)?;

        // Skip pings and other non-trade messages
        if !msg_str.contains("\"e\":\"trade\"") {
            return Ok(0);
        }

        let trade: BinanceTradeMsg = serde_json::from_str(msg_str).map_err(ParseError::Json)?;

        let normalized = self.normalize_trade(&trade);
        let tick = normalized.to_trade_tick(9, trade.trade_id as u32);
        f(tick)?;
        Ok(1)
    }

    /// Normalize a Binance trade message to our internal format.
    fn normalize_trade(&self, msg: &BinanceTradeMsg) -> NormalizedTick {
        // Timestamp: Binance uses milliseconds. Convert to nanoseconds.
        // After 2025-01-01, Binance switched to microseconds for some endpoints,
        // but the `@trade` stream still uses ms for both SPOT and futures.
        let ts_ns = msg.trade_time_ms * 1_000_000;

        let price: f64 = msg.price_str.parse().unwrap_or(0.0);
        let size: f64 = msg.quantity_str.parse().unwrap_or(0.0);

        // m=true means buyer is maker → seller aggressed → this is a SELL (side=1)
        // m=false means buyer is taker → buyer aggressed → this is a BUY (side=0)
        let side = if msg.is_buyer_maker { 1 } else { 0 };

        NormalizedTick {
            timestamp_ns: ts_ns,
            price,
            size,
            side,
            trade_id: msg.trade_id,
        }
    }

    /// Get the FNV-1a instrument ID for this symbol.
    pub fn instrument_id(&self) -> u32 {
        fnv1a_hash(self.symbol.as_bytes())
    }
}

/// Compute FNV-1a hash of a byte string (32-bit).
fn fnv1a_hash(data: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for byte in data {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

/// Parse error during WebSocket message processing.
#[derive(Debug)]
pub enum ParseError {
    InvalidUtf8,
    Json(serde_json::Error),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::InvalidUtf8 => write!(f, "Invalid UTF-8 in message"),
            ParseError::Json(e) => write!(f, "JSON parse error: {}", e),
        }
    }
}

impl std::error::Error for ParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_buy_trade() {
        let adapter = BinanceAdapter::new("BTCUSDT", BinanceVenue::Spot);
        let msg = BinanceTradeMsg {
            event_type: "trade".to_string(),
            event_time_ms: 1672515782136,
            symbol: "BTCUSDT".to_string(),
            trade_id: 12345,
            price_str: "50000.00".to_string(),
            quantity_str: "0.01".to_string(),
            trade_time_ms: 1672515782135,
            is_buyer_maker: false, // buyer is aggressor → BUY side
            _is_match: true,
        };

        let normalized = adapter.normalize_trade(&msg);
        assert_eq!(normalized.side, 0); // BUY
        assert_eq!(normalized.price, 50000.0);
        assert!(normalized.size > 0.0);
        assert_eq!(normalized.trade_id, 12345);
    }

    #[test]
    fn test_normalize_sell_trade() {
        let adapter = BinanceAdapter::new("BTCUSDT", BinanceVenue::Spot);
        let msg = BinanceTradeMsg {
            event_type: "trade".to_string(),
            event_time_ms: 1672515782136,
            symbol: "BTCUSDT".to_string(),
            trade_id: 12346,
            price_str: "50001.00".to_string(),
            quantity_str: "0.02".to_string(),
            trade_time_ms: 1672515782135,
            is_buyer_maker: true, // buyer is maker → seller aggressed → SELL side
            _is_match: true,
        };

        let normalized = adapter.normalize_trade(&msg);
        assert_eq!(normalized.side, 1); // SELL
    }

    #[test]
    fn test_parse_and_process_skips_non_trade() {
        let adapter = BinanceAdapter::new("BTCUSDT", BinanceVenue::Spot);
        let pong = b"{\"ping\":12345}";
        let result = adapter.parse_and_process(pong, |_tick| Ok(()));
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_instrument_id_is_stable() {
        let a1 = BinanceAdapter::new("BTCUSDT", BinanceVenue::Spot);
        let a2 = BinanceAdapter::new("btcusdt", BinanceVenue::Spot);
        // Same symbol, different case → should be same hash
        assert_eq!(a1.instrument_id(), a2.instrument_id());
    }

    #[test]
    fn test_to_trade_tick() {
        let tick = NormalizedTick {
            timestamp_ns: 1_000_000_000,
            price: 50000.0,
            size: 0.5,
            side: 0,
            trade_id: 42,
        };
        let trade = tick.to_trade_tick(2, 42);
        assert_eq!(trade.price_int, 5000000); // 50000 * 10^2
        assert_eq!(trade.timestamp_ns, 1_000_000_000);
    }

    #[test]
    fn test_fnv1a_consistency() {
        let id1 = fnv1a_hash(b"BTCUSDT");
        let id2 = fnv1a_hash(b"BTCUSDT");
        assert_eq!(id1, id2);
        // FNV should not be all zeros for a real string
        assert_ne!(id1, 0);
    }
}
