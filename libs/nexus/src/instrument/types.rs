//! Instrument type definitions — all 14 instrument types.
//!
//! Each instrument type is a variant of the `Instrument` enum with
//! type-specific fields. Common fields live directly on the enum.

use crate::instrument::enums::{
    AssetClass, ExerciseStyle, InstrumentClass, OptionKind, SettlementType,
};
use crate::instrument::instrument_id::fnv1a_hash;
use serde::{Deserialize, Serialize};

/// Base instrument with all common fields.
///
/// # Fields
/// - `id`: Instrument identifier (symbol + venue)
/// - `class`: Instrument class (SPOT, FUTURE, OPTION, etc.)
/// - `asset_class`: Asset class (FX, CRYPTO, EQUITY, etc.)
/// - `quote_currency`: Settlement/quote currency code (e.g., "USDT", "USD")
/// - `price_precision`: Decimal places for price
/// - `size_precision`: Decimal places for quantity
/// - `price_increment`: Minimum price change (tick_size)
/// - `size_increment`: Minimum quantity change
/// - `multiplier`: Contract value multiplier
/// - `lot_size`: Minimum lot size (0 = no minimum)
/// - `max_quantity`: Maximum order quantity
/// - `min_quantity`: Minimum order quantity
/// - `max_notional`: Maximum notional value per order
/// - `min_notional`: Minimum notional value per order
/// - `margin_init`: Initial margin requirement (0-1, e.g., 0.1 = 10%)
/// - `margin_maint`: Maintenance margin requirement
/// - `maker_fee`: Maker fee rate
/// - `taker_fee`: Taker fee rate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instrument {
    pub id: u32,
    pub symbol: String,
    pub venue: String,
    pub class: InstrumentClass,
    pub asset_class: AssetClass,
    pub quote_currency: String,
    pub is_inverse: bool,
    pub price_precision: u8,
    pub size_precision: u8,
    pub price_increment: i64,
    pub size_increment: i64,
    pub multiplier: i64,
    pub lot_size: i64,
    pub max_quantity: Option<i64>,
    pub min_quantity: Option<i64>,
    pub max_notional: Option<i64>,
    pub min_notional: Option<i64>,
    pub max_price: Option<i64>,
    pub min_price: Option<i64>,
    pub margin_init: f64,
    pub margin_maint: f64,
    pub maker_fee: f64,
    pub taker_fee: f64,
    pub kind: InstrumentKind,
    /// Timestamp of the last update event (nanoseconds).
    pub ts_event: u64,
    /// Timestamp of initialization (nanoseconds).
    pub ts_init: u64,
    /// Name of the tick scheme for this instrument (e.g., "BINANCE.DEFAULT").
    /// When set, price increments use the scheme's tiered lookup.
    pub scheme_name: Option<String>,
}

impl Instrument {
    pub fn new(
        symbol: &str,
        venue: &str,
        class: InstrumentClass,
        asset_class: AssetClass,
        quote_currency: &str,
        kind: InstrumentKind,
        ts_event: u64,
        ts_init: u64,
    ) -> Self {
        let raw = format!("{}.{}", symbol.to_uppercase(), venue.to_uppercase());
        let id = fnv1a_hash(raw.as_bytes());

        Self {
            id,
            symbol: symbol.to_string(),
            venue: venue.to_string(),
            class,
            asset_class,
            quote_currency: quote_currency.to_string(),
            is_inverse: false,
            price_precision: 2,
            size_precision: 6,
            price_increment: 1,
            size_increment: 1,
            multiplier: 1,
            lot_size: 0,
            max_quantity: None,
            min_quantity: None,
            max_notional: None,
            min_notional: None,
            max_price: None,
            min_price: None,
            margin_init: 0.0,
            margin_maint: 0.0,
            maker_fee: 0.0,
            taker_fee: 0.0,
            kind,
            ts_event,
            ts_init,
            scheme_name: None,
        }
    }

    /// Check if this instrument expires.
    pub fn is_expiring(&self) -> bool {
        self.class.is_expiring()
    }

    /// Check if this instrument allows negative prices.
    pub fn allows_negative_price(&self) -> bool {
        self.class.allows_negative_price()
    }

    /// Get the minimum price increment as a floating point value.
    pub fn price_increment_f64(&self) -> f64 {
        self.price_increment as f64 / 10f64.powi(self.price_precision as i32)
    }

