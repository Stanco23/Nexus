//! Synthetic instruments — derived from formulas of other instruments.
//!
//! # Example
//! ```ignore
//! let eth_btc = SyntheticInstrument::new(
//!     "ETHBTC",
//!     9,
//!     vec![eth_id, btc_id],
//!     "(slot0 / slot1)",
//!     1,
//!     0,
//! )?;
//! ```

use crate::instrument::InstrumentId;
use crate::instrument::expressions::{CompiledExpression, ParseExpressionError, EvalError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum SyntheticError {
    InvalidPrecision(String),
    TooFewComponents,
    FormulaParse(ParseExpressionError),
    FormulaEval(EvalError),
}

const PRICE_SCALE: f64 = 1_000_000_000.0;

/// A synthetic instrument derived from a formula of other instruments.
///
/// The synthetic price is computed on-demand from the prices of its
/// component instruments. No TVC file is needed — the price is derived
/// from the quote prices of the components.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntheticInstrument {
    pub id: InstrumentId,
    pub price_precision: u8,
    pub price_increment: i64,
    pub components: Vec<InstrumentId>,
    pub component_names: Vec<String>,
    pub formula: String,
    #[serde(skip, default)]
    compiled: CompiledExpression,
    pub ts_event: u64,
    pub ts_init: u64,
}

impl SyntheticInstrument {
    pub fn new(
        symbol: &str,
        price_precision: u8,
        components: Vec<InstrumentId>,
        formula: &str,
        ts_event: u64,
        ts_init: u64,
    ) -> Result<Self, SyntheticError> {
        if price_precision > 9 {
            return Err(SyntheticError::InvalidPrecision(format!(
                "price_precision must be <= 9, got {}",
                price_precision
            )));
        }
        if components.len() < 2 {
            return Err(SyntheticError::TooFewComponents);
        }
        let compiled = CompiledExpression::parse_formula(formula)
            .map_err(SyntheticError::FormulaParse)?;
        let price_increment = 10i64.pow((9 - price_precision) as u32);
        let id = InstrumentId::new(symbol, "SYNTH");
        let component_names = components
            .iter()
            .map(|i| i.as_str())
            .collect();
        Ok(Self {
            id,
            price_precision,
            price_increment,
            components,
            component_names,
            formula: formula.to_string(),
            compiled,
            ts_event,
            ts_init,
        })
    }

    pub fn compute_price(&self, prices: &HashMap<InstrumentId, i64>) -> Option<f64> {
        let mut inputs = Vec::with_capacity(self.components.len());
        for comp in &self.components {
            let price_raw = *prices.get(comp)? as f64 / PRICE_SCALE;
            inputs.push(price_raw);
        }
        self.compiled.eval(&inputs).ok()
    }

