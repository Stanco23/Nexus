//! Shared types for strategy definitions and the Nexus engine.
//!
//! These types are shared between `nexus` (engine) and `nexus-strategy` (strategy
//! trait). To avoid a circular dependency, both depend on this crate instead of each other.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

// ============================================================================
// Core types
// ============================================================================

/// Trading signal returned by strategy callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Signal {
    Buy,
    Sell,
    Close,
}

/// Position side for a given instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PositionSide {
    Long,
    Short,
    Flat,
}

/// Instrument identifier — symbol + exchange.
///
/// The `id` is a FNV-1a hash of the canonical "SYMBOL.EXCHANGE" string
/// (uppercase, "." separator), used for fast internal lookups in the engine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstrumentId {
    /// FNV-1a hash of "SYMBOL.EXCHANGE" (uppercase).
    pub id: u32,
    pub symbol: String,
    pub exchange: String,
}

impl InstrumentId {
    pub fn new(symbol: &str, exchange: &str) -> Self {
        Self {
            id: fnv1a_hash(symbol, exchange),
            symbol: symbol.to_uppercase(),
            exchange: exchange.to_uppercase(),
        }
    }
}

impl fmt::Display for InstrumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.symbol, self.exchange)
    }
}

impl FromStr for InstrumentId {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl InstrumentId {
    pub fn parse(s: &str) -> Result<Self, IdError> {
        if !s.contains('.') {
            return Err(IdError::InvalidFormat(s.to_string()));
        }
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            return Err(IdError::InvalidFormat(s.to_string()));
        }
        Ok(Self::new(parts[0], parts[1]))
    }

    pub fn as_str(&self) -> String {
        format!("{}.{}", self.symbol, self.exchange)
    }
}

#[derive(Debug, Clone)]
pub enum IdError {
    InvalidFormat(String),
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdError::InvalidFormat(s) => {
                write!(f, "Invalid instrument ID: {} (expected SYMBOL.EXCHANGE)", s)
            }
        }
    }
}

impl std::error::Error for IdError {}

/// FNV-1a hash of a symbol/exchange pair.
fn fnv1a_hash(symbol: &str, exchange: &str) -> u32 {
    let raw = format!("{}.{}", symbol.to_uppercase(), exchange.to_uppercase());
    let mut hash: u32 = 0x811c9dc5;
    for byte in raw.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

// ============================================================================
// Tick / Bar
// ============================================================================

/// Tick structure — lightweight trade event for tick-mode backtesting.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Tick {
    pub timestamp_ns: u64,
    pub price: f64,
    pub size: f64,
    pub vpin: f64,
}

impl Tick {
    pub fn new(timestamp_ns: u64, price: f64, size: f64, vpin: f64) -> Self {
        Self { timestamp_ns, price, size, vpin }
    }
}

/// Bar OHLCV structure for bar-mode backtesting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bar {
    pub timestamp_ns: u64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub buy_volume: f64,
    pub sell_volume: f64,
    pub tick_count: u64,
}

impl Bar {
    pub fn new(
        timestamp_ns: u64,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: f64,
    ) -> Self {
        Self {
            timestamp_ns,
            open,
            high,
            low,
            close,
            volume,
            buy_volume: 0.0,
            sell_volume: 0.0,
            tick_count: 0,
        }
    }
}

/// Backtest mode — controls whether the engine delivers ticks or bars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BacktestMode {
    Tick,
    Bar,
    Hybrid,
}

// ============================================================================
// Order / Execution
// ============================================================================

/// Order side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide {
    Buy,
    Sell,
}

/// Order type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    Market,
    Limit,
    Stop,
    StopLimit,
    TrailingStop,
}

/// Order type for OMS (Market, Limit, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OmsType {
    /// No OMS — raw signals only (no order management).
    None,
    /// Full OMS — the engine manages order lifecycle.
    Full,
    /// Net position — single position per instrument, net accounting.
    Net,
}

/// Order structure for pending order queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: u64,
    pub instrument_id: InstrumentId,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub price: f64,
    pub size: f64,
    pub sl: f64,
    pub tp: f64,
    pub filled: bool,
}

