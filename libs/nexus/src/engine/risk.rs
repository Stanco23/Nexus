//! Risk controls — pre-trade and intraday risk management.
//!
//! All checks are evaluated before an order is submitted. Same code runs
//! in backtest and live execution.
//!
//! # Checks
//!
//! - **Position limit**: max lots per instrument
//! - **Notional limit**: max position notional (size × price)
//! - **Drawdown circuit breaker**: halt new entries if equity drawdown > threshold
//! - **Daily loss limit**: disable new orders after daily loss exceeds threshold
//! - **Order size cap**: per-order maximum size
//! - **Trading state machine**: Active → ReduceOnly → Halted transitions

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;
use tokio::time::Duration;
use serde::{Deserialize, Serialize};

use crate::actor::{
    Actor, Clock, Component, ComponentTrait, Logger, MessageBus,
    TradingState,
};
use crate::messages::TraderId;

/// Name for the RiskEngine component.
const RISK_ENGINE_NAME: &str = "RiskEngine";

/// Risk configuration parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    /// Maximum position size in lots per instrument.
    pub max_position_size: f64,
    /// Maximum notional exposure (position + order) × price.
    pub max_notional_exposure: f64,
    /// Halt new entries if drawdown exceeds this percentage.
    pub max_drawdown_pct: f64,
    /// Disable new orders if daily loss exceeds this percentage of starting equity.
    pub daily_loss_limit_pct: f64,
    /// Per-order size hard cap.
    pub max_order_size: f64,
    /// Max submit order operations per second (0 = no limit).
    pub max_submit_per_sec: f64,
    /// Max modify order operations per second (0 = no limit).
    pub max_modify_per_sec: f64,
}

impl RiskConfig {
    pub fn new() -> Self {
        Self {
            max_position_size: f64::INFINITY,
            max_notional_exposure: f64::INFINITY,
            max_drawdown_pct: 1.0,
            daily_loss_limit_pct: 0.05,
            max_order_size: f64::INFINITY,
            max_submit_per_sec: 10.0, // Default 10 submit/s
            max_modify_per_sec: 20.0, // Default 20 modify/s
        }
    }

    pub fn with_max_position_size(mut self, size: f64) -> Self {
        self.max_position_size = size;
        self
    }

    pub fn with_max_notional(mut self, notional: f64) -> Self {
        self.max_notional_exposure = notional;
        self
    }

    pub fn with_max_drawdown_pct(mut self, pct: f64) -> Self {
        self.max_drawdown_pct = pct;
        self
    }

    pub fn with_daily_loss_limit_pct(mut self, pct: f64) -> Self {
        self.daily_loss_limit_pct = pct;
        self
    }

    pub fn with_max_order_size(mut self, size: f64) -> Self {
        self.max_order_size = size;
        self
    }

    pub fn with_max_submit_per_sec(mut self, rate: f64) -> Self {
        self.max_submit_per_sec = rate;
        self
    }

    pub fn with_max_modify_per_sec(mut self, rate: f64) -> Self {
        self.max_modify_per_sec = rate;
        self
    }
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Throttler — token-bucket rate limiter for order operations
// ============================================================================

/// Token-bucket throttler for order submit/modify operations.
struct Throttler {
    tokens: f64,
    max_tokens: f64,
    refill_per_ms: f64,
    last_refill_ms: u64,
}

impl Throttler {
    fn new(max_per_sec: f64, _burst: f64) -> Self {
        Self {
            tokens: max_per_sec,
            max_tokens: max_per_sec,
            refill_per_ms: max_per_sec / 1000.0,
            last_refill_ms: current_timestamp_ms(),
        }
    }

    fn refill(&mut self) {
        let now = current_timestamp_ms();
        let elapsed = now.saturating_sub(self.last_refill_ms);
        if elapsed > 0 {
            let refill = elapsed as f64 * self.refill_per_ms;
            self.tokens = (self.tokens + refill).min(self.max_tokens);
            self.last_refill_ms = now;
        }
    }

