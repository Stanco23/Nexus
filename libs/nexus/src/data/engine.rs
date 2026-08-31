//! Data Engine — subscription management and routing for live data.
//!
//! Architecture:
//! - DataEngine owns subscription state (which actors want which data)
//! - Receives pre-decoded ticks from live adapters (via process loop)
//! - For each matching subscription, delivers data via registered callbacks
//!
//! Bar handling: removed. Bars come from TVCB via DataManager; DataEngine only
//! routes ticks, quotes, and order books. SubscribeBars/UnsubscribeBars still
//! exist for API stability but are no-ops.
//!
//! MessageBus Integration (Phase 5.5):
//! - Registers endpoints: DataEngine.execute, DataEngine.process, DataEngine.request, DataEngine.response
//! - Subscribes to data topics: data.trade.*, data.quote.*, data.bar.*

use crate::actor::{Clock, ComponentState, FiniteStateMachine, Logger, MessageBus};
use crate::buffer::tick_buffer::TradeFlowStats;
use crate::cache::{Bar as CacheBar, OrderBook, QuoteTick};
use crate::data::messages::{
    BarType, ProcessBars, ProcessOrderBooks, ProcessQuotes, ProcessTrades, SubscribeBars,
    SubscribeTrades, UnsubscribeBars, UnsubscribeTrades,
};
use crate::instrument::InstrumentId;
use crate::messages::TraderId;
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Message handler callback type for DataEngine subscriptions.
pub type Handler = Box<dyn Fn(&dyn Any) + Send + Sync>;

// Re-export Component transitions for DataEngine FSM
const DATA_ENGINE_TRANSITIONS: &[(ComponentState, crate::actor::ComponentTrigger, ComponentState)] = &[
    (ComponentState::PreInitialized, crate::actor::ComponentTrigger::Initialize, ComponentState::Initialized),
    (ComponentState::Initialized, crate::actor::ComponentTrigger::Start, ComponentState::Starting),
    (ComponentState::Starting, crate::actor::ComponentTrigger::StartCompleted, ComponentState::Running),
    (ComponentState::Running, crate::actor::ComponentTrigger::Stop, ComponentState::Stopping),
    (ComponentState::Stopping, crate::actor::ComponentTrigger::StopCompleted, ComponentState::Stopped),
    (ComponentState::Ready, crate::actor::ComponentTrigger::Reset, ComponentState::Resetting),
    (ComponentState::Resetting, crate::actor::ComponentTrigger::ResetCompleted, ComponentState::Ready),
    (ComponentState::Ready, crate::actor::ComponentTrigger::Dispose, ComponentState::Disposing),
    (ComponentState::Disposing, crate::actor::ComponentTrigger::DisposeCompleted, ComponentState::Disposed),
    (ComponentState::Running, crate::actor::ComponentTrigger::Fault, ComponentState::Faulting),
    (ComponentState::Faulting, crate::actor::ComponentTrigger::FaultCompleted, ComponentState::Faulted),
    (ComponentState::Running, crate::actor::ComponentTrigger::Degrade, ComponentState::Degrading),
    (ComponentState::Degrading, crate::actor::ComponentTrigger::DegradeCompleted, ComponentState::Degraded),
    (ComponentState::Degraded, crate::actor::ComponentTrigger::Resume, ComponentState::Resuming),
    (ComponentState::Resuming, crate::actor::ComponentTrigger::ResumeCompleted, ComponentState::Ready),
];

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum DataSubscription {
    Trades { instrument_id: InstrumentId },
    Bars { bar_type: BarType },
    Quotes { instrument_id: InstrumentId },
    OrderBooks { instrument_id: InstrumentId },
}

