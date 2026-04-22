//! Data Engine messages — Subscribe/Unsubscribe commands.
//!
//! These messages flow through the MessageBus to DataEngine.

pub use crate::buffer::BarType;
use crate::instrument::InstrumentId;
use crate::messages::{StrategyId, TraderId};

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

/// Subscribe to quote ticks for an instrument.
#[derive(Debug, Clone)]
pub struct SubscribeQuotes {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub instrument_id: InstrumentId,
    pub endpoint: String,
}

/// Unsubscribe from quote ticks.
#[derive(Debug, Clone)]
pub struct UnsubscribeQuotes {
    pub endpoint: String,
}

/// Subscribe to order book updates for an instrument.
#[derive(Debug, Clone)]
pub struct SubscribeOrderBooks {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub instrument_id: InstrumentId,
    pub endpoint: String,
}

/// Unsubscribe from order book updates.
#[derive(Debug, Clone)]
pub struct UnsubscribeOrderBooks {
    pub endpoint: String,
}