//! Exchange adapters — normalize exchange WebSocket messages to TradeTick.
//!
//! # Supported Exchanges
//! - Binance: SPOT and USDT-M futures
//! - Bybit: WebSocket v3
//! - OKX: WebSocket streams

pub mod binance;

pub use binance::{BinanceAdapter, BinanceVenue, NormalizedTick};
