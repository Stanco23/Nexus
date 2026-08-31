//! Exchange-specific downloader implementations.
//!
//! Each downloader handles the specific HTTP endpoints and CSV/ZIP parsing
//! for a particular exchange's historical data archive.

pub mod binance;
pub mod bybit;
pub mod parsers;

#[cfg(test)]
pub mod test_integration;

pub use binance::BinanceDownloader;
pub use bybit::BybitDownloader;
pub use parsers::{parse_price_to_int, parse_qty_to_int};