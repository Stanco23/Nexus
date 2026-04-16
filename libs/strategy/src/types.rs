//! Shared types for strategy definitions.

use serde::{Deserialize, Serialize};

// Re-export from nexus for use across strategy implementations
pub use nexus::messages::{OmsType, PositionId, StrategyId};

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
        Self {
            timestamp_ns,
            price,
            size,
            vpin,
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

/// Position side for a given instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PositionSide {
    Long,
    Short,
    Flat,
}

/// Instrument identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstrumentId {
    pub symbol: String,
    pub exchange: String,
}

impl InstrumentId {
    pub fn new(symbol: &str, exchange: &str) -> Self {
        Self {
            symbol: symbol.to_string(),
            exchange: exchange.to_string(),
        }
    }
}

/// Trading signal returned by strategy callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Signal {
    Buy,
    Sell,
    Close,
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backtest_mode_variants() {
        assert_eq!(BacktestMode::Tick, BacktestMode::Tick);
        assert_eq!(BacktestMode::Bar, BacktestMode::Bar);
        assert_eq!(BacktestMode::Hybrid, BacktestMode::Hybrid);
        assert_ne!(BacktestMode::Tick, BacktestMode::Bar);
    }

    #[test]
    fn test_position_side() {
        assert_eq!(PositionSide::Long, PositionSide::Long);
        assert_eq!(PositionSide::Short, PositionSide::Short);
        assert_eq!(PositionSide::Flat, PositionSide::Flat);
        assert_ne!(PositionSide::Long, PositionSide::Short);
    }

    #[test]
    fn test_parameter_value_as_f64() {
        assert!((ParameterValue::Float(1.5).as_f64() - 1.5).abs() < 1e-9);
        assert!((ParameterValue::Int(42).as_f64() - 42.0).abs() < 1e-9);
        assert!((ParameterValue::Bool(true).as_f64() - 1.0).abs() < 1e-9);
        assert!((ParameterValue::Bool(false).as_f64() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn test_bar_new() {
        let bar = Bar::new(1000, 100.0, 105.0, 98.0, 103.0, 1000.0);
        assert_eq!(bar.open, 100.0);
        assert_eq!(bar.high, 105.0);
        assert_eq!(bar.low, 98.0);
        assert_eq!(bar.close, 103.0);
        assert_eq!(bar.volume, 1000.0);
        assert_eq!(bar.tick_count, 0);
    }

    #[test]
    fn test_tick_new() {
        let tick = Tick::new(1000, 100.0, 0.5, 0.3);
        assert_eq!(tick.timestamp_ns, 1000);
        assert_eq!(tick.price, 100.0);
        assert_eq!(tick.size, 0.5);
        assert_eq!(tick.vpin, 0.3);
    }

    #[test]
    fn test_instrument_id_new() {
        let id = InstrumentId::new("BTCUSDT", "BINANCE");
        assert_eq!(id.symbol, "BTCUSDT");
        assert_eq!(id.exchange, "BINANCE");
    }

    #[test]
    fn test_order_new() {
        let id = InstrumentId::new("BTCUSDT", "BINANCE");
        let order = Order::new(1, id.clone(), OrderSide::Buy, OrderType::Limit, 100.0, 0.5);
        assert_eq!(order.id, 1);
        assert_eq!(order.price, 100.0);
        assert_eq!(order.size, 0.5);
        assert_eq!(order.sl, 0.0);
        assert_eq!(order.tp, 0.0);
    }

    #[test]
    fn test_order_with_sl_tp() {
        let id = InstrumentId::new("BTCUSDT", "BINANCE");
        let order = Order::new(1, id, OrderSide::Sell, OrderType::Limit, 100.0, 0.5)
            .with_sl(95.0)
            .with_tp(110.0);
        assert_eq!(order.sl, 95.0);
        assert_eq!(order.tp, 110.0);
    }

    #[test]
    fn test_signal_variants() {
        assert_eq!(Signal::Buy, Signal::Buy);
        assert_eq!(Signal::Sell, Signal::Sell);
        assert_eq!(Signal::Close, Signal::Close);
    }
}
