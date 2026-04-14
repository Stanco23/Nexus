//! Instrument types and identifiers.
//!
//! # Instrument Hierarchy (14 types)
//! - `CurrencyPair` — FX pairs (pip conventions, lot size, overnight swap)
//! - `CryptoFuture` — BTC-PERP, ETH-PERP (funding rate, settlement)
//! - `CryptoPerpetual` — USDT-M linear perps (funding rate, mark price)
//! - `CryptoOption` — option contracts (exercise style, delta)
//! - `FuturesContract` — traditional futures (expiry, settlement price)
//! - `Commodity` — gold, oil (unit, tick size)
//! - `Equity` — stocks (dividend adjustments, corporate actions)
//! - `Betting` — sports betting (odds, stake)
//! - `BinaryOption` — binary outcomes
//! - `Cfd` — contract for difference
//! - `FuturesSpread` — calendar spreads
//! - `Index` — equity index
//! - `Synthetic` — derived from formula (see Phase 1.9)
//! - `TokenizedAsset` — tokenized securities

pub mod enums;
pub mod instrument_id;
pub mod registry;
pub mod synthetic;
pub mod types;

pub use enums::{AssetClass, InstrumentClass, OptionKind};
pub use instrument_id::InstrumentId;
pub use registry::InstrumentRegistry;
pub use synthetic::{parse_formula, Formula, SyntheticInstrument};
pub use types::Instrument;