/// DataEngine — subscription management and routing for live data.
///
/// Provides Component-like interface for MessageBus registration.
/// Registered endpoints:
/// - `DataEngine.execute` — handles Execute command
/// - `DataEngine.process` — handles Process command
/// - `DataEngine.request` — handles Request command
/// - `DataEngine.response` — handles Response command
///
/// Subscribed data topics:
/// - `data.trade.*` — trade tick data
/// - `data.quote.*` — quote tick data
/// - `data.bar.*` — bar data
pub struct DataEngine {
    /// Unique component identifier.
    pub id: u64,
    /// Component name.
    pub name: &'static str,
    /// Trader identifier.
    pub trader_id: TraderId,
    /// Clock for scheduling bar-close timers.
    pub clock: Box<dyn Clock>,
    /// Message bus for pub/sub and endpoint communication.
    pub msgbus: Arc<MessageBus>,
    /// Logger for component events.
    logger: Logger,
    /// Finite state machine for lifecycle management.
    fsm: FiniteStateMachine<ComponentState, crate::actor::ComponentTrigger>,
    /// Subscriptions: actor endpoint → subscription params.
    subscriptions: HashMap<String, DataSubscription>,
    /// Callbacks: actor endpoint → tick receiver callback.
    tick_callbacks: HashMap<String, Arc<dyn Fn(TradeFlowStats) + Send + Sync>>,
    /// Quote tick callbacks.
    quote_callbacks: HashMap<String, Arc<dyn Fn(QuoteTick) + Send + Sync>>,
    /// Order book callbacks.
    ob_callbacks: HashMap<String, Arc<dyn Fn(OrderBook) + Send + Sync>>,
    /// Bar callbacks (uses cache::Bar).
    /// Note: subscription still works, but bar_aggregators field is removed;
    /// SubscribeBars registers the subscription without creating an aggregator.
    bar_callbacks: HashMap<String, Arc<dyn Fn(CacheBar) + Send + Sync>>,
    /// Self-reference for use in async message handlers.
    /// Set once during construction before initialize() is called.
    /// Allows closures registered on the message bus to safely access DataEngine
    /// without raw pointer gymnastics.
    self_ref: Option<Arc<Mutex<DataEngine>>>,
}

impl DataEngine {
    /// Create a new DataEngine with minimal configuration.
    /// Used for backtesting where MessageBus integration is not needed.
    pub fn new(clock: Box<dyn Clock>) -> Self {
        // Create the Self first with placeholder self_ref
        let mut this = Self {
            id: 0,
            name: "DataEngine",
            trader_id: TraderId::new("BACKTEST"),
            clock,
            msgbus: Arc::new(MessageBus::new()),
            logger: Logger::new("DataEngine"),
            fsm: FiniteStateMachine::new(ComponentState::PreInitialized, DATA_ENGINE_TRANSITIONS),
            subscriptions: HashMap::new(),
            tick_callbacks: HashMap::new(),
            quote_callbacks: HashMap::new(),
            ob_callbacks: HashMap::new(),
            bar_callbacks: HashMap::new(),
            self_ref: None,
        };
        // Set up self-reference for message bus handlers.
        // We create the Arc<Mutex<Self>> from the raw pointer, then store it.
        let this_ptr = &mut this as *mut DataEngine;
        let mutex_ptr = this_ptr as *mut Mutex<DataEngine>;
        let self_arc = unsafe { Arc::from_raw(mutex_ptr) };
        this.self_ref = Some(self_arc);
        this
    }

    /// Create a new DataEngine with full component configuration.
    /// Used for live trading where MessageBus registration is required.
    pub fn new_with_components(
        trader_id: TraderId,
        msgbus: Arc<MessageBus>,
        clock: Box<dyn Clock>,
    ) -> Self {
        let mut this = Self {
            id: 0, // Set by trader when adding component
            name: "DataEngine",
            trader_id,
            clock,
            msgbus,
            logger: Logger::new("DataEngine"),
            fsm: FiniteStateMachine::new(ComponentState::PreInitialized, DATA_ENGINE_TRANSITIONS),
            subscriptions: HashMap::new(),
            tick_callbacks: HashMap::new(),
            quote_callbacks: HashMap::new(),
            ob_callbacks: HashMap::new(),
            bar_callbacks: HashMap::new(),
            self_ref: None,
        };
        // Set up self-reference for message bus handlers.
        let this_ptr = &mut this as *mut DataEngine;
        let mutex_ptr = this_ptr as *mut Mutex<DataEngine>;
        let self_arc = unsafe { Arc::from_raw(mutex_ptr) };
        this.self_ref = Some(self_arc);
        this
    }

