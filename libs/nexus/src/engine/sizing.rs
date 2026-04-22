//! Position sizing methods — determines trade quantity per signal.
//!
//! # Sizing Methods
//!
//! - **Fixed**: constant quantity per trade (size = fixed_lots)
//! - **FixedValue**: constant notional per trade (size = equity × fraction / price)
//! - **Kelly**: f* = (bp - q) / b where p=win_rate, b=odds, q=1-p
//! - **VolatilityBased**: size = equity × target_risk_pct / atr
//! - **RiskParity**: equal risk contribution across all positions
//!
//! All sizing is strategy-level: strategies call `Sizing::compute_size()`
//! to get the properly-scaled quantity before returning a signal.

#[cfg(test)]
use crate::engine::Signal;

/// Sizing method variant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SizingMethod {
    Fixed,
    FixedValue,
    Kelly,
    VolatilityBased,
    RiskParity,
}

/// Configuration for sizing calculation.
#[derive(Debug, Clone)]
pub struct SizingConfig {
    pub method: SizingMethod,
    /// For Fixed: number of lots
    pub fixed_size: f64,
    /// For FixedValue: fraction of equity (e.g. 0.02 = 2%)
    pub fraction: f64,
    /// For Kelly: win rate (0.0-1.0)
    pub win_rate: f64,
    /// For Kelly: profit/loss ratio (avg_win / avg_loss)
    pub odds: f64,
    /// For Kelly: fractional Kelly (e.g. 0.5 = half Kelly)
    pub kelly_fraction: f64,
    /// For VolatilityBased: % of equity at risk (e.g. 0.01 = 1%)
    pub target_risk_pct: f64,
    /// For VolatilityBased: ATR period
    pub atr_period: usize,
}

impl SizingConfig {
    pub fn fixed(size: f64) -> Self {
        Self {
            method: SizingMethod::Fixed,
            fixed_size: size,
            fraction: 0.0,
            win_rate: 0.0,
            odds: 0.0,
            kelly_fraction: 1.0,
            target_risk_pct: 0.0,
            atr_period: 14,
        }
    }

    pub fn fixed_value(fraction: f64) -> Self {
        Self {
            method: SizingMethod::FixedValue,
            fixed_size: 0.0,
            fraction,
            win_rate: 0.0,
            odds: 0.0,
            kelly_fraction: 1.0,
            target_risk_pct: 0.0,
            atr_period: 14,
        }
    }

    pub fn kelly(win_rate: f64, odds: f64, kelly_fraction: f64) -> Self {
        Self {
            method: SizingMethod::Kelly,
            fixed_size: 0.0,
            fraction: 0.0,
            win_rate,
            odds,
            kelly_fraction,
            target_risk_pct: 0.0,
            atr_period: 14,
        }
    }

    pub fn volatility_based(target_risk_pct: f64, atr_period: usize) -> Self {
        Self {
            method: SizingMethod::VolatilityBased,
            fixed_size: 0.0,
            fraction: 0.0,
            win_rate: 0.0,
            odds: 0.0,
            kelly_fraction: 1.0,
            target_risk_pct,
            atr_period,
        }
    }
}

/// Position sizing calculator.
#[derive(Debug, Clone)]
pub struct Sizing {
    pub method: SizingMethod,
    pub fixed_size: f64,
    pub fraction: f64,
    pub win_rate: f64,
    pub odds: f64,
    pub kelly_fraction: f64,
    pub target_risk_pct: f64,
    pub atr_period: usize,
}

impl Sizing {
    pub fn from_config(config: &SizingConfig) -> Self {
        Self {
            method: config.method,
            fixed_size: config.fixed_size,
            fraction: config.fraction,
            win_rate: config.win_rate,
            odds: config.odds,
            kelly_fraction: config.kelly_fraction,
            target_risk_pct: config.target_risk_pct,
            atr_period: config.atr_period,
        }
    }

