//! Data Engine messages — Subscribe/Unsubscribe commands and Process messages.
//!
//! These messages flow through the MessageBus to DataEngine.

use std::sync::Arc;

pub use crate::buffer::BarType;
use crate::buffer::tick_buffer::TradeFlowStats;
use crate::cache::{Bar as CacheBar, OrderBook, QuoteTick};
use crate::instrument::InstrumentId;
use crate::messages::{StrategyId, TraderId};

// Re-export Message trait
pub use crate::actor::Message;

/// Subscribe to trade ticks for an instrument.
#[derive(Clone)]
pub struct SubscribeTrades {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub instrument_id: InstrumentId,
    pub endpoint: String,
    /// Callback invoked when a trade tick arrives for this subscription.
    pub callback: Option<Arc<dyn Fn(TradeFlowStats) + Send + Sync>>,
}

/// Process trade tick command — routes to DataEngine.process_trade().
#[derive(Debug, Clone)]
pub struct ProcessTrades {
    pub instrument_id: InstrumentId,
    pub trades: Vec<TradeData>,
}

#[derive(Debug, Clone)]
pub struct TradeData {
    pub price: f64,
    pub size: f64,
    pub aggressor_side: u8, // 0 = buy, 1 = sell
    pub ts_event: u64,
}

impl Message for ProcessTrades {}

/// Process quote tick command — routes to DataEngine.process_quote().
#[derive(Debug, Clone)]
pub struct ProcessQuotes {
    pub instrument_id: InstrumentId,
    pub quote: QuoteTick,
}

impl Message for ProcessQuotes {}

/// Process bar command — routes to DataEngine.process_bar().
#[derive(Debug, Clone)]
pub struct ProcessBars {
    pub bar_type: BarType,
    pub bar: CacheBar,
}

impl Message for ProcessBars {}

/// Process order book command — routes to DataEngine.process_orderbook().
#[derive(Debug, Clone)]
pub struct ProcessOrderBooks {
    pub instrument_id: InstrumentId,
    pub book: crate::cache::OrderBook,
}

impl Message for ProcessOrderBooks {}

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
#[derive(Clone)]
pub struct SubscribeQuotes {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub instrument_id: InstrumentId,
    pub endpoint: String,
    /// Callback invoked when a quote tick arrives for this subscription.
    pub callback: Option<Arc<dyn Fn(QuoteTick) + Send + Sync>>,
}

/// Unsubscribe from quote ticks.
#[derive(Debug, Clone)]
pub struct UnsubscribeQuotes {
    pub endpoint: String,
}

/// Subscribe to order book updates for an instrument.
#[derive(Clone)]
pub struct SubscribeOrderBooks {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub instrument_id: InstrumentId,
    pub endpoint: String,
    /// Callback invoked when an order book update arrives for this subscription.
    pub callback: Option<Arc<dyn Fn(OrderBook) + Send + Sync>>,
}

/// Unsubscribe from order book updates.
#[derive(Debug, Clone)]
pub struct UnsubscribeOrderBooks {
    pub endpoint: String,
}