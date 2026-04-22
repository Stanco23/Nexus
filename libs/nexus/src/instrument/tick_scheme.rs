//! Tick scheme — maps price to valid tick increments.
//!
//! Nautilus: `model/tick_scheme/base.pyx` + `implementations/fixed.pyx`, `implementations/tiered.pyx`

use serde::{Deserialize, Serialize};

/// A tick scheme defines valid price increments for an instrument.
/// 
/// Nautilus TickScheme provides:
/// - `price_increment()` → the minimum tick size
/// - `next_bid_price(reference, n)` → n ticks away from reference (bid side)
/// - `next_ask_price(reference, n)` → n ticks away from reference (ask side)
pub trait TickScheme: Send + Sync {
    /// Return the price increment as a floating point value.
    fn price_increment(&self) -> f64;

    /// Return the next bid price n ticks away from the reference price.
    fn next_bid_price(&self, reference_price: f64, n: i32) -> f64;

    /// Return the next ask price n ticks away from the reference price.
    fn next_ask_price(&self, reference_price: f64, n: i32) -> f64;
}

/// Fixed tick scheme — single increment for all prices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixedTickScheme {
    pub price_increment: f64,
    pub min_price: f64,
    pub max_price: f64,
}

impl FixedTickScheme {
    pub fn new(price_increment: f64, min_price: f64, max_price: f64) -> Self {
        Self {
            price_increment,
            min_price,
            max_price,
        }
    }

    fn round_to_tick(&self, price: f64, n: i32) -> f64 {
        if self.price_increment == 0.0 {
            return price;
        }
        let ticks = ((price - self.min_price) / self.price_increment).round() as i64 + n as i64;
        let adjusted = self.min_price + (ticks as f64 * self.price_increment);
        adjusted.clamp(self.min_price, self.max_price)
    }
}

impl TickScheme for FixedTickScheme {
    fn price_increment(&self) -> f64 {
        self.price_increment
    }

    fn next_bid_price(&self, reference_price: f64, n: i32) -> f64 {
        self.round_to_tick(reference_price, -n)
    }

    fn next_ask_price(&self, reference_price: f64, n: i32) -> f64 {
        self.round_to_tick(reference_price, n)
    }
}

/// A single tier in a tiered tick scheme.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickTier {
    /// Upper price bound (exclusive) for this tier.
    pub price_ceiling: f64,
    /// Tick increment for prices in this tier.
    pub tick_size: f64,
}

impl TickTier {
    pub fn new(price_ceiling: f64, tick_size: f64) -> Self {
        Self { price_ceiling, tick_size }
    }
}

/// Tiered tick scheme — different increments at different price levels.
/// 
/// Used for instruments like Indian options (NSE) where tick size varies by price bracket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TieredTickScheme {
    pub tiers: Vec<TickTier>,
    pub min_price: f64,
    pub max_price: f64,
}

impl TieredTickScheme {
    pub fn new(min_price: f64, max_price: f64) -> Self {
        Self {
            tiers: Vec::new(),
            min_price,
            max_price,
        }
    }

    pub fn add_tier(&mut self, price_ceiling: f64, tick_size: f64) {
        self.tiers.push(TickTier::new(price_ceiling, tick_size));
    }

    fn tick_size_for_price(&self, price: f64) -> f64 {
        let mut result = 0.01; // default
        for tier in &self.tiers {
            if price < tier.price_ceiling {
                result = tier.tick_size;
                break;
            }
        }
        result
    }

    fn round_to_tick(&self, price: f64, n: i32) -> f64 {
        let tick = self.tick_size_for_price(price);
        if tick == 0.0 {
            return price;
        }
        let ticks = ((price - self.min_price) / tick).round() as i64 + n as i64;
        let adjusted = self.min_price + (ticks as f64 * tick);
        adjusted.clamp(self.min_price, self.max_price)
    }
}

impl TickScheme for TieredTickScheme {
    fn price_increment(&self) -> f64 {
        self.tiers.first().map(|t| t.tick_size).unwrap_or(0.01)
    }

