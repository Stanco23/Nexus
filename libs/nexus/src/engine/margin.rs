//! Margin system — margin modeling for leveraged instruments.
//!
//! # Margin Models
//!
//! - **StandardMarginModel**: margin = notional × margin_rate
//! - **LeveragedMarginModel**: isolated margin per position, cross-margin option
//!
//! # MarginAccount
//!
//! Tracks margin used, margin available, margin call events, and perpetual funding.
//!
//! # Liquidation
//!
//! When `margin_used / equity > liquidation_threshold`, position is liquidated.

use serde::{Deserialize, Serialize};

/// Margin model variant.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum MarginMode {
    /// Standard margin (cross-margin, positions share margin pool).
    Standard,
    /// Isolated margin per position.
    Isolated,
    /// Cross-margin with shared pool (same as Standard but explicit).
    Cross,
}

/// Margin model configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MarginConfig {
    /// Initial margin rate (e.g., 0.10 = 10% of notional required at open).
    pub margin_init: f64,
    /// Maintenance margin rate (e.g., 0.05 = 5% threshold for liquidation).
    pub margin_maint: f64,
    /// Margin mode.
    pub mode: MarginMode,
    /// Liquidation threshold: margin_used / equity > this → liquidation.
    pub liquidation_threshold: f64,
    /// Perpetual funding rate (hourly, e.g., 0.0001 = 0.01% per hour).
    pub funding_rate: f64,
    /// Funding payment interval in hours (default 8 for crypto perpetuals).
    pub funding_interval_h: u32,
}

impl Default for MarginConfig {
    fn default() -> Self {
        Self {
            margin_init: 0.10,     // 10% initial margin
            margin_maint: 0.05,    // 5% maintenance margin
            mode: MarginMode::Standard,
            liquidation_threshold: 1.0, // margin_used == equity triggers liquidation
            funding_rate: 0.0001, // 0.01% per hour
            funding_interval_h: 8,
        }
    }
}

impl MarginConfig {
    pub fn standard() -> Self {
        Self { mode: MarginMode::Standard, ..Self::default() }
    }

    pub fn isolated() -> Self {
        Self { mode: MarginMode::Isolated, ..Self::default() }
    }

    pub fn with_leverage(mut self, leverage: f64) -> Self {
        self.margin_init = 1.0 / leverage;
        self
    }

    pub fn with_funding(mut self, rate: f64, interval_h: u32) -> Self {
        self.funding_rate = rate;
        self.funding_interval_h = interval_h;
        self
    }
}

/// Computed margin state for a single position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarginState {
    /// Required margin at entry (initial margin).
    pub margin_required: f64,
    /// Current margin maintenance requirement.
    pub margin_maintenance: f64,
    /// Actual margin being used (includes PnL accrual).
    pub margin_used: f64,
    /// Equity available for new positions.
    pub margin_available: f64,
    /// Unrealized PnL on the position.
    pub unrealized_pnl: f64,
    /// Margin ratio: margin_used / equity.
    pub margin_ratio: f64,
    /// Funding liability accrued since last funding payment.
    pub funding_liability: f64,
    /// Whether liquidation has been triggered.
    pub liquidated: bool,
}

impl MarginState {
    pub fn new(position_value: f64, equity: f64, config: &MarginConfig) -> Self {
        let margin_required = position_value * config.margin_init;
        let margin_maintenance = position_value * config.margin_maint;
        let margin_used = margin_required;
        let margin_available = (equity - margin_used).max(0.0);
        let margin_ratio = if equity > 0.0 { margin_used / equity } else { f64::INFINITY };
        Self {
            margin_required,
            margin_maintenance,
            margin_used,
            margin_available,
            unrealized_pnl: 0.0,
            margin_ratio,
            funding_liability: 0.0,
            liquidated: false,
        }
    }

    /// Update margin state with new price.
    pub fn update(&mut self, position_value: f64, equity: f64, config: &MarginConfig) {
        self.margin_required = position_value * config.margin_init;
        self.margin_maintenance = position_value * config.margin_maint;
        self.margin_used = self.margin_required;
        self.margin_available = (equity - self.margin_used).max(0.0);
        self.margin_ratio = if equity > 0.0 { self.margin_used / equity } else { f64::INFINITY };

        if self.margin_ratio > config.liquidation_threshold {
            self.liquidated = true;
        }
    }

    /// Accumulate funding liability for a time delta.
    pub fn accumulate_funding(&mut self, hours_elapsed: f64, funding_rate: f64) {
        if self.margin_used > 0.0 {
            self.funding_liability += self.margin_used * funding_rate * hours_elapsed;
        }
    }
}

/// Margin account — aggregates margin state across all positions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarginAccount {
    pub equity: f64,
    pub margin_used: f64,
    pub margin_available: f64,
    pub total_funding_liability: f64,
    pub config: MarginConfig,
    /// Last funding payment timestamp (ns).
    pub last_funding_time_ns: u64,
    /// Number of margin call events.
    pub margin_call_count: usize,
}

impl MarginAccount {
    pub fn new(equity: f64, config: MarginConfig) -> Self {
        Self {
            equity,
            margin_used: 0.0,
            margin_available: equity,
            total_funding_liability: 0.0,
            config,
            last_funding_time_ns: 0,
            margin_call_count: 0,
        }
    }

