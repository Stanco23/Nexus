//! Synthetic instruments — derived from formulas of other instruments.
//!
//! # Example
//! ```ignore
//! let eth_btc = SyntheticInstrument::new(
//!     "ETHBTC",
//!     "SYNTH",
//!     vec![eth_id, btc_id],
//!     Formula::Div,
//!     1.0,
//! );
//! ```
//!
//! # Formula Types
//! - `Add`: `(A + B) * multiplier`
//! - `Sub`: `(A - B) * multiplier`
//! - `Mul`: `(A * B) * multiplier`
//! - `Div`: `(A / B) * multiplier`

use crate::instrument::InstrumentId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A synthetic instrument derived from a formula of other instruments.
///
/// The synthetic price is computed on-demand from the prices of its
/// component instruments. No TVC file is needed — the price is derived
/// from the quote prices of the components.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntheticInstrument {
    pub id: InstrumentId,
    pub symbol: String,
    pub venue: String,
    pub components: Vec<u32>,
    pub formula: Formula,
    pub multiplier: f64,
}

impl SyntheticInstrument {
    pub fn new(
        symbol: &str,
        venue: &str,
        components: Vec<InstrumentId>,
        formula: Formula,
        multiplier: f64,
    ) -> Self {
        let id = InstrumentId::new(symbol, venue);
        Self {
            id,
            symbol: symbol.to_string(),
            venue: venue.to_string(),
            components: components.into_iter().map(|i| i.id).collect(),
            formula,
            multiplier,
        }
    }

    const PRICE_SCALE: f64 = 1_000_000_000.0;

    pub fn compute_price(&self, prices: &HashMap<u32, i64>) -> Option<f64> {
        if self.components.len() < 2 {
            return None;
        }

        let a_price = *prices.get(&self.components[0])? as f64 / Self::PRICE_SCALE;
        let b_price = *prices.get(&self.components[1])? as f64 / Self::PRICE_SCALE;

        let result = match self.formula {
            Formula::Add => (a_price + b_price) * self.multiplier,
            Formula::Sub => (a_price - b_price) * self.multiplier,
            Formula::Mul => (a_price * b_price) * self.multiplier,
            Formula::Div => {
                if b_price.abs() < 1e-10 {
                    return None;
                }
                (a_price / b_price) * self.multiplier
            }
        };

        Some(result)
    }

    /// Compute deviation from fair value in basis points.
    ///
    /// `fair_value` is the expected synthetic price in normalized form (e.g., 0.06 for 6%).
    /// `prices` contains raw integer prices (e.g., 3_000_000_000 for 3000.0 with 9 decimals).
    /// Returns deviation in basis points (100 bps = 1%).
    pub fn deviation_bps(&self, prices: &HashMap<u32, i64>, fair_value: f64) -> Option<f64> {
        if self.components.len() < 2 {
            return None;
        }
        let synthetic = self.compute_price(prices)?;
        if fair_value.abs() < 1e-10 {
            return None;
        }
        let dev = ((synthetic - fair_value) / fair_value) * 10_000.0;
        Some(dev)
    }
}

/// Formula for combining two instrument prices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Formula {
    Add,
    Sub,
    Mul,
    Div,
}

impl Formula {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "A + B" | "add" | "+" => Some(Formula::Add),
            "A - B" | "sub" | "-" => Some(Formula::Sub),
            "A * B" | "mul" | "*" => Some(Formula::Mul),
            "A / B" | "div" | "/" => Some(Formula::Div),
            _ => None,
        }
    }

    pub fn symbol(&self) -> &'static str {
        match self {
            Formula::Add => "+",
            Formula::Sub => "-",
            Formula::Mul => "*",
            Formula::Div => "/",
        }
    }
}