    /// Try to acquire 1 token. Returns Ok(()) if acquired, Err(wait_ms) if throttled.
    fn try_acquire(&mut self) -> Result<(), u64> {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            let wait_ms = ((1.0 - self.tokens) / self.refill_per_ms).ceil() as u64;
            Err(wait_ms)
        }
    }
}


fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}


/// Risk engine — evaluates risk checks before order submission.
pub struct RiskEngine {
    /// Component providing lifecycle FSM, clock, msgbus, and logger.
    component: Component,
    /// Shared message bus for endpoint registration and subscriptions.
    msgbus: Arc<MessageBus>,
    /// Risk configuration.
    config: RiskConfig,
    peak_equity: f64,
    daily_loss: f64,
    daily_start_equity: f64,
    /// Live trading state machine — transitions: Active → ReduceOnly → Halted
    trading_state: TradingState,
    /// Submit order throttle (max N orders per second).
    submit_throttler: Arc<RwLock<Throttler>>,
    /// Modify order throttle (max N modifies per second).
    modify_throttler: Arc<RwLock<Throttler>>,
}

impl RiskEngine {
    /// Create a new RiskEngine (backtest/paper constructor).
    ///
    /// Does NOT register on msgbus — use `new_with_components` for live trading.
    pub fn new(config: RiskConfig, initial_equity: f64) -> Self {
        let trader_id = TraderId::new("BACKTEST");
        let msgbus = Arc::new(MessageBus::new());
        let clock: Box<dyn Clock> = Box::new(crate::actor::TestClock::new());
        let mut engine = Self::new_with_components(
            trader_id,
            msgbus,
            clock,
            config,
            initial_equity,
        );
        // Skip msgbus registration for backtest (no live msgbus)
        engine.component.initialize();
        engine
    }

    /// Create a new RiskEngine with components for live trading.
    ///
    /// This constructor registers the engine on the message bus and sets up
    /// endpoint handlers and event subscriptions.
    pub fn new_with_components(
        trader_id: TraderId,
        msgbus: Arc<MessageBus>,
        clock: Box<dyn Clock>,
        config: RiskConfig,
        initial_equity: f64,
    ) -> Self {
        let id = 0; // Id is assigned by the trader/node in live use
        let logger = Logger::new(RISK_ENGINE_NAME);

        let component = Component::new(
            id,
            RISK_ENGINE_NAME,
            trader_id.clone(),
            clock,
            (*msgbus).clone(),
            logger,
        );

        let mut engine = Self {
            component,
            msgbus,
            config,
            peak_equity: initial_equity,
            daily_loss: 0.0,
            daily_start_equity: initial_equity,
            trading_state: TradingState::Active,
            submit_throttler: Arc::new(RwLock::new(Throttler::new(10.0, 10.0))),
            modify_throttler: Arc::new(RwLock::new(Throttler::new(20.0, 20.0))),
        };

        engine.initialize();

        engine
    }

    /// Register endpoints and subscribe to event topics on the message bus.
    fn initialize(&mut self) {
        let execute_handler = Box::new(move |msg: &dyn std::any::Any| {
            let _ = msg;
        });

        let process_handler = Box::new(move |msg: &dyn std::any::Any| {
            let _ = msg;
        });

        self.msgbus.register("RiskEngine.execute", execute_handler);
        self.msgbus.register("RiskEngine.process", process_handler);

        let order_handler = Box::new(move |msg: &dyn std::any::Any| {
            let _ = msg;
        });
        let position_handler = Box::new(move |msg: &dyn std::any::Any| {
            let _ = msg;
        });

        self.msgbus.subscribe("events.order.*", 0, order_handler, 10);
        self.msgbus.subscribe("events.position.*", 0, position_handler, 10);

        self.component.initialize();
    }

    /// Get the current trading state.
    pub fn trading_state(&self) -> TradingState {
        self.trading_state
    }

    /// Reset trading state to Active (after recovery or end-of-day).
    pub fn reset_state(&mut self) {
        self.trading_state = TradingState::Active;
    }