    /// Get the minimum size increment as floating point.
    pub fn size_increment_f64(&self) -> f64 {
        self.size_increment as f64 / 10f64.powi(self.size_precision as i32)
    }

    /// Calculate the notional value (quantity * price).
    pub fn notional_value(&self, quantity: f64, price: f64) -> f64 {
        quantity * price
    }

    /// Calculate base quantity from notional value and price.
    pub fn calculate_base_quantity(&self, notional: f64, price: f64) -> f64 {
        if price == 0.0 {
            return 0.0;
        }
        notional / price
    }

    /// Get the settlement/quote currency.
    pub fn get_settlement_currency(&self) -> &str {
        &self.quote_currency
    }

    /// Get the next bid price using the instrument's price increment.
    pub fn next_bid_price(&self, reference_price: f64, n: i32) -> f64 {
        let tick = self.price_increment_f64();
        if tick == 0.0 {
            return reference_price;
        }
        let ticks = (reference_price / tick).round() as i64 - n as i64;
        ticks as f64 * tick
    }

    /// Get the next ask price using the instrument's price increment.
    pub fn next_ask_price(&self, reference_price: f64, n: i32) -> f64 {
        let tick = self.price_increment_f64();
        if tick == 0.0 {
            return reference_price;
        }
        let ticks = (reference_price / tick).round() as i64 + n as i64;
        ticks as f64 * tick
    }

    /// Get the tick scheme for this instrument, if registered.
    pub fn get_tick_scheme(&self) -> Option<&'static dyn crate::instrument::TickScheme> {
        self.scheme_name.as_deref()?;
        crate::instrument::tick_scheme::registry::get_scheme(self.scheme_name.as_deref()?)
    }
}

/// Instrument-specific details by type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InstrumentKind {
    Spot(SpotDetails),
    Swap(SwapDetails),
    Future(FutureDetails),
    FuturesSpread(FuturesSpreadDetails),
    Forward(ForwardDetails),
    Cfd(CfdDetails),
    Bond(BondDetails),
    Option(OptionDetails),
    OptionSpread(OptionSpreadDetails),
    Warrant(WarrantDetails),
    SportsBetting(SportsBettingDetails),
    BinaryOption(BinaryOptionDetails),
    Index(IndexDetails),
    TokenizedAsset(TokenizedAssetDetails),
}

impl InstrumentKind {
    pub fn is_spot(&self) -> bool {
        matches!(self, InstrumentKind::Spot(_))
    }

    pub fn is_swap(&self) -> bool {
        matches!(self, InstrumentKind::Swap(_))
    }

    pub fn is_future(&self) -> bool {
        matches!(self, InstrumentKind::Future(_))
    }

    pub fn is_option(&self) -> bool {
        matches!(self, InstrumentKind::Option(_))
    }
}

/// Common fields for SPOT instruments (CurrencyPair, Equity, Commodity, etc.).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpotDetails {
    /// Base currency (e.g., "BTC" for BTCUSDT).
    pub base_currency: Option<String>,
}

/// Common fields for SWAP/Perpetual instruments.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SwapDetails {
    pub base_currency: Option<String>,
    pub settlement_currency: Option<String>,
    pub is_quanto: bool,
    pub funding_rate: Option<f64>,
    pub funding_rate_interval_h: Option<u32>,
}

/// Common fields for FUTURE instruments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FutureDetails {
    pub exchange: Option<String>,
    pub underlying: String,
    pub activation_ns: u64,
    pub expiration_ns: u64,
    pub settlement_type: SettlementType,
}

impl Default for FutureDetails {
    fn default() -> Self {
        Self {
            exchange: None,
            underlying: String::new(),
            activation_ns: 0,
            expiration_ns: 0,
            settlement_type: SettlementType::PHYSICAL,
        }
    }
}

/// Spread futures (calendar spreads).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FuturesSpreadDetails {
    pub legs: Vec<SpreadLeg>,
}

/// Single leg of a spread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpreadLeg {
    pub instrument_id: u32,
    pub ratio: f64,
}

/// Forward contract (placeholder for completeness).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ForwardDetails {
    pub settlement_type: SettlementType,
}

/// CFD (Contract for Difference).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CfdDetails {
    pub base_currency: Option<String>,
}

/// Bond (placeholder for completeness).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BondDetails {}

