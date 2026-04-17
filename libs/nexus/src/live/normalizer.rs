//! Symbol normalization for multi-exchange support.
//!
//! Converts between Nexus internal format (e.g. "BTC-USDT.BINANCE") and
//! exchange-specific formats (e.g. "BTCUSDT" for Binance, "BTC-USDT" for OKX).

#[allow(clippy::if_same_then_else)]
/// Normalizes an instrument ID to an exchange-specific symbol string.
pub trait SymbolNormalizer: Send + Sync {
    /// Convert Nexus internal format to exchange-specific symbol.
    fn to_exchange_symbol(&self, instrument_id: &str) -> String;

    /// Convert exchange-specific symbol to Nexus internal format.
    #[allow(clippy::wrong_self_convention)]
    fn from_exchange_symbol(&self, exchange_symbol: &str, venue: &str) -> String;

    /// Extract the base asset (e.g. "BTC") from an exchange symbol.
    fn base_asset(&self, exchange_symbol: &str) -> String;

    /// Extract the quote asset (e.g. "USDT") from an exchange symbol.
    fn quote_asset(&self, exchange_symbol: &str) -> String;
}

// Binance: "BTCUSDT" -> ("BTC", "USDT"), no transformation needed
pub struct BinanceSymbolNormalizer;

impl SymbolNormalizer for BinanceSymbolNormalizer {
    fn to_exchange_symbol(&self, instrument_id: &str) -> String {
        instrument_id.split('.').next().unwrap_or(instrument_id).to_uppercase()
    }

    fn from_exchange_symbol(&self, exchange_symbol: &str, venue: &str) -> String {
        format!("{}.{}", exchange_symbol.to_uppercase(), venue.to_uppercase())
    }

    #[allow(clippy::if_same_then_else)]
    fn base_asset(&self, exchange_symbol: &str) -> String {
        let s = exchange_symbol.to_uppercase();
        if s.ends_with("USDT") || s.ends_with("USDC") {
            s[..s.len() - 4].to_string()
        } else if s.ends_with("BTC") || s.ends_with("ETH") {
            s[..s.len() - 3].to_string()
        } else {
            s.clone()
        }
    }

    #[allow(clippy::if_same_then_else)]
    fn quote_asset(&self, exchange_symbol: &str) -> String {
        let s = exchange_symbol.to_uppercase();
        if s.ends_with("USDT") {
            "USDT".to_string()
        } else if s.ends_with("USDC") {
            "USDC".to_string()
        } else if s.ends_with("BTC") {
            "BTC".to_string()
        } else if s.ends_with("ETH") {
            "ETH".to_string()
        } else {
            "USDT".to_string()
        }
    }
}

// Bybit: same normalization as Binance for spot
pub struct BybitSymbolNormalizer;

impl SymbolNormalizer for BybitSymbolNormalizer {
    fn to_exchange_symbol(&self, instrument_id: &str) -> String {
        instrument_id.split('.').next().unwrap_or(instrument_id).to_uppercase()
    }

    fn from_exchange_symbol(&self, exchange_symbol: &str, venue: &str) -> String {
        format!("{}.{}", exchange_symbol.to_uppercase(), venue.to_uppercase())
    }

    #[allow(clippy::if_same_then_else)]
    fn base_asset(&self, exchange_symbol: &str) -> String {
        let s = exchange_symbol.to_uppercase();
        if s.ends_with("USDT") {
            s[..s.len() - 4].to_string()
        } else if s.ends_with("USDC") {
            s[..s.len() - 4].to_string()
        } else if s.len() >= 4 {
            s[..s.len() - 3].to_string()
        } else {
            s.clone()
        }
    }

    #[allow(clippy::if_same_then_else)]
    fn quote_asset(&self, exchange_symbol: &str) -> String {
        let s = exchange_symbol.to_uppercase();
        if s.ends_with("USDT") {
            "USDT".to_string()
        } else if s.ends_with("USDC") {
            "USDC".to_string()
        } else if s.len() >= 3 {
            s[s.len() - 3..].to_string()
        } else {
            s.clone()
        }
    }
}

// OKX: "BTC-USDT" -> base/quote split by hyphen
pub struct OkxSymbolNormalizer;

impl SymbolNormalizer for OkxSymbolNormalizer {
    fn to_exchange_symbol(&self, instrument_id: &str) -> String {
        let symbol = instrument_id.split('.').next().unwrap_or(instrument_id);
        symbol.to_uppercase()
    }

    fn from_exchange_symbol(&self, exchange_symbol: &str, venue: &str) -> String {
        format!("{}.{}", exchange_symbol.to_uppercase(), venue.to_uppercase())
    }

    #[allow(clippy::if_same_then_else)]
    fn base_asset(&self, exchange_symbol: &str) -> String {
        let parts: Vec<&str> = exchange_symbol.split('-').collect();
        parts.first().unwrap_or(&exchange_symbol).to_string()
    }

    #[allow(clippy::if_same_then_else)]
    fn quote_asset(&self, exchange_symbol: &str) -> String {
        let parts: Vec<&str> = exchange_symbol.split('-').collect();
        parts.get(1).unwrap_or(&"USDT").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_binance_roundtrip() {
        let n = BinanceSymbolNormalizer;
        assert_eq!(n.to_exchange_symbol("BTCUSDT.BINANCE"), "BTCUSDT");
        assert_eq!(n.from_exchange_symbol("BTCUSDT", "BINANCE"), "BTCUSDT.BINANCE");
        assert_eq!(n.base_asset("BTCUSDT"), "BTC");
        assert_eq!(n.quote_asset("BTCUSDT"), "USDT");
    }

    #[test]
    fn test_bybit_roundtrip() {
        let n = BybitSymbolNormalizer;
        assert_eq!(n.to_exchange_symbol("BTCUSDT.BYBIT"), "BTCUSDT");
        assert_eq!(n.from_exchange_symbol("BTCUSDT", "BYBIT"), "BTCUSDT.BYBIT");
        assert_eq!(n.base_asset("BTCUSDT"), "BTC");
        assert_eq!(n.quote_asset("BTCUSDT"), "USDT");
    }

    #[test]
    fn test_okx_roundtrip() {
        let n = OkxSymbolNormalizer;
        assert_eq!(n.to_exchange_symbol("BTC-USDT.OKX"), "BTC-USDT");
        assert_eq!(n.from_exchange_symbol("BTC-USDT", "OKX"), "BTC-USDT.OKX");
        assert_eq!(n.base_asset("BTC-USDT"), "BTC");
        assert_eq!(n.quote_asset("BTC-USDT"), "USDT");
    }

    #[test]
    fn test_okx_eth_quote() {
        let n = OkxSymbolNormalizer;
        assert_eq!(n.base_asset("ETH-USDT"), "ETH");
        assert_eq!(n.quote_asset("ETH-USDT"), "USDT");
    }
}