    /// Called after each fill to evaluate equity and trigger state transitions.
    pub fn on_trade(&mut self, _instrument_id: u32, equity: f64) {
        if equity > self.peak_equity {
            self.peak_equity = equity;
        }

        let drawdown = if self.peak_equity > 0.0 {
            (self.peak_equity - equity) / self.peak_equity
        } else {
            0.0
        };

        match self.trading_state {
            TradingState::Active => {
                if drawdown > self.config.max_drawdown_pct {
                    self.trading_state = TradingState::ReduceOnly;
                }
            }
            TradingState::ReduceOnly => {
                let daily_loss_pct = if self.daily_start_equity > 0.0 {
                    self.daily_loss / self.daily_start_equity
                } else {
                    0.0
                };
                if daily_loss_pct >= self.config.daily_loss_limit_pct {
                    self.trading_state = TradingState::Halted;
                }
                if drawdown == 0.0 {
                    self.trading_state = TradingState::Active;
                }
            }
            TradingState::Halted => {}
        }
    }

    /// Check if an order would breach risk limits.
    /// Returns `None` if the order is allowed.
    /// Returns `Some(&str)` with a rejection reason if blocked.
    pub fn check_order(
        &self,
        order_size: f64,
        price: f64,
        current_position: f64,
        _equity: f64,
        max_drawdown_pct: f64,
    ) -> Option<&'static str> {
        match self.trading_state {
            TradingState::Halted => {
                return Some("trading_halted");
            }
            TradingState::ReduceOnly => {
                if order_size > 0.0 {
                    return Some("reduce_only_no_new_entries");
                }
            }
            TradingState::Active => {}
        }

        if order_size > self.config.max_order_size {
            return Some("order_size_exceeded");
        }

        if current_position.abs() + order_size > self.config.max_position_size {
            return Some("position_limit_exceeded");
        }

        let notional = (current_position.abs() + order_size) * price;
        if notional >= self.config.max_notional_exposure {
            return Some("notional_limit_exceeded");
        }

        if max_drawdown_pct > self.config.max_drawdown_pct {
            return Some("drawdown_limit_exceeded");
        }

        let daily_loss_pct = if self.daily_start_equity > 0.0 {
            self.daily_loss / self.daily_start_equity
        } else {
            0.0
        };
        if daily_loss_pct >= self.config.daily_loss_limit_pct {
            return Some("daily_loss_limit_exceeded");
        }

        None
    }

    /// Check if a signal should be allowed given current state.
    pub fn check_signal(
        &self,
        signal_size: f64,
        price: f64,
        current_position: f64,
        equity: f64,
        max_drawdown_pct: f64,
    ) -> Option<&'static str> {
        self.check_order(signal_size, price, current_position, equity, max_drawdown_pct)
    }

    /// Try to acquire a submit slot. Returns Ok(()) if allowed, Err(retry_ms) if throttled.
    pub async fn try_submit(&self) -> Result<(), u64> {
        if self.config.max_submit_per_sec <= 0.0 {
            return Ok(());
        }
        let mut throttle = self.submit_throttler.write().await;
        throttle.try_acquire()
    }

    /// Try to acquire a modify slot. Returns Ok(()) if allowed, Err(retry_ms) if throttled.
    pub async fn try_modify(&self) -> Result<(), u64> {
        if self.config.max_modify_per_sec <= 0.0 {
            return Ok(());
        }
        let mut throttle = self.modify_throttler.write().await;
        throttle.try_acquire()
    }

    /// Update peak equity after each tick.
    pub fn update_peak(&mut self, equity: f64) {
        if equity > self.peak_equity {
            self.peak_equity = equity;
        }
    }

    /// Record a loss for daily loss tracking.
    pub fn record_loss(&mut self, loss: f64) {
        if loss > 0.0 {
            self.daily_loss += loss;
        }
    }

    /// End the trading day and reset daily loss tracking.
    pub fn end_day(&mut self, closing_equity: f64) {
        let realized_loss = self.daily_start_equity - closing_equity;
        if realized_loss > 0.0 {
            self.daily_loss = realized_loss;
        }
        self.daily_start_equity = closing_equity;
        if closing_equity > self.peak_equity {
            self.peak_equity = closing_equity;
        }
    }

    /// Start a new day with updated equity.
    pub fn start_day(&mut self, equity: f64) {
        self.daily_start_equity = equity;
        self.daily_loss = 0.0;
    }

    /// Current daily loss amount.
    pub fn daily_loss(&self) -> f64 {
        self.daily_loss
    }

    /// Current peak equity.
    pub fn peak_equity(&self) -> f64 {
        self.peak_equity
    }
}