    fn next_bid_price(&self, reference_price: f64, n: i32) -> f64 {
        self.round_to_tick(reference_price, -n)
    }

    fn next_ask_price(&self, reference_price: f64, n: i32) -> f64 {
        self.round_to_tick(reference_price, n)
    }
}

/// Global tick scheme registry.
///
/// In Nautilus, tick schemes are registered and looked up by name.
/// We provide a simple registry here for commonly used schemes.
pub mod registry {
    use super::*;

    pub static BINANCE_DEFAULT: FixedTickScheme = FixedTickScheme {
        price_increment: 0.01,
        min_price: 0.0,
        max_price: 1_000_000.0,
    };

    pub static BINANCE_FUTURES: FixedTickScheme = FixedTickScheme {
        price_increment: 0.1,
        min_price: 0.0,
        max_price: 1_000_000.0,
    };

    pub static DEFAULT_SCHEME: FixedTickScheme = FixedTickScheme {
        price_increment: 0.01,
        min_price: 0.0,
        max_price: 1_000_000.0,
    };

    pub fn get_scheme(name: &str) -> Option<&'static dyn TickScheme> {
        match name {
            "BINANCE.DEFAULT" => Some(&BINANCE_DEFAULT as &dyn TickScheme),
            "BINANCE.FUTURES" => Some(&BINANCE_FUTURES as &dyn TickScheme),
            "DEFAULT" => Some(&DEFAULT_SCHEME as &dyn TickScheme),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fixed_tick_scheme_basic() {
        let scheme = FixedTickScheme::new(0.01, 0.0, 10000.0);
        assert_eq!(scheme.price_increment(), 0.01);
    }

    #[test]
    fn test_fixed_next_bid_ask() {
        let scheme = FixedTickScheme::new(0.01, 0.0, 10000.0);
        // Price 100.00, next bid (n=1) should be 99.99
        let bid = scheme.next_bid_price(100.00, 1);
        assert!((bid - 99.99).abs() < 1e-9, "bid was {}, expected ~99.99", bid);
        // Next ask should be 100.01
        let ask = scheme.next_ask_price(100.00, 1);
        assert!((ask - 100.01).abs() < 1e-9, "ask was {}, expected ~100.01", ask);
    }

    #[test]
    fn test_tiered_tick_scheme() {
        let mut scheme = TieredTickScheme::new(0.0, 10000.0);
        scheme.add_tier(50.0, 0.05);   // prices 0-50: tick = 0.05
        scheme.add_tier(100.0, 0.1);   // prices 50-100: tick = 0.1
        scheme.add_tier(f64::MAX, 0.5); // prices 100+: tick = 0.5

        // Price in tier 1 (0-50)
        let bid1 = scheme.next_bid_price(25.0, 1);
        assert!((bid1 - 24.95).abs() < 1e-9, "bid1 was {}", bid1);
        let ask1 = scheme.next_ask_price(25.0, 1);
        assert!((ask1 - 25.05).abs() < 1e-9, "ask1 was {}", ask1);

        // Price in tier 2 (50-100)
        let bid2 = scheme.next_bid_price(75.0, 1);
        assert!((bid2 - 74.9).abs() < 1e-9, "bid2 was {}", bid2);
        let ask2 = scheme.next_ask_price(75.0, 1);
        assert!((ask2 - 75.1).abs() < 1e-9, "ask2 was {}", ask2);
    }

    #[test]
    fn test_tiered_tick_scheme_clamp() {
        let scheme = TieredTickScheme::new(0.0, 10000.0);
        // At min boundary
        let bid = scheme.next_bid_price(0.01, 1);
        assert!((bid - 0.0).abs() < 1e-9, "bid was {}", bid);
        // At max boundary
        let ask = scheme.next_ask_price(9999.99, 1);
        assert!((ask - 10000.0).abs() < 1e-9, "ask was {}", ask);
    }
}