    /// Return the current component state.
    pub fn state(&self) -> ComponentState {
        self.fsm.current()
    }

    /// Initialize the DataEngine component.
    ///
    /// Registers the following endpoints on the MessageBus:
    /// - `DataEngine.execute` — handles Execute command
    /// - `DataEngine.process` — handles Process command
    /// - `DataEngine.request` — handles Request command
    /// - `DataEngine.response` — handles Response command
    ///
    /// Subscribes to the following data topics:
    /// - `data.trade.*` — trade tick data
    /// - `data.quote.*` — quote tick data
    /// - `data.bar.*` — bar data
    pub fn initialize(&mut self) {
        if self.fsm.current() != ComponentState::PreInitialized {
            return;
        }

        let self_ref = self.self_ref.clone();

        // Register endpoints on the message bus

        // DataEngine.execute endpoint - handles Process* commands
        let self_ref_execute = self_ref.clone();
        self.msgbus.register("DataEngine.execute", Box::new(move |msg| {
            // Handle execute command - route Process* messages
            if let Some(process) = msg.downcast_ref::<ProcessTrades>() {
                // Convert TradeData to TradeFlowStats and route
                for trade in &process.trades {
                    let tick = TradeFlowStats {
                        timestamp_ns: trade.ts_event,
                        price_int: (trade.price * 10000.0) as i64,
                        size_int: (trade.size * 10000.0) as i64,
                        side: trade.aggressor_side,
                        cum_buy_volume: 0,
                        cum_sell_volume: 0,
                        vpin: 0.0,
                        bucket_index: 0,
                        instrument_id: Some(process.instrument_id.clone()),
                    };
                    if let Some(ref r) = self_ref_execute {
                        let mut engine = r.lock().unwrap();
                        engine.process_trade(&tick, process.instrument_id.clone());
                    }
                }
            } else if let Some(process) = msg.downcast_ref::<ProcessQuotes>() {
                if let Some(ref r) = self_ref_execute {
                    let engine = r.lock().unwrap();
                    engine.process_quote(&process.quote, process.instrument_id.clone());
                }
            } else if let Some(process) = msg.downcast_ref::<ProcessBars>() {
                if let Some(ref r) = self_ref_execute {
                    let engine = r.lock().unwrap();
                    engine.process_bar(&process.bar, &process.bar_type);
                }
            } else if let Some(process) = msg.downcast_ref::<ProcessOrderBooks>() {
                if let Some(ref r) = self_ref_execute {
                    let engine = r.lock().unwrap();
                    engine.process_orderbook(&process.book, process.instrument_id.clone());
                }
            }
        }));

        // DataEngine.process endpoint - handles Process* commands (alias)
        let self_ref_process = self_ref.clone();
        self.msgbus.register("DataEngine.process", Box::new(move |msg| {
            if let Some(process) = msg.downcast_ref::<ProcessTrades>() {
                for trade in &process.trades {
                    let tick = TradeFlowStats {
                        timestamp_ns: trade.ts_event,
                        price_int: (trade.price * 10000.0) as i64,
                        size_int: (trade.size * 10000.0) as i64,
                        side: trade.aggressor_side,
                        cum_buy_volume: 0,
                        cum_sell_volume: 0,
                        vpin: 0.0,
                        bucket_index: 0,
                        instrument_id: Some(process.instrument_id.clone()),
                    };
                    if let Some(ref r) = self_ref_process {
                        let mut engine = r.lock().unwrap();
                        engine.process_trade(&tick, process.instrument_id.clone());
                    }
                }
            } else if let Some(process) = msg.downcast_ref::<ProcessQuotes>() {
                if let Some(ref r) = self_ref_process {
                    let engine = r.lock().unwrap();
                    engine.process_quote(&process.quote, process.instrument_id.clone());
                }
            } else if let Some(process) = msg.downcast_ref::<ProcessBars>() {
                if let Some(ref r) = self_ref_process {
                    let engine = r.lock().unwrap();
                    engine.process_bar(&process.bar, &process.bar_type);
                }
            } else if let Some(process) = msg.downcast_ref::<ProcessOrderBooks>() {
                if let Some(ref r) = self_ref_process {
                    let engine = r.lock().unwrap();
                    engine.process_orderbook(&process.book, process.instrument_id.clone());
                }
            }
        }));

        // DataEngine.request endpoint
        self.msgbus.register("DataEngine.request", Box::new(move |_msg| {
            // Handle request command - return data snapshots, historical bars, etc.
        }));

        // DataEngine.response endpoint
        self.msgbus.register("DataEngine.response", Box::new(move |_msg| {
            // Handle response command - return query responses
        }));

        // Subscribe to data topics
        // SAFETY: DataEngine must be live for the lifetime of the subscription handlers.
        // This is guaranteed when the subscription is cancelled before DataEngine is dropped.
        let self_ref_trade = self_ref.clone();
        let trade_handler = Box::new(move |msg: &dyn Any| {
            if let Some(tick) = msg.downcast_ref::<TradeFlowStats>() {
                // Route to process_trade - instrument_id may be None if not set on this tick
                // For subscription routing, callers must ensure instrument_id is set
                if let Some(instrument_id) = &tick.instrument_id {
                    if let Some(ref r) = self_ref_trade {
                        let mut engine = r.lock().unwrap();
                        engine.process_trade(tick, instrument_id.clone());
                    }
                }
            }
        });

        let self_ref_quote = self_ref.clone();
        let quote_handler = Box::new(move |msg: &dyn Any| {
            if let Some(tick) = msg.downcast_ref::<QuoteTick>() {
                // Route to process_quote - QuoteTick has instrument_id directly
                if let Some(ref r) = self_ref_quote {
                    let engine = r.lock().unwrap();
                    engine.process_quote(tick, tick.instrument_id.clone());
                }
            }
        });

        let self_ref_bar = self_ref.clone();
        let bar_handler = Box::new(move |msg: &dyn Any| {
            if let Some(process) = msg.downcast_ref::<ProcessBars>() {
                // Route to process_bar
                if let Some(ref r) = self_ref_bar {
                    let engine = r.lock().unwrap();
                    engine.process_bar(&process.bar, &process.bar_type);
                }
            }
        });

        let self_ref_ob = self_ref.clone();
        let ob_handler = Box::new(move |msg: &dyn Any| {
            if let Some(book) = msg.downcast_ref::<crate::cache::OrderBook>() {
                // Route to process_orderbook - OrderBook has instrument_id directly
                if let Some(ref r) = self_ref_ob {
                    let engine = r.lock().unwrap();
                    engine.process_orderbook(book, book.instrument_id.clone());
                }
            }
        });

        self.msgbus.subscribe("data.trade.*", self.id, trade_handler, 0);
        self.msgbus.subscribe("data.quote.*", self.id, quote_handler, 0);
        self.msgbus.subscribe("data.bar.*", self.id, bar_handler, 0);
        self.msgbus.subscribe("data.ob.*", self.id, ob_handler, 0);

        self.fsm.trigger(crate::actor::ComponentTrigger::Initialize);
        self.logger.info("DataEngine initialized");
    }