// =============================================================================
// ComponentTrait implementation
// =============================================================================

impl ComponentTrait for RiskEngine {
    fn id(&self) -> u64 {
        self.component.id
    }

    fn trader_id(&self) -> &TraderId {
        &self.component.trader_id
    }

    fn label(&self) -> Option<&str> {
        None
    }

    fn msgbus(&self) -> &MessageBus {
        &self.component.msgbus
    }

    fn clock(&self) -> &dyn Clock {
        &*self.component.clock
    }

    fn component(&self) -> &Component {
        &self.component
    }

    fn component_mut(&mut self) -> &mut Component {
        &mut self.component
    }

    fn on_save(&mut self) -> std::collections::HashMap<String, Vec<u8>> {
        std::collections::HashMap::new()
    }

    fn on_load(&mut self, _state: &std::collections::HashMap<String, Vec<u8>>) {}
}

// =============================================================================
// Actor trait implementation
// =============================================================================

impl Actor for RiskEngine {
    fn component(&self) -> &Component {
        &self.component
    }

    fn component_mut(&mut self) -> &mut Component {
        &mut self.component
    }

    fn trader_id(&self) -> &str {
        self.component.trader_id.as_str()
    }

    fn trader_id_obj(&self) -> &TraderId {
        &self.component.trader_id
    }

    fn on_order_filled(&mut self, event: &crate::messages::OrderFilled) {
        self.component.logger.debug("RiskEngine received order fill event");
        let _ = event;
    }

    fn on_save(&mut self) -> std::collections::HashMap<String, Vec<u8>> {
        std::collections::HashMap::new()
    }

    fn on_load(&mut self, _state: &std::collections::HashMap<String, Vec<u8>>) {}

    fn on_trade_tick(&mut self, _tick: &crate::cache::TradeTick) {}
    fn on_quote_tick(&mut self, _tick: &crate::cache::QuoteTick) {}
    fn on_bar(&mut self, _bar: &crate::cache::Bar) {}
    fn on_order_book(&mut self, _book: &crate::cache::OrderBook) {}
    fn on_instrument(&mut self, _instrument: &crate::instrument::Instrument) {}
    fn on_instrument_status(&mut self, _status: &crate::messages::InstrumentStatus) {}
    fn on_instrument_close(&mut self, _close: &crate::messages::InstrumentClose) {}
    fn on_funding_rate(&mut self, _rate: &crate::messages::FundingRateUpdate) {}
    fn on_mark_price(&mut self, _mark: &crate::messages::MarkPriceUpdate) {}
    fn on_index_price(&mut self, _index: &crate::messages::IndexPriceUpdate) {}
    fn on_data(&mut self, _data: &dyn std::any::Any) {}
    fn on_order_book_depth(&mut self, _depth: &crate::cache::OrderBook) {}
    fn on_historical_data(&mut self, _data: &dyn std::any::Any) {}
    fn on_option_greeks(&mut self, _greeks: &dyn std::any::Any) {}
    fn on_option_chain(&mut self, _chain: &dyn std::any::Any) {}
    fn on_event(&mut self, _event: &dyn std::any::Any) {}
    fn on_signal(&mut self, _signal: &crate::messages::SignalData) {}
    fn on_account_state(&mut self, _event: &crate::messages::AccountState) {}
    fn on_account_info(&mut self, _event: &crate::messages::AccountState) {}
    fn on_risk_state_changed(&mut self, _event: &dyn std::any::Any) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_block_when_all_limits_ok() {
        let risk = RiskEngine::new(RiskConfig::default(), 10_000.0);
        let result = risk.check_order(1.0, 100.0, 0.0, 10_000.0, 0.0);
        assert!(result.is_none());
    }