    pub fn price_increment_f64(&self) -> f64 {
        10f64.powi(-(self.price_precision as i32))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn synth_id(symbol: &str) -> InstrumentId {
        InstrumentId::new(symbol, "SYNTH")
    }

    #[test]
    fn test_synthetic_price_add() {
        let btc = synth_id("BTC");
        let eth = synth_id("ETH");
        let synth = SyntheticInstrument::new(
            "BTCETH",
            2,
            vec![btc.clone(), eth.clone()],
            "slot0 + slot1",
            0,
            0,
        )
        .unwrap();

        let mut prices = HashMap::new();
        prices.insert(btc, 50_000_000_000i64); // 50.0 after /1e9
        prices.insert(eth, 3_000_000_000i64);  // 3.0 after /1e9

        let price = synth.compute_price(&prices).unwrap();
        // 50.0 + 3.0 = 53.0
        assert!((price - 53.0).abs() < 1.0, "price = {}", price);
    }

    #[test]
    fn test_synthetic_price_div() {
        let eth = synth_id("ETH");
        let btc = synth_id("BTC");
        let synth = SyntheticInstrument::new(
            "ETHBTC",
            4,
            vec![eth.clone(), btc.clone()],
            "slot0 / slot1",
            0,
            0,
        )
        .unwrap();

        let mut prices = HashMap::new();
        prices.insert(eth, 3_000_000_000i64);   // 3.0 after /1e9
        prices.insert(btc, 50_000_000_000i64);  // 50.0 after /1e9

        let price = synth.compute_price(&prices).unwrap();
        // 3.0 / 50.0 = 0.06
        assert!((price - 0.06).abs() < 0.0001, "price = {}", price);
    }

    #[test]
    fn test_synthetic_with_multiplier() {
        let btc = synth_id("BTC");
        let usd = synth_id("USD");
        let synth = SyntheticInstrument::new(
            "BTCUSD",
            2,
            vec![btc.clone(), usd.clone()],
            "slot0 * 100.0",
            0,
            0,
        )
        .unwrap();

        let mut prices = HashMap::new();
        prices.insert(btc, 50_000_000_000i64); // 50.0 after /1e9
        prices.insert(usd, 1_000_000_000i64);  // 1.0 after /1e9 (unused by formula but required)

        let price = synth.compute_price(&prices).unwrap();
        // 50.0 * 100.0 = 5000.0
        assert!((price - 5000.0).abs() < 1.0, "price = {}", price);
    }

    #[test]
    fn test_synthetic_3_component_average() {
        let a = synth_id("A");
        let b = synth_id("B");
        let c = synth_id("C");
        let synth = SyntheticInstrument::new(
            "ABC_AVG",
            2,
            vec![a.clone(), b.clone(), c.clone()],
            "(slot0 + slot1 + slot2) / 3.0",
            0,
            0,
        )
        .unwrap();

        let mut prices = HashMap::new();
        prices.insert(a, 100_000_000_000i64); // 100.0 after /1e9
        prices.insert(b, 200_000_000_000i64); // 200.0 after /1e9
        prices.insert(c, 300_000_000_000i64); // 300.0 after /1e9

        let price = synth.compute_price(&prices).unwrap();
        // (100 + 200 + 300) / 3.0 = 200.0
        assert!((price - 200.0).abs() < 0.01, "price = {}", price);
    }

    #[test]
    fn test_synthetic_price_precision_and_increment() {
        let a = synth_id("A");
        let b = synth_id("B");
        let synth = SyntheticInstrument::new(
            "SYNTH_PREC4",
            4,
            vec![a.clone(), b.clone()],
            "slot0 + slot1",
            0,
            0,
        )
        .unwrap();

        assert_eq!(synth.price_precision, 4);
        assert!(
            (synth.price_increment_f64() - 0.0001).abs() < 1e-10,
            "increment = {}",
            synth.price_increment_f64()
        );
    }

    #[test]
    fn test_synthetic_invalid_formula_rejected() {
        let a = synth_id("A");
        let b = synth_id("B");
        let result = SyntheticInstrument::new("BAD", 2, vec![a.clone(), b.clone()], "A + ", 0, 0);
        assert!(result.is_err(), "Expected Err for invalid formula, got Ok");
    }

    #[test]
    fn test_synthetic_too_few_components_rejected() {
        let a = synth_id("A");
        let result = SyntheticInstrument::new("BAD", 2, vec![a.clone()], "slot0 * 2.0", 0, 0);
        assert!(
            matches!(result, Err(SyntheticError::TooFewComponents)),
            "Expected TooFewComponents, got {:?}",
            result
        );
    }

    #[test]
    fn test_synthetic_invalid_precision_rejected() {
        let a = synth_id("A");
        let b = synth_id("B");
        let result = SyntheticInstrument::new("BAD", 10, vec![a.clone(), b.clone()], "slot0 + slot1", 0, 0);
        assert!(
            matches!(result, Err(SyntheticError::InvalidPrecision(_))),
            "Expected InvalidPrecision, got {:?}",
            result
        );
    }

    #[test]
    fn test_synthetic_missing_component() {
        let a = synth_id("A");
        let b = synth_id("B");
        let synth = SyntheticInstrument::new(
            "TEST",
            2,
            vec![a.clone(), b.clone()],
            "slot0 + slot1",
            0,
            0,
        )
        .unwrap();

        let mut prices = HashMap::new();
        prices.insert(a, 100_000_000_000i64);
        // b is intentionally absent

        let price = synth.compute_price(&prices);
        assert!(price.is_none(), "Expected None when component missing, got {:?}", price);
    }

    #[test]
    fn test_synthetic_component_names() {
        let btc = synth_id("BTC");
        let eth = synth_id("ETH");
        let synth = SyntheticInstrument::new(
            "BTCETH",
            2,
            vec![btc.clone(), eth.clone()],
            "slot0 / slot1",
            0,
            0,
        )
        .unwrap();

        assert_eq!(synth.component_names.len(), 2);
        // as_str() returns "INSTR{:08X}" (hash-based, not original symbol)
        assert_eq!(synth.component_names[0], btc.as_str());
        assert_eq!(synth.component_names[1], eth.as_str());
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