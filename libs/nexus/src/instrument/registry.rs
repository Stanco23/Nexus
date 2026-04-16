//! Instrument registry — lookup and validation of instruments.
//!
//! Provides a central registry for all instruments with fast lookup
//! by instrument_id and validation of instrument compatibility.

use crate::instrument::instrument_id::fnv1a_hash;
use crate::instrument::Instrument;
use std::collections::HashMap;

/// Thread-safe instrument registry.
///
/// Stores instruments by instrument_id and provides lookup methods.
pub struct InstrumentRegistry {
    instruments: HashMap<u32, Instrument>,
}

impl InstrumentRegistry {
    pub fn new() -> Self {
        Self {
            instruments: HashMap::new(),
        }
    }

    /// Register an instrument.
    pub fn register(&mut self, instrument: Instrument) {
        self.instruments.insert(instrument.id, instrument);
    }

    /// Get an instrument by id.
    pub fn get(&self, instrument_id: u32) -> Option<&Instrument> {
        self.instruments.get(&instrument_id)
    }

    /// Get an instrument by symbol and venue.
    pub fn get_by_symbol(&self, symbol: &str, venue: &str) -> Option<&Instrument> {
        let raw = format!("{}.{}", symbol.to_uppercase(), venue.to_uppercase());
        let id = fnv1a_hash(raw.as_bytes());
        self.get(id)
    }

    /// Get all instruments of a specific class.
    pub fn by_class(&self, class: crate::instrument::enums::InstrumentClass) -> Vec<&Instrument> {
        self.instruments
            .values()
            .filter(|i| i.class == class)
            .collect()
    }

    /// Get all instruments of a specific asset class.
    pub fn by_asset_class(&self, asset: crate::instrument::enums::AssetClass) -> Vec<&Instrument> {
        self.instruments
            .values()
            .filter(|i| i.asset_class == asset)
            .collect()
    }

    /// Number of instruments in the registry.
    pub fn len(&self) -> usize {
        self.instruments.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.instruments.is_empty()
    }

    /// Get all instruments.
    pub fn all(&self) -> Vec<&Instrument> {
        self.instruments.values().collect()
    }

    /// Check if an instrument with the given id exists.
    pub fn contains(&self, instrument_id: u32) -> bool {
        self.instruments.contains_key(&instrument_id)
    }
}

impl Default for InstrumentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::enums::{AssetClass, InstrumentClass};
    use crate::instrument::types::InstrumentBuilder;

    #[test]
    fn test_registry_crud() {
        let mut registry = InstrumentRegistry::new();

        let btc = InstrumentBuilder::new(
            "BTCUSDT",
            "BINANCE",
            InstrumentClass::SPOT,
            AssetClass::CRYPTOCURRENCY,
            "USDT",
        )
        .price_precision(2)
        .build();
        let btc_id = btc.id;

        registry.register(btc);

        assert_eq!(registry.len(), 1);
        assert!(registry.contains(btc_id));

        let found = registry.get(btc_id);
        assert!(found.is_some());
        assert_eq!(found.unwrap().symbol, "BTCUSDT");

        // By symbol
        let found2 = registry.get_by_symbol("BTCUSDT", "BINANCE");
        assert!(found2.is_some());

        // Case insensitive
        let found3 = registry.get_by_symbol("btcusdt", "binance");
        assert!(found3.is_some());
    }

    #[test]
    fn test_registry_by_class() {
        let mut registry = InstrumentRegistry::new();

        let btc = InstrumentBuilder::new(
            "BTCUSDT",
            "BINANCE",
            InstrumentClass::SPOT,
            AssetClass::CRYPTOCURRENCY,
            "USDT",
        )
        .build();
        let eth = InstrumentBuilder::new(
            "ETHUSDT",
            "BINANCE",
            InstrumentClass::SPOT,
            AssetClass::CRYPTOCURRENCY,
            "USDT",
        )
        .build();
        let btc_perp = InstrumentBuilder::new(
            "BTCUSDT",
            "BYBIT",
            InstrumentClass::SWAP,
            AssetClass::CRYPTOCURRENCY,
            "USDT",
        )
        .build();

        registry.register(btc);
        registry.register(eth);
        registry.register(btc_perp);

        let spots = registry.by_class(InstrumentClass::SPOT);
        assert_eq!(spots.len(), 2);

        let swaps = registry.by_class(InstrumentClass::SWAP);
        assert_eq!(swaps.len(), 1);
    }

    #[test]
    fn test_registry_all() {
        let mut registry = InstrumentRegistry::new();
        let a =
            InstrumentBuilder::new("A", "X", InstrumentClass::SPOT, AssetClass::FX, "USD").build();
        registry.register(a);

        let all = registry.all();
        assert_eq!(all.len(), 1);
    }
}