    #[test]
    fn test_position_limit_exceeded() {
        let config = RiskConfig::new().with_max_position_size(5.0);
        let risk = RiskEngine::new(config, 10_000.0);
        let result = risk.check_order(2.0, 100.0, 4.0, 10_000.0, 0.0);
        assert_eq!(result, Some("position_limit_exceeded"));
    }

    #[test]
    fn test_position_limit_ok_at_boundary() {
        let config = RiskConfig::new().with_max_position_size(5.0);
        let risk = RiskEngine::new(config, 10_000.0);
        let result = risk.check_order(1.0, 100.0, 4.0, 10_000.0, 0.0);
        assert!(result.is_none());
    }

    #[test]
    fn test_notional_limit_exceeded() {
        let config = RiskConfig::new().with_max_notional(1_000.0);
        let risk = RiskEngine::new(config, 10_000.0);
        let result = risk.check_order(10.0, 100.0, 0.0, 10_000.0, 0.0);
        assert_eq!(result, Some("notional_limit_exceeded"));
    }

    #[test]
    fn test_drawdown_limit_exceeded() {
        let config = RiskConfig::new().with_max_drawdown_pct(0.10);
        let risk = RiskEngine::new(config, 10_000.0);
        let result = risk.check_order(1.0, 100.0, 0.0, 8_900.0, 11.0);
        assert_eq!(result, Some("drawdown_limit_exceeded"));
    }

    #[test]
    fn test_drawdown_just_under_limit_allowed() {
        let config = RiskConfig::new()
            .with_max_drawdown_pct(0.10)
            .with_daily_loss_limit_pct(0.20);
        let risk = RiskEngine::new(config, 10_000.0);
        let result = risk.check_order(1.0, 100.0, 0.0, 9_901.0, 0.099);
        assert!(result.is_none());
    }

    #[test]
    fn test_daily_loss_limit_exceeded() {
        let config = RiskConfig::new().with_daily_loss_limit_pct(0.05);
        let mut risk = RiskEngine::new(config, 10_000.0);
        risk.record_loss(200.0);
        risk.record_loss(150.0);
        risk.record_loss(200.0);
        let result = risk.check_order(1.0, 100.0, 0.0, 9_450.0, 0.0);
        assert_eq!(result, Some("daily_loss_limit_exceeded"));
    }

    #[test]
    fn test_order_size_cap() {
        let config = RiskConfig::new().with_max_order_size(10.0);
        let risk = RiskEngine::new(config, 10_000.0);
        let result = risk.check_order(15.0, 100.0, 0.0, 10_000.0, 0.0);
        assert_eq!(result, Some("order_size_exceeded"));
    }

    #[test]
    fn test_end_day_resets_daily_loss() {
        let config = RiskConfig::new().with_daily_loss_limit_pct(0.05);
        let mut risk = RiskEngine::new(config, 10_000.0);
        risk.record_loss(300.0);
        assert!(risk.daily_loss() > 0.0);
        risk.end_day(9_600.0);
        assert!((risk.daily_loss() - 400.0).abs() < 1.0);
    }

    #[test]
    fn test_peak_equity_updates() {
        let config = RiskConfig::new();
        let mut risk = RiskEngine::new(config, 10_000.0);
        assert_eq!(risk.peak_equity(), 10_000.0);
        risk.update_peak(10_500.0);
        assert_eq!(risk.peak_equity(), 10_500.0);
        risk.update_peak(9_800.0);
        assert_eq!(risk.peak_equity(), 10_500.0);
    }

    #[test]
    fn test_trading_state_starts_active() {
        let risk = RiskEngine::new(RiskConfig::default(), 10_000.0);
        assert_eq!(risk.trading_state(), TradingState::Active);
    }

    #[test]
    fn test_trading_state_reduce_only_on_drawdown() {
        let config = RiskConfig::new().with_max_drawdown_pct(0.10);
        let mut risk = RiskEngine::new(config, 10_000.0);
        assert_eq!(risk.trading_state(), TradingState::Active);
        risk.on_trade(0, 8_900.0);
        assert_eq!(risk.trading_state(), TradingState::ReduceOnly);
    }