/// Option contract details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionDetails {
    pub exchange: Option<String>,
    pub underlying: String,
    pub option_kind: OptionKind,
    pub strike_price: i64,
    pub strike_currency: Option<String>,
    pub exercise_style: ExerciseStyle,
    pub activation_ns: u64,
    pub expiration_ns: u64,
    pub settlement_type: SettlementType,
    /// The currency of the underlying asset.
    pub underlying_currency: String,
    /// The currency in which the option is quoted/traded.
    pub option_currency: String,
}

impl Default for OptionDetails {
    fn default() -> Self {
        Self {
            exchange: None,
            underlying: String::new(),
            option_kind: OptionKind::CALL,
            strike_price: 0,
            strike_currency: None,
            exercise_style: ExerciseStyle::EUROPEAN,
            activation_ns: 0,
            expiration_ns: 0,
            settlement_type: SettlementType::CASH,
            underlying_currency: String::new(),
            option_currency: String::new(),
        }
    }
}

/// Option spread.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OptionSpreadDetails {
    pub legs: Vec<SpreadLeg>,
}

/// Warrant (placeholder for completeness).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WarrantDetails {}

/// Sports betting instrument.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SportsBettingDetails {
    pub sport: Option<String>,
    pub competition: Option<String>,
    pub event_id: Option<String>,
    pub market_name: Option<String>,
    pub market_start_ns: Option<u64>,
    pub selection_id: Option<String>,
    pub selection_name: Option<String>,
    pub selection_handicap: Option<f64>,
}

/// Binary option (event/digital options).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryOptionDetails {
    pub underlying: String,
    pub event_id: Option<String>,
    pub event_description: Option<String>,
    pub expiration_ns: u64,
    pub settlement_type: SettlementType,
}

impl Default for BinaryOptionDetails {
    fn default() -> Self {
        Self {
            underlying: String::new(),
            event_id: None,
            event_description: None,
            expiration_ns: 0,
            settlement_type: SettlementType::CASH,
        }
    }
}

/// Equity index.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexDetails {}

/// Tokenized asset.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenizedAssetDetails {}

/// Builder for creating instruments with fluent API.
pub struct InstrumentBuilder {
    inner: Instrument,
}

impl InstrumentBuilder {
    pub fn new(
        symbol: &str,
        venue: &str,
        class: InstrumentClass,
        asset_class: AssetClass,
        quote_currency: &str,
    ) -> Self {
        let kind = match class {
            InstrumentClass::SPOT => InstrumentKind::Spot(SpotDetails::default()),
            InstrumentClass::SWAP => InstrumentKind::Swap(SwapDetails::default()),
            InstrumentClass::FUTURE => InstrumentKind::Future(FutureDetails::default()),
            InstrumentClass::FuturesSpread => {
                InstrumentKind::FuturesSpread(FuturesSpreadDetails::default())
            }
            InstrumentClass::FORWARD => InstrumentKind::Forward(ForwardDetails::default()),
            InstrumentClass::CFD => InstrumentKind::Cfd(CfdDetails::default()),
            InstrumentClass::BOND => InstrumentKind::Bond(BondDetails::default()),
            InstrumentClass::OPTION => InstrumentKind::Option(OptionDetails::default()),
            InstrumentClass::OPTION_SPREAD => {
                InstrumentKind::OptionSpread(OptionSpreadDetails::default())
            }
            InstrumentClass::WARRANT => InstrumentKind::Warrant(WarrantDetails::default()),
            InstrumentClass::SPORTS_BETTING => {
                InstrumentKind::SportsBetting(SportsBettingDetails::default())
            }
            InstrumentClass::BINARY_OPTION => {
                InstrumentKind::BinaryOption(BinaryOptionDetails::default())
            }
        };
        Self {
            inner: Instrument::new(symbol, venue, class, asset_class, quote_currency, kind, 0, 0),
        }
    }

    pub fn ts_event(mut self, ts: u64) -> Self {
        self.inner.ts_event = ts;
        self
    }

    pub fn ts_init(mut self, ts: u64) -> Self {
        self.inner.ts_init = ts;
        self
    }

    pub fn price_precision(mut self, p: u8) -> Self {
        self.inner.price_precision = p;
        self
    }

    pub fn size_precision(mut self, p: u8) -> Self {
        self.inner.size_precision = p;
        self
    }

    pub fn price_increment(mut self, v: i64) -> Self {
        self.inner.price_increment = v;
        self
    }

    pub fn size_increment(mut self, v: i64) -> Self {
        self.inner.size_increment = v;
        self
    }

    pub fn multiplier(mut self, v: i64) -> Self {
        self.inner.multiplier = v;
        self
    }