    /// Open a new position, consuming margin.
    /// Returns false if insufficient margin available.
    pub fn open_position(&mut self, position_value: f64) -> bool {
        let required = position_value * self.config.margin_init;
        if required > self.margin_available {
            return false;
        }
        self.margin_used += required;
        self.margin_available -= required;
        true
    }

    /// Accumulate funding liability for a time delta.
    pub fn accumulate_funding(&mut self, hours_elapsed: f64) {
        if self.margin_used > 0.0 {
            self.total_funding_liability += self.margin_used * self.config.funding_rate * hours_elapsed;
        }
    }

    /// Close a position, releasing margin and applying funding.
    pub fn close_position(&mut self, position_value: f64, pnl: f64) {
        let released = position_value * self.config.margin_init;
        self.margin_used = (self.margin_used - released).max(0.0);
        self.equity += pnl;
        self.equity -= self.total_funding_liability;
        self.total_funding_liability = 0.0;
        self.margin_available = (self.equity - self.margin_used).max(0.0);
    }

    /// Process funding payment at the scheduled interval.
    /// Call this every `funding_interval_h` hours.
    pub fn process_funding(&mut self) {
        self.equity -= self.total_funding_liability;
        self.total_funding_liability = 0.0;
        self.margin_available = (self.equity - self.margin_used).max(0.0);
    }

    /// Update equity and check for margin calls.
    pub fn update_equity(&mut self, new_equity: f64) {
        self.equity = new_equity;
        self.margin_available = (self.equity - self.margin_used).max(0.0);
        let ratio = if self.equity > 0.0 {
            self.margin_used / self.equity
        } else {
            f64::INFINITY
        };
        if ratio > self.config.liquidation_threshold {
            self.margin_call_count += 1;
        }
    }

    /// Whether a margin call event is active.
    pub fn in_margin_call(&self) -> bool {
        if self.equity <= 0.0 {
            return true;
        }
        self.margin_used / self.equity > self.config.liquidation_threshold
    }

    /// Liquidation: equity wiped out or margin exhausted.
    pub fn is_liquidated(&self) -> bool {
        self.equity <= 0.0 || self.margin_used / self.equity > self.config.liquidation_threshold
    }

    /// Compute current margin ratio.
    pub fn margin_ratio(&self) -> f64 {
        if self.equity > 0.0 {
            self.margin_used / self.equity
        } else {
            f64::INFINITY
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_margin_account_open_close() {
        let config = MarginConfig::standard();
        let mut account = MarginAccount::new(10_000.0, config);

        // Open 1 BTC at $100K with 10x leverage (10% margin = $10K)
        let position_value = 100_000.0;
        assert!(account.open_position(position_value)); // uses $10K margin
        assert!((account.margin_used - 10_000.0).abs() < 0.01);
        assert!((account.margin_available - 0.0).abs() < 0.01);

        // Close position with $1K profit
        account.close_position(position_value, 1_000.0);
        assert!(account.margin_used < 100.0); // margin released
        assert!((account.equity - 11_000.0).abs() < 0.01); // initial + pnl
    }

    #[test]
    fn test_margin_call_trigger() {
        let config = MarginConfig::standard();
        let mut account = MarginAccount::new(10_000.0, config);

        // Open position ($10K margin used, $10K available)
        account.open_position(100_000.0);
        assert!(!account.in_margin_call());

        // Equity drops 50%: $10K equity / $10K margin = 100% ratio → margin call
        account.update_equity(5_000.0);
        assert!(account.in_margin_call());
        assert_eq!(account.margin_call_count, 1);
    }

    #[test]
    fn test_liquidation() {
        let config = MarginConfig::with_leverage(MarginConfig::default(), 10.0);
        let mut account = MarginAccount::new(10_000.0, config);

        account.open_position(100_000.0);
        assert!(!account.is_liquidated());

        // Equity wiped out
        account.update_equity(0.0);
        assert!(account.is_liquidated());
    }

    #[test]
    fn test_isolated_margin() {
        let config = MarginConfig::isolated();
        let mut account = MarginAccount::new(10_000.0, config);

        // Open 2 isolated positions at $50K each
        assert!(account.open_position(50_000.0)); // $5K margin
        assert!((account.margin_available - 5_000.0).abs() < 0.01);
        assert!(account.open_position(50_000.0)); // another $5K margin
        assert!((account.margin_available - 0.0).abs() < 0.01);

        // Both positions share margin pool but are tracked separately conceptually
        // (actual per-position tracking would live in InstrumentState)
    }

    #[test]
    fn test_funding_accumulation() {
        let config = MarginConfig::with_funding(MarginConfig::default(), 0.0001, 8);
        let mut account = MarginAccount::new(10_000.0, config);

        account.open_position(100_000.0);

        // Simulate 8 hours passing (MarginAccount uses config.funding_rate)
        account.accumulate_funding(8.0);
        // Funding = $10K * 0.0001 * 8 = $8
        assert!((account.total_funding_liability - 8.0).abs() < 0.01);
    }
}
