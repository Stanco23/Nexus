//! Instrument enums: AssetClass, InstrumentClass, OptionKind.

use serde::{Deserialize, Serialize};

/// Asset class of an instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum AssetClass {
    FX = 1,
    CRYPTOCURRENCY = 2,
    EQUITY = 3,
    COMMODITY = 4,
    INDEX = 5,
    ALTERNATIVE = 6,
    BONDS = 7,
    MONEY = 8,
    CREDIT = 9,
}

impl AssetClass {
    pub fn is_crypto(&self) -> bool {
        matches!(self, AssetClass::CRYPTOCURRENCY)
    }

    pub fn is_fx(&self) -> bool {
        matches!(self, AssetClass::FX)
    }

    pub fn is_equity(&self) -> bool {
        matches!(self, AssetClass::EQUITY)
    }
}

/// Instrument class (trading category).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
#[allow(non_camel_case_types)]
pub enum InstrumentClass {
    SPOT = 1,
    SWAP = 2,
    FUTURE = 3,
    FuturesSpread = 4,
    FORWARD = 5,
    CFD = 6,
    BOND = 7,
    OPTION = 8,
    OPTION_SPREAD = 9,
    WARRANT = 10,
    SPORTS_BETTING = 11,
    BINARY_OPTION = 12,
}

impl InstrumentClass {
    /// Whether this instrument class expires (FUTURE, OPTION, etc.).
    pub fn is_expiring(&self) -> bool {
        matches!(
            self,
            InstrumentClass::FUTURE
                | InstrumentClass::FuturesSpread
                | InstrumentClass::OPTION
                | InstrumentClass::OPTION_SPREAD
        )
    }

    /// Whether this instrument class allows negative prices.
    pub fn allows_negative_price(&self) -> bool {
        matches!(
            self,
            InstrumentClass::OPTION
                | InstrumentClass::OPTION_SPREAD
                | InstrumentClass::FuturesSpread
        )
    }

    pub fn is_spread(&self) -> bool {
        matches!(
            self,
            InstrumentClass::FuturesSpread | InstrumentClass::OPTION_SPREAD
        )
    }
}

/// Option type: Put or Call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum OptionKind {
    PUT = 1,
    CALL = 2,
}

/// Exercise style for options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
#[allow(non_camel_case_types)]
pub enum ExerciseStyle {
    EUROPEAN = 1,
    AMERICAN = 2,
}

/// Settlement type for derivatives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[repr(u8)]
#[allow(non_camel_case_types)]
pub enum SettlementType {
    #[default]
    CASH = 1,
    PHYSICAL = 2,
    BY_CLAIM = 3,
}

impl std::str::FromStr for AssetClass {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "FX" | "FOREX" => Ok(AssetClass::FX),
            "CRYPTO" | "CRYPTOCURRENCY" => Ok(AssetClass::CRYPTOCURRENCY),
            "EQUITY" | "STOCK" => Ok(AssetClass::EQUITY),
            "COMMODITY" => Ok(AssetClass::COMMODITY),
            "INDEX" => Ok(AssetClass::INDEX),
            "ALTERNATIVE" => Ok(AssetClass::ALTERNATIVE),
            "BONDS" | "BOND" => Ok(AssetClass::BONDS),
            "MONEY" | "CASH" => Ok(AssetClass::MONEY),
            "CREDIT" => Ok(AssetClass::CREDIT),
            _ => Err(format!("Unknown asset class: {}", s)),
        }
    }
}

/// Create an InstrumentClass from a string.
impl std::str::FromStr for InstrumentClass {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "SPOT" | "CURRENCY_PAIR" | "EQUITY" | "COMMODITY" => Ok(InstrumentClass::SPOT),
            "SWAP" | "PERPETUAL" | "CRYPTO_PERPETUAL" => Ok(InstrumentClass::SWAP),
            "FUTURE" | "FUTURES_CONTRACT" | "CRYPTO_FUTURE" => Ok(InstrumentClass::FUTURE),
            "FUTURES_SPREAD" | "SPREAD" => Ok(InstrumentClass::FuturesSpread),
            "FORWARD" => Ok(InstrumentClass::FORWARD),
            "CFD" => Ok(InstrumentClass::CFD),
            "BOND" => Ok(InstrumentClass::BOND),
            "OPTION" | "OPTION_CONTRACT" | "CRYPTO_OPTION" => Ok(InstrumentClass::OPTION),
            "OPTION_SPREAD" => Ok(InstrumentClass::OPTION_SPREAD),
            "WARRANT" => Ok(InstrumentClass::WARRANT),
            "SPORTS_BETTING" | "BETTING" => Ok(InstrumentClass::SPORTS_BETTING),
            "BINARY_OPTION" => Ok(InstrumentClass::BINARY_OPTION),
            _ => Err(format!("Unknown instrument class: {}", s)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_asset_class_is_crypto() {
        assert!(AssetClass::CRYPTOCURRENCY.is_crypto());
        assert!(!AssetClass::FX.is_crypto());
        assert!(!AssetClass::EQUITY.is_crypto());
    }

    #[test]
    fn test_instrument_class_expiring() {
        assert!(InstrumentClass::FUTURE.is_expiring());
        assert!(InstrumentClass::OPTION.is_expiring());
        assert!(!InstrumentClass::SPOT.is_expiring());
        assert!(!InstrumentClass::SWAP.is_expiring());
    }

    #[test]
    fn test_instrument_class_negative_price() {
        assert!(InstrumentClass::OPTION.allows_negative_price());
        assert!(InstrumentClass::OPTION_SPREAD.allows_negative_price());
        assert!(InstrumentClass::FuturesSpread.allows_negative_price());
        assert!(!InstrumentClass::SPOT.allows_negative_price());
        assert!(!InstrumentClass::FUTURE.allows_negative_price());
    }

    #[test]
    fn test_parse_asset_class() {
        assert_eq!(
            "CRYPTO".parse::<AssetClass>().unwrap(),
            AssetClass::CRYPTOCURRENCY
        );
        assert_eq!("fx".parse::<AssetClass>().unwrap(), AssetClass::FX);
        assert!("INVALID".parse::<AssetClass>().is_err());
    }

    #[test]
    fn test_parse_instrument_class() {
        assert_eq!(
            "SPOT".parse::<InstrumentClass>().unwrap(),
            InstrumentClass::SPOT
        );
        assert_eq!(
            "PERPETUAL".parse::<InstrumentClass>().unwrap(),
            InstrumentClass::SWAP
        );
        assert_eq!(
            "FUTURE".parse::<InstrumentClass>().unwrap(),
            InstrumentClass::FUTURE
        );
    }
}
