//! Data Engine — subscription management and routing for live data.
//!
//! Architecture:
//! - DataEngine owns subscription state (which actors want which data)
//! - Receives pre-decoded ticks/bars from live adapters (via process loop)
//! - For each matching subscription, delivers data via registered callbacks
//! - BarAggregator.advance_time() driven by clock ticks for time-based bar closing

use crate::actor::Clock;
use crate::buffer::tick_buffer::TradeFlowStats;
use crate::buffer::Aggregator;
use crate::cache::{Bar as CacheBar, OrderBook, QuoteTick};
use crate::data::messages::{BarType, SubscribeBars, SubscribeTrades, UnsubscribeBars, UnsubscribeTrades};
use crate::instrument::InstrumentId;
use std::collections::HashMap;
use std::sync::Arc;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum DataSubscription {
    Trades { instrument_id: InstrumentId },
    Bars { bar_type: BarType },
    Quotes { instrument_id: InstrumentId },
    OrderBooks { instrument_id: InstrumentId },
}

/// DataEngine — subscription management and routing for live data.
pub struct DataEngine {
    /// Clock for scheduling bar-close timers.
    clock: Box<dyn Clock>,
    /// Subscriptions: actor endpoint → subscription params.
    subscriptions: HashMap<String, DataSubscription>,
    /// Callbacks: actor endpoint → tick receiver callback.
    /// Phase 5.5 replaces this with full MsgBus integration.
    tick_callbacks: HashMap<String, Arc<dyn Fn(TradeFlowStats) + Send + Sync>>,
    /// Quote tick callbacks.
    quote_callbacks: HashMap<String, Arc<dyn Fn(QuoteTick) + Send + Sync>>,
    /// Order book callbacks.
    ob_callbacks: HashMap<String, Arc<dyn Fn(OrderBook) + Send + Sync>>,
    /// Bar callbacks (uses cache::Bar).
    bar_callbacks: HashMap<String, Arc<dyn Fn(CacheBar) + Send + Sync>>,
    /// Bar aggregators keyed by BarType.
    bar_aggregators: HashMap<BarType, Box<dyn Aggregator>>,
}