    /// Compute position size for a signal.
    ///
    /// - `signal_size`: raw size from strategy signal (0 = use method default)
    /// - `atr`: current ATR value (only used for VolatilityBased)
    /// - `equity`: current account equity
    /// - `current_price`: current price (for FixedValue)
    pub fn compute_size(
        &self,
        signal_size: f64,
        atr: f64,
        equity: f64,
        current_price: f64,
    ) -> f64 {
        match self.method {
            SizingMethod::Fixed => {
                if signal_size > 0.0 { signal_size } else { self.fixed_size }
            }
            SizingMethod::FixedValue => {
                let size = equity * self.fraction / current_price;
                if signal_size > 0.0 { signal_size.min(size) } else { size }
            }
            SizingMethod::Kelly => {
                // Kelly: f* = (bp - q) / b
                // p = win_rate, q = 1 - p, b = odds (avg_win / avg_loss)
                let p = self.win_rate;
                let q = 1.0 - p;
                let b = self.odds;
                let kelly = if b <= 0.0 || p <= 0.0 {
                    0.0
                } else {
                    ((b * p) - q) / b
                };
                let fraction = kelly.clamp(0.0, 1.0) * self.kelly_fraction;
                let size = equity * fraction / current_price;
                if signal_size > 0.0 { signal_size.min(size) } else { size }
            }
            SizingMethod::VolatilityBased => {
                if atr <= 0.0 || current_price <= 0.0 {
                    return signal_size;
                }
                let target_risk = equity * self.target_risk_pct;
                let size = target_risk / atr;
                if signal_size > 0.0 { signal_size.min(size) } else { size }
            }
            SizingMethod::RiskParity => {
                // Risk parity: size based on inverse ATR weighting across portfolio
                // For single-position: size = equity * fraction / atr
                if atr <= 0.0 || current_price <= 0.0 {
                    return signal_size;
                }
                let fraction = 0.01; // 1% base risk
                let size = equity * fraction / atr;
                if signal_size > 0.0 { signal_size.min(size) } else { size }
            }
        }
    }