    pub fn lot_size(mut self, v: i64) -> Self {
        self.inner.lot_size = v;
        self
    }

    pub fn margin_init(mut self, v: f64) -> Self {
        self.inner.margin_init = v;
        self
    }

    pub fn margin_maint(mut self, v: f64) -> Self {
        self.inner.margin_maint = v;
        self
    }

    pub fn maker_fee(mut self, v: f64) -> Self {
        self.inner.maker_fee = v;
        self
    }

    pub fn taker_fee(mut self, v: f64) -> Self {
        self.inner.taker_fee = v;
        self
    }

    pub fn is_inverse(mut self, v: bool) -> Self {
        self.inner.is_inverse = v;
        self
    }

    pub fn max_quantity(mut self, v: i64) -> Self {
        self.inner.max_quantity = Some(v);
        self
    }

    pub fn min_quantity(mut self, v: i64) -> Self {
        self.inner.min_quantity = Some(v);
        self
    }

    pub fn funding_rate(mut self, v: f64) -> Self {
        if let InstrumentKind::Swap(ref mut s) = self.inner.kind {
            s.funding_rate = Some(v);
        }
        self
    }

    pub fn kind(mut self, k: InstrumentKind) -> Self {
        self.inner.kind = k;
        self
    }

    pub fn scheme(mut self, name: &str) -> Self {
        self.inner.scheme_name = Some(name.to_string());
        self
    }

    pub fn build(self) -> Instrument {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::enums::*;

    #[test]
    fn test_build_crypto_perpetual() {
        let instr = InstrumentBuilder::new(
            "BTCUSDT",
            "BINANCE",
            InstrumentClass::SWAP,
            AssetClass::CRYPTOCURRENCY,
            "USDT",
        )
        .price_precision(2)
        .size_precision(6)
        .price_increment(1) // 0.01 with precision 2
        .size_increment(1) // 0.000001 with precision 6
        .multiplier(1)
        .margin_init(0.05) // 5% initial margin
        .margin_maint(0.02) // 2% maintenance
        .build();

        assert_eq!(instr.symbol, "BTCUSDT");
        assert_eq!(instr.venue, "BINANCE");
        assert!(matches!(instr.kind, InstrumentKind::Swap(_)));
        assert!(instr.margin_init > 0.0);
    }

    #[test]
    fn test_instrument_is_expiring() {
        let fut = InstrumentBuilder::new(
            "BTC.PERP",
            "BYBIT",
            InstrumentClass::FUTURE,
            AssetClass::CRYPTOCURRENCY,
            "USDT",
        )
        .build();
        assert!(fut.is_expiring());

        let spot = InstrumentBuilder::new(
            "BTCUSDT",
            "BINANCE",
            InstrumentClass::SPOT,
            AssetClass::CRYPTOCURRENCY,
            "USDT",
        )
        .build();
        assert!(!spot.is_expiring());
    }

    #[test]
    fn test_option_allows_negative() {
        let opt = InstrumentBuilder::new(
            "BTC-50000-C",
            "DERIBIT",
            InstrumentClass::OPTION,
            AssetClass::CRYPTOCURRENCY,
            "USDT",
        )
        .build();
        assert!(opt.allows_negative_price());
        assert!(opt.class.allows_negative_price());
    }

    #[test]
    fn test_fnv1a_consistent() {
        // Verify our FNV-1a function is case-sensitive
        let h1 = fnv1a_hash(b"BTCUSDT.BINANCE");
        let h2 = fnv1a_hash(b"btcusdt.binance");
        // These should be different because FNV is case-sensitive
        assert_ne!(h1, h2);
        // But stable across calls
        assert_eq!(h1, fnv1a_hash(b"BTCUSDT.BINANCE"));
    }

    #[test]
    fn test_instrument_with_tick_scheme() {
        use crate::instrument::tick_scheme::registry::get_scheme;
        let instr = InstrumentBuilder::new(
            "BTCUSDT",
            "BINANCE",
            InstrumentClass::SPOT,
            AssetClass::CRYPTOCURRENCY,
            "USDT",
        )
        .scheme("BINANCE.DEFAULT")
        .build();
        assert_eq!(instr.scheme_name, Some("BINANCE.DEFAULT".to_string()));
        let scheme = instr.get_tick_scheme();
        assert!(scheme.is_some());
        assert_eq!(scheme.unwrap().price_increment(), 0.01);
    }
}