impl Order {
    pub fn new(
        id: u64,
        instrument_id: InstrumentId,
        side: OrderSide,
        order_type: OrderType,
        price: f64,
        size: f64,
    ) -> Self {
        Self {
            id,
            instrument_id,
            side,
            order_type,
            price,
            size,
            sl: 0.0,
            tp: 0.0,
            filled: false,
        }
    }

    pub fn with_sl(mut self, sl: f64) -> Self {
        self.sl = sl;
        self
    }

    pub fn with_tp(mut self, tp: f64) -> Self {
        self.tp = tp;
        self
    }
}

/// Opaque handle to an order — used by the engine/OMS layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderHandle(pub u64);

// ============================================================================
// Identifiers
// ============================================================================

/// Strategy identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategyId(pub String);

/// Position identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionId(pub String);

// ============================================================================
// Parameters
// ============================================================================

/// Parameter schema entry — describes one tunable strategy parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterSchema {
    pub name: String,
    pub param_type: ParameterType,
    pub default: ParameterValue,
    pub bounds: Option<(f64, f64)>, // (min, max) for f64 params
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParameterType {
    Float,
    Int,
    Bool,
    String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ParameterValue {
    Float(f64),
    Int(i64),
    Bool(bool),
}

impl ParameterValue {
    pub fn as_f64(&self) -> f64 {
        match self {
            ParameterValue::Float(v) => *v,
            ParameterValue::Int(v) => *v as f64,
            ParameterValue::Bool(v) => if *v { 1.0 } else { 0.0 },
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal() {
        assert_eq!(Signal::Buy, Signal::Buy);
        assert_eq!(Signal::Sell, Signal::Sell);
        assert_eq!(Signal::Close, Signal::Close);
    }

    #[test]
    fn test_position_side() {
        assert_eq!(PositionSide::Long, PositionSide::Long);
        assert_eq!(PositionSide::Short, PositionSide::Short);
        assert_eq!(PositionSide::Flat, PositionSide::Flat);
        assert_ne!(PositionSide::Long, PositionSide::Short);
    }

    #[test]
    fn test_instrument_id() {
        let id = InstrumentId::new("BTCUSDT", "BINANCE");
        assert_eq!(id.symbol, "BTCUSDT");
        assert_eq!(id.exchange, "BINANCE");
        assert_ne!(id.id, 0);
    }

    #[test]
    fn test_tick() {
        let t = Tick::new(1000, 100.0, 0.5, 0.3);
        assert_eq!(t.timestamp_ns, 1000);
        assert_eq!(t.price, 100.0);
        assert_eq!(t.size, 0.5);
        assert_eq!(t.vpin, 0.3);
    }

    #[test]
    fn test_bar() {
        let b = Bar::new(1000, 100.0, 105.0, 98.0, 103.0, 1000.0);
        assert_eq!(b.open, 100.0);
        assert_eq!(b.high, 105.0);
        assert_eq!(b.low, 98.0);
        assert_eq!(b.close, 103.0);
        assert_eq!(b.volume, 1000.0);
        assert_eq!(b.tick_count, 0);
    }

    #[test]
    fn test_order() {
        let id = InstrumentId::new("BTCUSDT", "BINANCE");
        let o = Order::new(1, id, OrderSide::Buy, OrderType::Limit, 100.0, 0.5)
            .with_sl(95.0)
            .with_tp(110.0);
        assert_eq!(o.id, 1);
        assert_eq!(o.sl, 95.0);
        assert_eq!(o.tp, 110.0);
        assert!(!o.filled);
    }

    #[test]
    fn test_parameter_value() {
        assert!((ParameterValue::Float(1.5).as_f64() - 1.5).abs() < 1e-9);
        assert!((ParameterValue::Int(42).as_f64() - 42.0).abs() < 1e-9);
        assert!((ParameterValue::Bool(true).as_f64() - 1.0).abs() < 1e-9);
    }
}
