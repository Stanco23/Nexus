//! Data Engine — subscription management and routing for live data.
//!
//! Architecture:
//! - DataEngine owns subscription state (which actors want which data)
//! - Receives pre-decoded ticks/bars from live adapters (via process loop)
//! - For each matching subscription, delivers data via registered callbacks
//! - BarAggregator.advance_time() driven by clock ticks for time-based bar closing

use crate::buffer::tick_buffer::TradeFlowStats;
use crate::data::messages::{BarType, SubscribeBars, SubscribeTrades, UnsubscribeBars, UnsubscribeTrades};
use crate::instrument::InstrumentId;
use std::collections::HashMap;
use std::sync::Arc;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum DataSubscription {
    Trades { instrument_id: InstrumentId },
    Bars { bar_type: BarType },
}

/// DataEngine — subscription management and routing for live data.
pub struct DataEngine {
    /// Subscriptions: actor endpoint → subscription params.
    subscriptions: HashMap<String, DataSubscription>,
    /// Callbacks: actor endpoint → tick receiver callback.
    /// Phase 5.5 replaces this with full MsgBus integration.
    tick_callbacks: HashMap<String, Arc<dyn Fn(TradeFlowStats) + Send + Sync>>,
}

impl DataEngine {
    pub fn new() -> Self {
        Self {
            subscriptions: HashMap::new(),
            tick_callbacks: HashMap::new(),
        }
    }

    /// Register a tick callback for an endpoint.
    /// Phase 5.5 replaces this with MsgBus::send integration.
    #[allow(dead_code)]
    pub fn register_tick_callback<F>(&mut self, endpoint: String, callback: F)
    where
        F: Fn(TradeFlowStats) + Send + Sync + 'static,
    {
        self.tick_callbacks.insert(endpoint, Arc::new(callback));
    }

    /// Process an incoming trade tick from an adapter.
    /// Looks up all subscriptions for the given instrument_id and routes to each endpoint.
    pub fn process_trade(&mut self, tick: &TradeFlowStats, instrument_id: InstrumentId) {
        for (endpoint, sub) in &self.subscriptions {
            match sub {
                DataSubscription::Trades {
                    instrument_id: sub_instrument_id,
                } => {
                    if *sub_instrument_id == instrument_id {
                        self.route_trade_to_endpoint(endpoint, tick.clone());
                    }
                }
                DataSubscription::Bars { .. } => {}
            }
        }
    }

    fn route_trade_to_endpoint(&self, endpoint: &str, tick: TradeFlowStats) {
        if let Some(cb) = self.tick_callbacks.get(endpoint) {
            cb(tick);
        }
    }

    /// Handle a subscribe trades message.
    pub fn subscribe_trades(&mut self, msg: SubscribeTrades) {
        self.subscriptions.insert(
            msg.endpoint,
            DataSubscription::Trades {
                instrument_id: msg.instrument_id,
            },
        );
    }

    /// Handle an unsubscribe trades message.
    pub fn unsubscribe_trades(&mut self, msg: UnsubscribeTrades) {
        self.subscriptions.remove(&msg.endpoint);
    }

    /// Handle a subscribe bars message.
    pub fn subscribe_bars(&mut self, msg: SubscribeBars) {
        self.subscriptions.insert(
            msg.endpoint,
            DataSubscription::Bars { bar_type: msg.bar_type },
        );
    }

    /// Handle an unsubscribe bars message.
    pub fn unsubscribe_bars(&mut self, msg: UnsubscribeBars) {
        self.subscriptions.remove(&msg.endpoint);
    }

    /// Return the number of active subscriptions.
    pub fn subscription_count(&self) -> usize {
        self.subscriptions.len()
    }
}

impl Default for DataEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::InstrumentId;
    use crate::messages::{StrategyId, TraderId};

    #[test]
    fn test_subscribe_and_unsubscribe_trades() {
        let mut engine = DataEngine::new();
        assert_eq!(engine.subscription_count(), 0);

        engine.subscribe_trades(SubscribeTrades {
            trader_id: TraderId::new("trader-001"),
            strategy_id: StrategyId::new("strategy-001"),
            instrument_id: InstrumentId::new("BTCUSDT", "BINANCE"),
            endpoint: "MyStrategy.on_trade_tick".to_string(),
        });
        assert_eq!(engine.subscription_count(), 1);

        engine.unsubscribe_trades(UnsubscribeTrades {
            endpoint: "MyStrategy.on_trade_tick".to_string(),
        });
        assert_eq!(engine.subscription_count(), 0);
    }

    #[test]
    fn test_subscribe_and_unsubscribe_bars() {
        let mut engine = DataEngine::new();

        engine.subscribe_bars(SubscribeBars {
            trader_id: TraderId::new("trader-001"),
            strategy_id: StrategyId::new("strategy-001"),
            bar_type: BarType::new("BTCUSDT.BINANCE-1m-agg"),
            endpoint: "MyStrategy.on_bar".to_string(),
        });
        assert_eq!(engine.subscription_count(), 1);

        engine.unsubscribe_bars(UnsubscribeBars {
            endpoint: "MyStrategy.on_bar".to_string(),
        });
        assert_eq!(engine.subscription_count(), 0);
    }

    #[test]
    fn test_multiple_subscriptions() {
        let mut engine = DataEngine::new();
        let btc = InstrumentId::new("BTCUSDT", "BINANCE");
        let eth = InstrumentId::new("ETHUSDT", "BINANCE");

        // Each actor endpoint subscribes once per instrument
        engine.subscribe_trades(SubscribeTrades {
            trader_id: TraderId::new("trader-001"),
            strategy_id: StrategyId::new("strategy-001"),
            instrument_id: btc.clone(),
            endpoint: "Strategy1.on_trade_tick_btc".to_string(),
        });

        engine.subscribe_trades(SubscribeTrades {
            trader_id: TraderId::new("trader-001"),
            strategy_id: StrategyId::new("strategy-001"),
            instrument_id: eth.clone(),
            endpoint: "Strategy1.on_trade_tick_eth".to_string(),
        });

        engine.subscribe_bars(SubscribeBars {
            trader_id: TraderId::new("trader-001"),
            strategy_id: StrategyId::new("strategy-001"),
            bar_type: BarType::new("BTCUSDT.BINANCE-1m-agg"),
            endpoint: "Strategy1.on_bar".to_string(),
        });

        assert_eq!(engine.subscription_count(), 3);
    }
}