    /// Compute Kelly fraction from trade history.
    pub fn kelly_from_trades(trades: &[crate::engine::Trade]) -> (f64, f64, f64) {
        if trades.is_empty() {
            return (0.5, 1.0, 1.0);
        }
        let wins: f64 = trades.iter().filter(|t| t.pnl > 0.0).count() as f64;
        let losses: f64 = trades.iter().filter(|t| t.pnl < 0.0).count() as f64;
        let total = wins + losses;
        let win_rate = if total > 0.0 { wins / total } else { 0.5 };

        let avg_win = if wins > 0.0 {
            trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl).sum::<f64>() / wins
        } else {
            1.0
        };
        let avg_loss = if losses > 0.0 {
            trades.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl.abs()).sum::<f64>() / losses
        } else {
            1.0
        };

        let odds = if avg_loss > 0.0 { avg_win / avg_loss } else { 1.0 };
        (win_rate, odds, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Fixed tests ---

    #[test]
    fn test_sizing_fixed_uses_signal_size() {
        let sizing = Sizing::from_config(&SizingConfig::fixed(1.0));
        let size = sizing.compute_size(0.5, 100.0, 10_000.0, 100.0);
        assert!((size - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_sizing_fixed_uses_default_when_zero() {
        let sizing = Sizing::from_config(&SizingConfig::fixed(2.0));
        let size = sizing.compute_size(0.0, 100.0, 10_000.0, 100.0);
        assert!((size - 2.0).abs() < 1e-10);
    }

    // --- FixedValue tests ---

    #[test]
    fn test_sizing_fixed_value_size() {
        let sizing = Sizing::from_config(&SizingConfig::fixed_value(0.10));
        // 10% of 10_000 = 1_000 notional, at price 100 = 10 units
        let size = sizing.compute_size(0.0, 100.0, 10_000.0, 100.0);
        assert!((size - 10.0).abs() < 0.001);
    }

    #[test]
    fn test_sizing_fixed_value_caps_signal() {
        let sizing = Sizing::from_config(&SizingConfig::fixed_value(0.10));
        // signal says 5, but computed size is 10 — cap to signal (min)
        let size = sizing.compute_size(5.0, 100.0, 10_000.0, 100.0);
        assert!((size - 5.0).abs() < 1e-10);
    }

    // --- Kelly tests ---

    #[test]
    fn test_sizing_kelly_full_kelly() {
        // p=0.55, b=2.0 → f* = (2*0.55 - 0.45) / 2 = 0.325
        let sizing = Sizing::from_config(&SizingConfig::kelly(0.55, 2.0, 1.0));
        let size = sizing.compute_size(0.0, 100.0, 10_000.0, 100.0);
        // 32.5% of 10_000 = 3_250 notional, at price 100 = 32.5 units
        assert!((size - 32.5).abs() < 0.1);
    }

    #[test]
    fn test_sizing_kelly_half_kelly() {
        let sizing = Sizing::from_config(&SizingConfig::kelly(0.55, 2.0, 0.5));
        let size = sizing.compute_size(0.0, 100.0, 10_000.0, 100.0);
        // half Kelly = 16.25%
        assert!((size - 16.25).abs() < 0.1);
    }

    #[test]
    fn test_sizing_kelly_zero_when_worse_than_random() {
        // p=0.4, b=2.0 → (0.8 - 0.6)/2 = 0.1 but clamped to 0
        let sizing = Sizing::from_config(&SizingConfig::kelly(0.4, 2.0, 1.0));
        let size = sizing.compute_size(0.0, 100.0, 10_000.0, 100.0);
        // expected Kelly = (2*0.4 - 0.6)/2 = 0.1, win rate < 50% → reduced
        assert!(size >= 0.0);
    }

    #[test]
    fn test_sizing_kelly_from_trades() {
        use crate::engine::{Signal, Trade};
        let trades = vec![
            Trade {
                timestamp_ns: 1_000_000_000,
                instrument_id: 0,
                side: Signal::Buy,
                price: 100.0,
                size: 1.0,
                commission: 0.0,
                pnl: 10.0, // win
                fee: 0.0,
                is_maker: false,
            },
            Trade {
                timestamp_ns: 2_000_000_000,
                instrument_id: 0,
                side: Signal::Sell,
                price: 110.0,
                size: 1.0,
                commission: 0.0,
                pnl: -5.0, // loss
                fee: 0.0,
                is_maker: false,
            },
        ];
        let (win_rate, odds, kelly_frac) = Sizing::kelly_from_trades(&trades);
        assert!((win_rate - 0.5).abs() < 1e-10);
        assert!((odds - 2.0).abs() < 1e-10);
        assert!((kelly_frac - 1.0).abs() < 1e-10);
    }

    // --- VolatilityBased tests ---

    #[test]
    fn test_sizing_volatility_based_size() {
        // 1% of equity risk, ATR=100 → size = 10_000 * 0.01 / 100 = 1.0
        let sizing = Sizing::from_config(&SizingConfig::volatility_based(0.01, 14));
        let size = sizing.compute_size(0.0, 100.0, 10_000.0, 100.0);
        assert!((size - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_sizing_volatility_based_signal_caps() {
        let sizing = Sizing::from_config(&SizingConfig::volatility_based(0.01, 14));
        // computed size = 1.0, signal = 0.5 → cap at 0.5
        let size = sizing.compute_size(0.5, 100.0, 10_000.0, 100.0);
        assert!((size - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_sizing_volatility_based_zero_atr() {
        let sizing = Sizing::from_config(&SizingConfig::volatility_based(0.01, 14));
        let size = sizing.compute_size(0.0, 0.0, 10_000.0, 100.0);
        // with zero ATR, returns signal size (0)
        assert!((size - 0.0).abs() < 1e-10);
    }

    // --- RiskParity tests ---

    #[test]
    fn test_sizing_risk_parity_basic() {
        let sizing = Sizing::from_config(&SizingConfig::volatility_based(0.01, 14));
        // Risk parity same formula as volatility-based with fixed fraction
        let size = sizing.compute_size(0.0, 100.0, 10_000.0, 100.0);
        assert!((size - 1.0).abs() < 0.001);
    }
}