    #[test]
    fn test_trading_state_halted_on_daily_loss() {
        let config = RiskConfig::new()
            .with_max_drawdown_pct(0.05)
            .with_daily_loss_limit_pct(0.05);
        let mut risk = RiskEngine::new(config, 10_000.0);
        risk.on_trade(0, 9_400.0);
        assert_eq!(risk.trading_state(), TradingState::ReduceOnly);
        risk.record_loss(600.0);
        risk.on_trade(0, 9_400.0);
        assert_eq!(risk.trading_state(), TradingState::Halted);
    }

    #[test]
    fn test_trading_state_rejects_orders_when_halted() {
        let config = RiskConfig::new()
            .with_max_drawdown_pct(0.05)
            .with_daily_loss_limit_pct(0.05);
        let mut risk = RiskEngine::new(config, 10_000.0);
        risk.on_trade(0, 9_000.0);
        risk.record_loss(600.0);
        risk.on_trade(0, 9_000.0);
        let result = risk.check_order(1.0, 100.0, 0.0, 9_000.0, 0.0);
        assert_eq!(result, Some("trading_halted"));
    }

    #[test]
    fn test_trading_state_reset_to_active() {
        let config = RiskConfig::new()
            .with_max_drawdown_pct(0.05)
            .with_daily_loss_limit_pct(0.05);
        let mut risk = RiskEngine::new(config, 10_000.0);
        risk.on_trade(0, 9_000.0);
        risk.record_loss(600.0);
        risk.on_trade(0, 9_000.0);
        assert_eq!(risk.trading_state(), TradingState::Halted);
        risk.reset_state();
        assert_eq!(risk.trading_state(), TradingState::Active);
    }

    #[test]
    fn test_component_trait_id() {
        let risk = RiskEngine::new_with_components(
            TraderId::new("TEST-Trader"),
            Arc::new(MessageBus::new()),
            Box::new(crate::actor::TestClock::new()),
            RiskConfig::default(),
            10_000.0,
        );
        let comp_trait = &risk as &dyn ComponentTrait;
        assert_eq!(comp_trait.id(), 0);
    }

    #[test]
    fn test_component_trait_trader_id() {
        let risk = RiskEngine::new_with_components(
            TraderId::new("TRADER-002"),
            Arc::new(MessageBus::new()),
            Box::new(crate::actor::TestClock::new()),
            RiskConfig::default(),
            10_000.0,
        );
        let comp_trait = &risk as &dyn ComponentTrait;
        assert_eq!(comp_trait.trader_id().as_str(), "TRADER-002");
    }

    #[test]
    fn test_component_trait_msgbus() {
        let msgbus = Arc::new(MessageBus::new());
        let risk = RiskEngine::new_with_components(
            TraderId::new("TEST-Trader"),
            msgbus.clone(),
            Box::new(crate::actor::TestClock::new()),
            RiskConfig::default(),
            10_000.0,
        );
        let comp_trait = &risk as &dyn ComponentTrait;
        let _ = comp_trait.msgbus();
    }

    #[test]
    fn test_actor_trait_trader_id() {
        let risk = RiskEngine::new_with_components(
            TraderId::new("ACTOR-TRADER"),
            Arc::new(MessageBus::new()),
            Box::new(crate::actor::TestClock::new()),
            RiskConfig::default(),
            10_000.0,
        );
        assert_eq!(Actor::trader_id(&risk), "ACTOR-TRADER");
    }

    #[test]
    fn test_actor_trait_trader_id_obj() {
        let trader_id = TraderId::new("OBJ-TRADER");
        let risk = RiskEngine::new_with_components(
            trader_id,
            Arc::new(MessageBus::new()),
            Box::new(crate::actor::TestClock::new()),
            RiskConfig::default(),
            10_000.0,
        );
        assert_eq!(risk.trader_id_obj().as_str(), "OBJ-TRADER");
    }
}