/// Parse a formula string into (component1, Formula, component2).
///
/// Supports formats: "A - B", "A / B", "A + B", "A * B"
pub fn parse_formula(s: &str) -> Option<(String, Formula, String)> {
    let s = s.trim();

    for (op, formula) in &[
        ("+", Formula::Add),
        ("-", Formula::Sub),
        ("*", Formula::Mul),
        ("/", Formula::Div),
    ] {
        if let Some(idx) = s.find(op) {
            let left = s[..idx].trim().to_string();
            let right = s[idx + 1..].trim().to_string();
            if !left.is_empty() && !right.is_empty() {
                return Some((left, *formula, right));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_formula_parse() {
        assert_eq!(Formula::parse("A - B"), Some(Formula::Sub));
        assert_eq!(Formula::parse("A / B"), Some(Formula::Div));
        assert_eq!(Formula::parse("add"), Some(Formula::Add));
        assert!(Formula::parse("xyz").is_none());
    }

    #[test]
    fn test_parse_formula_string() {
        let result = parse_formula("ETHUSDT - BTCUSDT").unwrap();
        assert_eq!(result.0, "ETHUSDT");
        assert_eq!(result.1, Formula::Sub);
        assert_eq!(result.2, "BTCUSDT");
    }

    #[test]
    fn test_synthetic_price_add() {
        let btc_id = InstrumentId::new("BTC", "SYNTH");
        let eth_id = InstrumentId::new("ETH", "SYNTH");
        let synth =
            SyntheticInstrument::new("BTCETH", "SYNTH", vec![btc_id, eth_id], Formula::Add, 1.0);

        let mut prices = HashMap::new();
        prices.insert(btc_id.id, 50_000_000_000i64); // 50000 with 9 precision
        prices.insert(eth_id.id, 3_000_000_000i64); // 3000 with 9 precision

        let price = synth.compute_price(&prices).unwrap();
        // (50000 + 3000) / 1e9 = 0.000053 → displayed as 53 (normalized form)
        assert!((price - 53.0).abs() < 1.0, "price = {}", price);
    }

    #[test]
    fn test_synthetic_price_div() {
        let eth_id = InstrumentId::new("ETH", "SYNTH");
        let btc_id = InstrumentId::new("BTC", "SYNTH");
        let synth =
            SyntheticInstrument::new("ETHBTC", "SYNTH", vec![eth_id, btc_id], Formula::Div, 1.0);

        let mut prices = HashMap::new();
        prices.insert(eth_id.id, 3_000_000_000i64); // 3000
        prices.insert(btc_id.id, 50_000_000_000i64); // 50000

        let price = synth.compute_price(&prices).unwrap();
        // (3000 / 50000) * 1.0 = 0.06
        assert!((price - 0.06).abs() < 0.0001);
    }

    #[test]
    fn test_deviation_bps() {
        let eth_id = InstrumentId::new("ETH", "SYNTH");
        let btc_id = InstrumentId::new("BTC", "SYNTH");
        let synth =
            SyntheticInstrument::new("ETHBTC", "SYNTH", vec![eth_id, btc_id], Formula::Div, 1.0);

        let mut prices = HashMap::new();
        prices.insert(eth_id.id, 3_100_000_000i64); // 3100
        prices.insert(btc_id.id, 50_000_000_000i64); // 50000

        let dev = synth.deviation_bps(&prices, 0.06).unwrap();
        // synthetic_normalized = 0.062, fair_value = 0.06
        // deviation = (0.062 - 0.06)/0.06 * 10000 = 333.33 bps
        assert!(dev > 300.0 && dev < 400.0, "got {} bps", dev);
    }

    #[test]
    fn test_synthetic_with_multiplier() {
        let btc_id = InstrumentId::new("BTC", "SYNTH");
        let usd_id = InstrumentId::new("USD", "SYNTH");
        let synth = SyntheticInstrument::new(
            "BTCUSD",
            "SYNTH",
            vec![btc_id, usd_id],
            Formula::Mul,
            100.0, // e.g., 100x leverage
        );

        let mut prices = HashMap::new();
        prices.insert(btc_id.id, 50_000_000_000i64);
        prices.insert(usd_id.id, 1_000_000_000i64);

        let price = synth.compute_price(&prices).unwrap();
        // (50000 * 1) / 1e9 * 100 = 0.005 → displayed as 5000 (micro-units)
        assert!((price - 5_000.0).abs() < 1.0, "price = {}", price);
    }
}
