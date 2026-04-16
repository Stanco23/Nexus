//! Data Engine messages — Subscribe/Unsubscribe commands.
//!
//! These messages flow through the MessageBus to DataEngine.

use crate::instrument::InstrumentId;
use crate::messages::{StrategyId, TraderId};

/// Bar type identifier — describes how a bar was aggregated.
///
/// Format: `{instrument_id}-{period}-{aggregation}`
/// Example: `BTCUSDT.BINANCE-1m-BETWEEN`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BarType(pub String);

impl BarType {
    pub fn new(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// Subscribe to trade ticks for an instrument.
#[derive(Debug, Clone)]
pub struct SubscribeTrades {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub instrument_id: InstrumentId,
    pub endpoint: String,
}

/// Unsubscribe from trade ticks.
#[derive(Debug, Clone)]
pub struct UnsubscribeTrades {
    pub endpoint: String,
}

/// Subscribe to bars for a bar type.
#[derive(Debug, Clone)]
pub struct SubscribeBars {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub bar_type: BarType,
    pub endpoint: String,
}

/// Unsubscribe from bars.
#[derive(Debug, Clone)]
pub struct UnsubscribeBars {
    pub endpoint: String,
}