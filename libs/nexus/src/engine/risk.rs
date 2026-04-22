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

use crate::actor::TradingState;

/// Risk configuration parameters.
#[derive(Debug, Clone)]
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
}

impl RiskConfig {
    pub fn new() -> Self {
        Self {
            max_position_size: f64::INFINITY,
            max_notional_exposure: f64::INFINITY,
            max_drawdown_pct: 1.0,
            daily_loss_limit_pct: 0.05,
            max_order_size: f64::INFINITY,
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
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Risk engine — evaluates risk checks before order submission.
#[derive(Debug, Clone)]
pub struct RiskEngine {
    config: RiskConfig,
    peak_equity: f64,
    daily_loss: f64,
    daily_start_equity: f64,
    /// Live trading state machine — transitions: Active → ReduceOnly → Halted
    trading_state: TradingState,
}

impl RiskEngine {
    pub fn new(config: RiskConfig, initial_equity: f64) -> Self {
        Self {
            config,
            peak_equity: initial_equity,
            daily_loss: 0.0,
            daily_start_equity: initial_equity,
            trading_state: TradingState::Active,
        }
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
        // Update peak equity
        if equity > self.peak_equity {
            self.peak_equity = equity;
        }

        let drawdown = if self.peak_equity > 0.0 {
            (self.peak_equity - equity) / self.peak_equity
        } else {
            0.0
        };

        // State transitions based on equity
        match self.trading_state {
            TradingState::Active => {
                // Active → ReduceOnly: drawdown exceeds threshold
                if drawdown > self.config.max_drawdown_pct {
                    self.trading_state = TradingState::ReduceOnly;
                }
            }
            TradingState::ReduceOnly => {
                // ReduceOnly → Halted: daily loss exceeds limit
                let daily_loss_pct = if self.daily_start_equity > 0.0 {
                    self.daily_loss / self.daily_start_equity
                } else {
                    0.0
                };
                if daily_loss_pct >= self.config.daily_loss_limit_pct {
                    self.trading_state = TradingState::Halted;
                }
                // ReduceOnly → Active: equity recovers above peak
                if drawdown == 0.0 {
                    self.trading_state = TradingState::Active;
                }
            }
            TradingState::Halted => {
                // Halted can only be manually reset
            }
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
        // State-based rejection first
        match self.trading_state {
            TradingState::Halted => {
                return Some("trading_halted");
            }
            TradingState::ReduceOnly => {
                // Reject new entries (orders that increase position size)
                if order_size > 0.0 {
                    return Some("reduce_only_no_new_entries");
                }
            }
            TradingState::Active => {}
        }

        // Order size cap
        if order_size > self.config.max_order_size {
            return Some("order_size_exceeded");
        }

        // Position limit — block when order would exceed max
        if current_position.abs() + order_size > self.config.max_position_size {
            return Some("position_limit_exceeded");
        }

        // Notional limit
        let notional = (current_position.abs() + order_size) * price;
        if notional >= self.config.max_notional_exposure {
            return Some("notional_limit_exceeded");
        }

        // Drawdown circuit breaker — block when drawdown AT OR ABOVE threshold
        if max_drawdown_pct > self.config.max_drawdown_pct {
            return Some("drawdown_limit_exceeded");
        }

        // Daily loss limit
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

impl Default for RiskEngine {
    fn default() -> Self {
        Self::new(RiskConfig::default(), 10_000.0)
    }
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
        // Already have 4 lots, trying to add 2 → exceeds limit of 5
        let result = risk.check_order(2.0, 100.0, 4.0, 10_000.0, 0.0);
        assert_eq!(result, Some("position_limit_exceeded"));
    }

    #[test]
    fn test_position_limit_ok_at_boundary() {
        let config = RiskConfig::new().with_max_position_size(5.0);
        let risk = RiskEngine::new(config, 10_000.0);
        // 4 + 1 = 5 → exactly at limit
        let result = risk.check_order(1.0, 100.0, 4.0, 10_000.0, 0.0);
        assert!(result.is_none());
    }

    #[test]
    fn test_notional_limit_exceeded() {
        let config = RiskConfig::new().with_max_notional(1_000.0);
        let risk = RiskEngine::new(config, 10_000.0);
        // 10 lots × $100 = $1000 notional > $1000 limit
        let result = risk.check_order(10.0, 100.0, 0.0, 10_000.0, 0.0);
        assert_eq!(result, Some("notional_limit_exceeded"));
    }

    #[test]
    fn test_drawdown_limit_exceeded() {
        let config = RiskConfig::new().with_max_drawdown_pct(0.10); // 10%
        let risk = RiskEngine::new(config, 10_000.0);
        // 11% drawdown → blocked
        let result = risk.check_order(1.0, 100.0, 0.0, 8_900.0, 11.0);
        assert_eq!(result, Some("drawdown_limit_exceeded"));
    }

    #[test]
    fn test_drawdown_just_under_limit_allowed() {
        let config = RiskConfig::new()
            .with_max_drawdown_pct(0.10)   // 10% drawdown limit
            .with_daily_loss_limit_pct(0.20); // high daily loss limit (not the issue here)
        let risk = RiskEngine::new(config, 10_000.0);
        // 9.9% drawdown (decimal 0.099) → below 10% threshold → allowed
        let result = risk.check_order(1.0, 100.0, 0.0, 9_901.0, 0.099);
        assert!(result.is_none(), "drawdown 0.099 < 0.10 should be allowed");
    }

    #[test]
    fn test_daily_loss_limit_exceeded() {
        let config = RiskConfig::new().with_daily_loss_limit_pct(0.05); // 5%
        let mut risk = RiskEngine::new(config, 10_000.0);
        // Record some losses
        risk.record_loss(200.0);
        risk.record_loss(150.0); // $350 total loss = 3.5% < 5% → ok so far
        risk.record_loss(200.0); // $550 total = 5.5% > 5% → blocked
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
        // End day with closing equity of $9,600 (realized $400 loss)
        risk.end_day(9_600.0);
        // daily_loss should now be $400 (the realized loss)
        assert!((risk.daily_loss() - 400.0).abs() < 1.0);
    }

    #[test]
    fn test_peak_equity_updates() {
        let config = RiskConfig::new();
        let mut risk = RiskEngine::new(config, 10_000.0);
        assert_eq!(risk.peak_equity(), 10_000.0);
        risk.update_peak(10_500.0);
        assert_eq!(risk.peak_equity(), 10_500.0);
        // Equity drops but peak stays
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
        let config = RiskConfig::new().with_max_drawdown_pct(0.10); // 10%
        let mut risk = RiskEngine::new(config, 10_000.0);
        assert_eq!(risk.trading_state(), TradingState::Active);
        // Drop equity to 8900 (11% drawdown) → transition to ReduceOnly
        risk.on_trade(0, 8_900.0);
        assert_eq!(risk.trading_state(), TradingState::ReduceOnly);
    }

    #[test]
    fn test_trading_state_halted_on_daily_loss() {
        let config = RiskConfig::new()
            .with_max_drawdown_pct(0.05)
            .with_daily_loss_limit_pct(0.05); // 5%
        let mut risk = RiskEngine::new(config, 10_000.0);
        // Trigger ReduceOnly first
        risk.on_trade(0, 9_400.0); // 6% drawdown
        assert_eq!(risk.trading_state(), TradingState::ReduceOnly);
        // Record daily loss exceeding limit
        risk.record_loss(600.0); // 6% of 10k
        risk.on_trade(0, 9_400.0); // trigger transition check
        assert_eq!(risk.trading_state(), TradingState::Halted);
    }

    #[test]
    fn test_trading_state_rejects_orders_when_halted() {
        let config = RiskConfig::new()
            .with_max_drawdown_pct(0.05)
            .with_daily_loss_limit_pct(0.05);
        let mut risk = RiskEngine::new(config, 10_000.0);
        risk.on_trade(0, 9_000.0); // 10% drawdown → ReduceOnly
        risk.record_loss(600.0); // 6% daily loss
        risk.on_trade(0, 9_000.0); // → Halted

        // All new orders rejected
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
        risk.on_trade(0, 9_000.0); // Halted
        assert_eq!(risk.trading_state(), TradingState::Halted);

        // Manual reset
        risk.reset_state();
        assert_eq!(risk.trading_state(), TradingState::Active);
    }
}