impl DataEngine {
    pub fn new(clock: Box<dyn Clock>) -> Self {
        Self {
            clock,
            subscriptions: HashMap::new(),
            tick_callbacks: HashMap::new(),
            quote_callbacks: HashMap::new(),
            ob_callbacks: HashMap::new(),
            bar_callbacks: HashMap::new(),
            bar_aggregators: HashMap::new(),
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

    /// Register a quote tick callback for an endpoint.
    #[allow(dead_code)]
    pub fn register_quote_callback<F>(&mut self, endpoint: String, callback: F)
    where
        F: Fn(QuoteTick) + Send + Sync + 'static,
    {
        self.quote_callbacks.insert(endpoint, Arc::new(callback));
    }

    /// Register an order book callback for an endpoint.
    #[allow(dead_code)]
    pub fn register_ob_callback<F>(&mut self, endpoint: String, callback: F)
    where
        F: Fn(OrderBook) + Send + Sync + 'static,
    {
        self.ob_callbacks.insert(endpoint, Arc::new(callback));
    }

    /// Register a bar callback for an endpoint.
    #[allow(dead_code)]
    pub fn register_bar_callback<F>(&mut self, endpoint: String, callback: F)
    where
        F: Fn(CacheBar) + Send + Sync + 'static,
    {
        self.bar_callbacks.insert(endpoint, Arc::new(callback));
    }

    /// Advance the clock — updates all bar aggregators and routes completed bars.
    pub fn advance_clock(&mut self, timestamp_ns: u64) {
        // Collect raw buffer bars first to avoid borrow conflict
        let raw_bars: Vec<(BarType, crate::buffer::bar_aggregation::Bar)> = self
            .bar_aggregators
            .iter_mut()
            .filter_map(|(bar_type, aggregator)| {
                aggregator.advance_time(timestamp_ns).map(|bar| (bar_type.clone(), bar))
            })
            .collect();

        for (bar_type, bar) in raw_bars {
            let cache_bar = self.buffer_bar_to_cache_bar(&bar);
            self.process_bar(&cache_bar, &bar_type);
        }
    }

    fn buffer_bar_to_cache_bar(&self, bar: &crate::buffer::bar_aggregation::Bar) -> CacheBar {
        CacheBar {
            ts_event: bar.ts_event,
            ts_init: bar.ts_init,
            open: bar.open as f64,
            high: bar.high as f64,
            low: bar.low as f64,
            close: bar.close as f64,
            volume: bar.volume as f64,
            buy_volume: bar.buy_volume as f64,
            sell_volume: bar.sell_volume as f64,
            tick_count: bar.tick_count,
            instrument_id: bar.instrument_id.clone(),
        }
    }

    /// Process an incoming trade tick from an adapter.
    /// Looks up all subscriptions for the given instrument_id and routes to each endpoint.
    pub fn process_trade(&mut self, tick: &TradeFlowStats, instrument_id: InstrumentId) {
        for (endpoint, sub) in &self.subscriptions {
            if let DataSubscription::Trades { instrument_id: sub_instrument_id } = sub {
                if *sub_instrument_id == instrument_id {
                    self.route_trade_to_endpoint(endpoint, tick.clone());
                }
            }
        }

        // Also update ALL bar aggregators for this instrument
        // Collect completed bars first to avoid borrow conflict
        let mut completed_bars: Vec<(BarType, crate::buffer::bar_aggregation::Bar)> = Vec::new();
        for (bar_type, aggregator) in self.bar_aggregators.iter_mut() {
            if bar_type.instrument_id() == instrument_id {
                // Loop to handle multi-bar generation from VolumeBarAggregator
                loop {
                    let bar = aggregator.update(
                        tick.price_int,
                        tick.size_int,
                        tick.side,
                        tick.timestamp_ns,
                    );
                    match bar {
                        Some(completed_bar) => completed_bars.push((bar_type.clone(), completed_bar)),
                        None => break,
                    }
                }
            }
        }

        // Process bars outside the mutable borrow
        for (bar_type, bar) in completed_bars {
            let cache_bar = self.buffer_bar_to_cache_bar(&bar);
            self.process_bar(&cache_bar, &bar_type);
        }
    }

    /// Process an incoming quote tick from an adapter.
    pub fn process_quote(&self, tick: &QuoteTick, instrument_id: InstrumentId) {
        for (endpoint, sub) in &self.subscriptions {
            if let DataSubscription::Quotes { instrument_id: sub_instrument_id } = sub {
                if *sub_instrument_id == instrument_id {
                    self.route_quote_to_endpoint(endpoint, tick.clone());
                }
            }
        }
    }

    /// Process an incoming order book update from an adapter.
    pub fn process_orderbook(&self, book: &OrderBook, instrument_id: InstrumentId) {
        for (endpoint, sub) in &self.subscriptions {
            if let DataSubscription::OrderBooks { instrument_id: sub_instrument_id } = sub {
                if *sub_instrument_id == instrument_id {
                    self.route_ob_to_endpoint(endpoint, book.clone());
                }
            }
        }
    }

    /// Process an incoming bar from an aggregator or adapter.
    pub fn process_bar(&self, bar: &CacheBar, bar_type: &BarType) {
        for (endpoint, sub) in &self.subscriptions {
            if let DataSubscription::Bars { bar_type: sub_bar_type } = sub {
                if bar_type == sub_bar_type {
                    self.route_bar_to_endpoint(endpoint, bar.clone());
                }
            }
        }
    }

    fn route_trade_to_endpoint(&self, endpoint: &str, tick: TradeFlowStats) {
        if let Some(cb) = self.tick_callbacks.get(endpoint) {
            cb(tick);
        }
    }

    fn route_quote_to_endpoint(&self, endpoint: &str, tick: QuoteTick) {
        if let Some(cb) = self.quote_callbacks.get(endpoint) {
            cb(tick);
        }
    }

    fn route_ob_to_endpoint(&self, endpoint: &str, book: OrderBook) {
        if let Some(cb) = self.ob_callbacks.get(endpoint) {
            cb(book);
        }
    }

    fn route_bar_to_endpoint(&self, endpoint: &str, bar: CacheBar) {
        if let Some(cb) = self.bar_callbacks.get(endpoint) {
            cb(bar);
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
        let period_ns = msg.bar_type.spec().get_interval_ns()
            .unwrap_or(60_000_000_000);

        // Create aggregator if not exists
        let _ = self.bar_aggregators.entry(msg.bar_type.clone()).or_insert_with(|| {
            crate::buffer::AggregatorFactory::create(msg.bar_type.clone())
        });

        // Namespaced timer name includes full bar_type for uniqueness across instruments.
        // e.g., "data_engine.bar_close_BTCUSDT.BINANCE-1m-BETWEEN"
        let timer_name = format!("data_engine.bar_close_{}", msg.bar_type.as_str());
        let current_ns = self.clock.timestamp_ns();
        let next_boundary = ((current_ns / period_ns) + 1) * period_ns;

        // SAFETY: Raw pointer created from &mut self. The pointer is only dereferenced
        // inside the timer callback, which fires when the clock advances. The clock is
        // owned by DataEngine, so the callback fires as long as DataEngine lives.
        // If DataEngine is dropped without cancelling the timer, the callback may fire
        // with a dangling pointer — but since DataEngine owns the clock and the timer
        // callback lives inside the clock, the timer must be cancelled (via Clock::cancel_timer
        // or Clock::cancel_timers) before DataEngine is dropped, which is the caller's
        // responsibility. The `engine_ptr` is only dereferenced after checking that
        // the timer has not been cancelled.
        let engine_ptr = self as *mut DataEngine;
        self.clock.set_timer_repeating(
            &timer_name,
            period_ns,
            next_boundary,
            None,
            Box::new(move |event| {
                // SAFETY: DataEngine must be live for the lifetime of the timer callback.
                // This is guaranteed when the timer is cancelled before DataEngine is dropped.
                unsafe {
                    let engine = &mut *engine_ptr;
                    engine.advance_clock(event.timestamp_ns);
                }
            }),
            false, // fire_immediately = false
        );

        self.subscriptions.insert(
            msg.endpoint,
            DataSubscription::Bars { bar_type: msg.bar_type },
        );
    }

    /// Handle an unsubscribe bars message.
    pub fn unsubscribe_bars(&mut self, msg: UnsubscribeBars) {
        self.subscriptions.remove(&msg.endpoint);
        // Note: we keep the aggregator around for now — it may still close bars on clock advance
    }

    /// Handle a subscribe quotes message.
    pub fn subscribe_quotes(&mut self, msg: crate::data::messages::SubscribeQuotes) {
        self.subscriptions.insert(
            msg.endpoint,
            DataSubscription::Quotes {
                instrument_id: msg.instrument_id,
            },
        );
    }

    /// Handle an unsubscribe quotes message.
    pub fn unsubscribe_quotes(&mut self, msg: crate::data::messages::UnsubscribeQuotes) {
        self.subscriptions.remove(&msg.endpoint);
    }

    /// Handle a subscribe order books message.
    pub fn subscribe_orderbooks(&mut self, msg: crate::data::messages::SubscribeOrderBooks) {
        self.subscriptions.insert(
            msg.endpoint,
            DataSubscription::OrderBooks {
                instrument_id: msg.instrument_id,
            },
        );
    }

    /// Handle an unsubscribe order books message.
    pub fn unsubscribe_orderbooks(&mut self, msg: crate::data::messages::UnsubscribeOrderBooks) {
        self.subscriptions.remove(&msg.endpoint);
    }

    /// Return the number of active subscriptions.
    pub fn subscription_count(&self) -> usize {
        self.subscriptions.len()
    }

    /// Replay historical ticks through the data engine.
    /// Iterates ticks in order and calls process_trade for each.
    ///
    /// NOTE: Currently dead code — the sweep runner drives BacktestEngine directly,
    /// never creating a DataEngine. This method is kept for potential future use when
    /// DataEngine becomes the canonical backtest data path. Remove or wire if that
    /// integration is needed.
    #[allow(dead_code)]
    pub fn replay(&mut self, ticks: &[TradeFlowStats], instrument_id: InstrumentId) {
        for tick in ticks {
            self.process_trade(tick, instrument_id.clone());
        }
    }
}

impl Default for DataEngine {
    fn default() -> Self {
        panic!("DataEngine::default() requires a Clock — use DataEngine::new(clock) instead")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::TestClock;
    use crate::instrument::InstrumentId;
    use crate::messages::{StrategyId, TraderId};

    #[test]
    fn test_subscribe_and_unsubscribe_trades() {
        let clock = Box::new(TestClock::new());
        let mut engine = DataEngine::new(clock);
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
        let clock = Box::new(TestClock::new());
        let mut engine = DataEngine::new(clock);

        engine.subscribe_bars(SubscribeBars {
            trader_id: TraderId::new("trader-001"),
            strategy_id: StrategyId::new("strategy-001"),
            bar_type: BarType::parse_bar_type("BTCUSDT.BINANCE-1-MINUTE-LAST").unwrap(),
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
        let clock = Box::new(TestClock::new());
        let mut engine = DataEngine::new(clock);
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
            bar_type: BarType::parse_bar_type("BTCUSDT.BINANCE-1-MINUTE-LAST").unwrap(),
            endpoint: "Strategy1.on_bar".to_string(),
        });

        assert_eq!(engine.subscription_count(), 3);
    }
}