    /// Start the DataEngine component.
    pub fn start(&mut self) {
        if self.fsm.current() != ComponentState::Initialized {
            return;
        }
        self.fsm.trigger(crate::actor::ComponentTrigger::Start);
        self.fsm.trigger(crate::actor::ComponentTrigger::StartCompleted);
        self.logger.info("DataEngine started");
    }

    /// Stop the DataEngine component.
    pub fn stop(&mut self) {
        if self.fsm.current() != ComponentState::Running {
            return;
        }
        self.fsm.trigger(crate::actor::ComponentTrigger::Stop);
        self.fsm.trigger(crate::actor::ComponentTrigger::StopCompleted);
        self.logger.info("DataEngine stopped");
    }

    /// Subscribe to a topic on the message bus.
    pub fn subscribe(
        &self,
        topic: &str,
        handler: Handler,
        priority: i32,
    ) {
        self.msgbus.subscribe(topic, self.id, handler, priority);
    }

    /// Register a tick callback for an endpoint.
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

    /// Advance the clock — no-op. Bar aggregator machinery was removed; TVCB provides
    /// pre-aggregated bars. Kept as a stub for API stability.
    #[allow(dead_code)]
    pub fn advance_clock(&mut self, _timestamp_ns: u64) {
        // Bar aggregation removed.
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
        // Bar aggregator updates removed — TVCB provides pre-aggregated bars.
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
            msg.endpoint.clone(),
            DataSubscription::Trades {
                instrument_id: msg.instrument_id,
            },
        );
        // Register the callback so route_trade_to_endpoint() can invoke it
        if let Some(cb) = msg.callback {
            self.tick_callbacks.insert(msg.endpoint, cb);
        }
    }

    /// Handle an unsubscribe trades message.
    pub fn unsubscribe_trades(&mut self, msg: UnsubscribeTrades) {
        self.subscriptions.remove(&msg.endpoint);
    }

    /// Handle a subscribe bars message.
    /// Stub: bar aggregator removed. TVCB provides pre-aggregated bars.
    /// SubscribeBars now only registers the subscription; no aggregator + no timer.
    #[allow(dead_code)]
    pub fn subscribe_bars(&mut self, msg: SubscribeBars) {
        // Bar aggregation removed — bars come from TVCB via DataManager.
        // Timer + aggregator registration is no longer needed.
        let _ = msg; // suppress unused warning until we wire TVCB delivery
    }

    /// Handle an unsubscribe bars message.
    pub fn unsubscribe_bars(&mut self, msg: UnsubscribeBars) {
        self.subscriptions.remove(&msg.endpoint);
        // Note: we keep the aggregator around for now — it may still close bars on clock advance
    }

    /// Handle a subscribe quotes message.
    pub fn subscribe_quotes(&mut self, msg: crate::data::messages::SubscribeQuotes) {
        self.subscriptions.insert(
            msg.endpoint.clone(),
            DataSubscription::Quotes {
                instrument_id: msg.instrument_id,
            },
        );
        // Register the callback so route_quote_to_endpoint() can invoke it
        if let Some(cb) = msg.callback {
            self.quote_callbacks.insert(msg.endpoint, cb);
        }
    }

    /// Handle an unsubscribe quotes message.
    pub fn unsubscribe_quotes(&mut self, msg: crate::data::messages::UnsubscribeQuotes) {
        self.subscriptions.remove(&msg.endpoint);
    }

    /// Handle a subscribe order books message.
    pub fn subscribe_orderbooks(&mut self, msg: crate::data::messages::SubscribeOrderBooks) {
        self.subscriptions.insert(
            msg.endpoint.clone(),
            DataSubscription::OrderBooks {
                instrument_id: msg.instrument_id,
            },
        );
        // Register the callback so route_ob_to_endpoint() can invoke it
        if let Some(cb) = msg.callback {
            self.ob_callbacks.insert(msg.endpoint, cb);
        }
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

// SAFETY: DataEngine is designed to be wrapped in Arc<Mutex<DataEngine>> for async
// access. All internal mutation goes through &mut self, which is protected by the
// Mutex. The MessageBus inside is also only accessed through Mutex guards.
// This is the same pattern used by BinanceMarketDataAdapter.
unsafe impl Send for DataEngine {}
unsafe impl Sync for DataEngine {}

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
            callback: None,
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
            callback: None,
        });

        engine.subscribe_trades(SubscribeTrades {
            trader_id: TraderId::new("trader-001"),
            strategy_id: StrategyId::new("strategy-001"),
            instrument_id: eth.clone(),
            endpoint: "Strategy1.on_trade_tick_eth".to_string(),
            callback: None,
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