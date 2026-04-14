//! VPIN slippage model — realistic fill simulation using Volume-synchronized Probability of Informed Trading.
//!
//! # VPIN Formula
//! VPIN = |cum_buy - cum_sell| / (cum_buy + cum_sell)
//! VPIN is already computed in TickBuffer per bucket.
//!
//! # Slippage Model
//! - `compute_fill_delay()`: delay based on adverse selection probability
//! - `compute_impact_bps()`: price impact based on order size and VPIN
//! - Combined: `fill_price = market_price_at(timestamp + delay) * (1 + impact_bps / 10000)`
//!
//! # Constants
//! - Adverse probability: `vpin * 0.5`
//! - Delay ticks: `ceil(order_size * adverse_prob * 1.5)`
//! - Delay cap: 200ms
//! - Size impact: `min(sqrt(order_size)/100, 10.0)` bps
//! - VPIN impact: `min(vpin * 5.0, 5.0)` bps
//! - Total impact cap: 15 bps

use std::f64;

#[derive(Debug, Clone, Copy)]
pub struct SlippageConfig {
    pub delay_multiplier: f64,
    pub delay_cap_ns: u64,
    pub size_impact_max: f64,
    pub vpin_impact_max: f64,
    pub total_impact_max: f64,
}

impl Default for SlippageConfig {
    fn default() -> Self {
        Self {
            delay_multiplier: 1.5,
            delay_cap_ns: 200_000_000,
            size_impact_max: 10.0,
            vpin_impact_max: 5.0,
            total_impact_max: 15.0,
        }
    }
}

impl SlippageConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn compute_fill_delay(
        &self,
        order_size_ticks: f64,
        vpin: f64,
        avg_tick_duration_ns: u64,
    ) -> u64 {
        let adverse_prob = vpin * 0.5;
        let delay_ticks = (order_size_ticks * adverse_prob * self.delay_multiplier).ceil();
        let delay_ns = (delay_ticks as u64) * avg_tick_duration_ns;
        delay_ns.min(self.delay_cap_ns)
    }

    pub fn compute_impact_bps(&self, order_size_ticks: f64, vpin: f64) -> f64 {
        let size_impact = (order_size_ticks.sqrt() / 100.0).min(self.size_impact_max);
        let vpin_impact = (vpin * 5.0).min(self.vpin_impact_max);
        (size_impact + vpin_impact).min(self.total_impact_max)
    }
}

/// Compute fill price with VPIN slippage.
///
/// `order_size_ticks` — size in ticks
/// `vpin` — Volume-synchronized Probability of Informed Trading (0.0 to 1.0)
/// `avg_tick_duration_ns` — average time between ticks in nanoseconds
/// `market_price` — current market price
/// Returns `(fill_price, delay_ns, impact_bps)`
pub fn compute_fill_price(
    order_size_ticks: f64,
    vpin: f64,
    avg_tick_duration_ns: u64,
    market_price: f64,
) -> (f64, u64, f64) {
    let config = SlippageConfig::new();
    let delay_ns = config.compute_fill_delay(order_size_ticks, vpin, avg_tick_duration_ns);
    let impact_bps = config.compute_impact_bps(order_size_ticks, vpin);
    let fill_price = market_price * (1.0 + impact_bps / 10_000.0);
    (fill_price, delay_ns, impact_bps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vpin_impact_at_zero() {
        let config = SlippageConfig::new();
        let impact = config.compute_impact_bps(100.0, 0.0);
        let size_impact = (100.0f64.sqrt() / 100.0).min(10.0);
        assert!((impact - size_impact).abs() < 0.001);
    }

    #[test]
    fn test_vpin_impact_max() {
        let config = SlippageConfig::new();
        let impact = config.compute_impact_bps(100.0, 1.0);
        let size_impact = (100.0f64.sqrt() / 100.0).min(10.0);
        let vpin_impact = 5.0_f64.min(5.0);
        let expected = (size_impact + vpin_impact).min(15.0);
        assert!((impact - expected).abs() < 0.001);
    }

    #[test]
    fn test_size_impact_cap() {
        let config = SlippageConfig::new();
        let impact = config.compute_impact_bps(1_000_000.0, 0.0);
        assert!(
            (impact - 10.0).abs() < 0.001,
            "size impact should cap at 10 bps"
        );
    }

    #[test]
    fn test_total_impact_cap() {
        let config = SlippageConfig::new();
        let impact = config.compute_impact_bps(1_000_000.0, 1.0);
        assert!(
            (impact - 15.0).abs() < 0.001,
            "total impact should cap at 15 bps"
        );
    }

    #[test]
    fn test_fill_delay_low_vpin() {
        let config = SlippageConfig::new();
        let delay = config.compute_fill_delay(100.0, 0.1, 1_000_000);
        let adverse_prob: f64 = 0.1 * 0.5;
        let expected_ticks = (100.0 * adverse_prob * 1.5).ceil() as u64;
        let expected = (expected_ticks * 1_000_000).min(200_000_000);
        assert_eq!(delay, expected);
    }

    #[test]
    fn test_fill_delay_high_vpin() {
        let config = SlippageConfig::new();
        let delay = config.compute_fill_delay(100.0, 1.0, 1_000_000);
        let adverse_prob: f64 = 1.0 * 0.5;
        let expected_ticks = (100.0 * adverse_prob * 1.5).ceil() as u64;
        let expected = (expected_ticks * 1_000_000).min(200_000_000);
        assert_eq!(delay, expected);
    }

    #[test]
    fn test_fill_delay_cap() {
        let config = SlippageConfig::new();
        let delay = config.compute_fill_delay(1_000_000.0, 1.0, 1_000_000);
        assert_eq!(delay, 200_000_000, "delay should cap at 200ms");
    }

    #[test]
    fn test_compute_fill_price_increases() {
        let (fill_price, delay, impact) = compute_fill_price(100.0, 1.0, 1_000_000, 100.0);
        assert!(
            fill_price > 100.0,
            "fill_price={} should be > 100",
            fill_price
        );
        assert!(impact > 0.0);
        assert!(delay > 0);
    }

    #[test]
    fn test_exit_criteria_high_vpin_fill_above_market() {
        let (fill_price, _, impact_bps) = compute_fill_price(100.0, 0.8, 1_000_000, 100.0);
        let expected_impact = impact_bps;
        let expected_fill = 100.0 * (1.0 + expected_impact / 10_000.0);
        assert!(
            expected_fill > 100.001,
            "high VPIN fill should be > 0.1bps above market (100.001), got {}",
            expected_fill
        );
        assert!((fill_price - expected_fill).abs() < 0.001);
    }
}
