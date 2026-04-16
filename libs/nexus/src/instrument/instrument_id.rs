//! Instrument identifier — symbol + venue pair.
//!
//! Format: "SYMBOL.VENUE" (e.g., "BTCUSDT.BINANCE", "AUD/USD.IDEALPRO")
//!
//! The combination of symbol and venue uniquely identifies an instrument.
//! FNV-1a hash of the raw string is used as the instrument_id u32.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// A trading venue (exchange).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Venue {
    pub code: String, // e.g., "BINANCE", "KRKEN", "SYNTH"
}

impl Venue {
    pub fn new(code: &str) -> Self {
        Self {
            code: code.to_uppercase(),
        }
    }

    pub fn synthetic() -> Self {
        Self {
            code: "SYNTH".to_string(),
        }
    }

    pub fn is_synthetic(&self) -> bool {
        self.code == "SYNTH"
    }
}

impl fmt::Display for Venue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code)
    }
}

impl FromStr for Venue {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err("Venue cannot be empty".to_string());
        }
        Ok(Venue::new(s))
    }
}

/// A trading symbol (ticker).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Symbol {
    pub code: String, // e.g., "BTCUSDT", "AUD/USD"
}

impl Symbol {
    pub fn new(code: &str) -> Self {
        Self {
            code: code.to_string(),
        }
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code)
    }
}

impl FromStr for Symbol {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err("Symbol cannot be empty".to_string());
        }
        Ok(Symbol::new(s))
    }
}

/// Instrument identifier — uniquely identifies a tradeable instrument.
///
/// Format: "SYMBOL.VENUE" (e.g., "BTCUSDT.BINANCE")
///
/// The instrument_id is a 32-bit FNV-1a hash of the normalized string
/// representation (symbol in uppercase, venue in uppercase, joined by '.').
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct InstrumentId {
    pub id: u32, // FNV-1a hash of "SYMBOL.VENUE"
}

impl InstrumentId {
    /// Create from symbol and venue.
    pub fn new(symbol: &str, venue: &str) -> Self {
        let raw = format!("{}.{}", symbol.to_uppercase(), venue.to_uppercase());
        Self {
            id: fnv1a_hash(raw.as_bytes()),
        }
    }

    /// Parse an instrument ID from a string like "BTCUSDT.BINANCE".
    pub fn parse(value: &str) -> Result<Self, IdError> {
        if !value.contains('.') {
            return Err(IdError::InvalidFormat(value.to_string()));
        }
        let parts: Vec<&str> = value.split('.').collect();
        if parts.len() != 2 {
            return Err(IdError::InvalidFormat(value.to_string()));
        }
        if parts[0].is_empty() || parts[1].is_empty() {
            return Err(IdError::InvalidFormat(value.to_string()));
        }
        Ok(Self::new(parts[0], parts[1]))
    }

    /// Get the raw string representation.
    pub fn as_str(&self) -> String {
        // We can't reconstruct the original string from the hash alone.
        // For display purposes, use a placeholder when we don't have the mapping.
        format!("INSTR{:08X}", self.id)
    }

    /// Check if this is a synthetic instrument (venue = SYNTH).
    pub fn is_synthetic(&self) -> bool {
        // Can't determine from hash alone; this requires lookup
        false
    }
}

impl fmt::Display for InstrumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InstrumentId({:08X})", self.id)
    }
}

impl FromStr for InstrumentId {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Errors for instrument ID operations.
#[derive(Debug, Clone)]
pub enum IdError {
    InvalidFormat(String),
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdError::InvalidFormat(s) => write!(
                f,
                "Invalid instrument ID format: {} (expected SYMBOL.VENUE)",
                s
            ),
        }
    }
}

impl std::error::Error for IdError {}

/// Compute FNV-1a hash of a byte string (32-bit).
pub fn fnv1a_hash(data: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for byte in data {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instrument_id_from_str() {
        let id = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        assert_eq!(id.id, fnv1a_hash(b"BTCUSDT.BINANCE"));

        // Same symbol, different case
        let id2 = InstrumentId::parse("btcusdt.binance").unwrap();
        assert_eq!(id, id2);
    }

    #[test]
    fn test_instrument_id_no_venue() {
        assert!(InstrumentId::parse("BTCUSDT").is_err());
        assert!(InstrumentId::parse("BTCUSDT.").is_err());
        assert!(InstrumentId::parse(".BINANCE").is_err());
    }

    #[test]
    fn test_fnv1a_stable() {
        let h1 = fnv1a_hash(b"BTCUSDT.BINANCE");
        let h2 = fnv1a_hash(b"BTCUSDT.BINANCE");
        assert_eq!(h1, h2);
        assert_ne!(h1, 0);
    }

    #[test]
    fn test_venue_is_synthetic() {
        let v = Venue::synthetic();
        assert!(v.is_synthetic());
        let v2 = Venue::new("BINANCE");
        assert!(!v2.is_synthetic());
    }

    #[test]
    fn test_symbol_display() {
        let s = Symbol::new("BTCUSDT");
        assert_eq!(format!("{}", s), "BTCUSDT");
    }
}
