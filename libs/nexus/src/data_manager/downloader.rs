//! Exchange downloader trait and implementations.
//!
//! Each exchange has its own API for historical data. The `Downloader` orchestrates
//! the flow: check catalog → identify missing → call exchange-specific source → raw data.
//!
//! The actual HTTP/API calls live in exchange-specific modules. This module provides
//! the trait boundary so the DataManager stays exchange-agnostic.

use chrono::NaiveDate;
use std::path::PathBuf;
use crate::data_manager::types::{Exchange, Venue};

/// Raw downloaded trade data from an exchange.
#[derive(Debug)]
pub struct RawTradeData {
    pub exchange: Exchange,
    pub venue: Venue,
    pub symbol: String,
    pub date: NaiveDate,
    /// Vector of (timestamp_ns, price_int, size_int) — nano-integer format
    pub trades: Vec<(u64, i64, i64)>,
}

/// Trait for exchange-specific download implementations.
pub trait DownloadSource {
    /// Human-readable name (e.g. "Binance Historical Archive")
    fn name(&self) -> &str;

    /// The exchange this source handles.
    fn exchange(&self) -> Exchange;

    /// Download raw trade data for a specific symbol and date.
    /// Returns the raw data or an error.
    fn download(&self, symbol: &str, date: NaiveDate) -> Result<RawTradeData, DownloadError>;
}

/// Download error types.
#[derive(Debug)]
pub enum DownloadError {
    /// Exchange returned an HTTP error (4xx/5xx)
    Http(u16, String),
    /// Rate limited — retry after the indicated duration
    RateLimited(u64),
    /// Symbol/date not supported by this exchange
    Unsupported(String),
    /// Network error
    Network(String),
    /// Parse error in exchange response
    Parse(String),
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::Http(code, msg) => write!(f, "HTTP {}: {}", code, msg),
            DownloadError::RateLimited(after_ms) => write!(f, "Rate limited, retry after {}ms", after_ms),
            DownloadError::Unsupported(msg) => write!(f, "Unsupported: {}", msg),
            DownloadError::Network(msg) => write!(f, "Network error: {}", msg),
            DownloadError::Parse(msg) => write!(f, "Parse error: {}", msg),
        }
    }
}

impl std::error::Error for DownloadError {}

/// Orchestrates downloads across all exchange sources.
/// Holds all registered `DownloadSource` instances.
#[derive(Default)]
pub struct Downloader {
    sources: Vec<Box<dyn DownloadSource>>,
}

impl Downloader {
    pub fn new() -> Self {
        Self { sources: Vec::new() }
    }

    /// Register an exchange download source.
    pub fn register<S: DownloadSource + 'static>(&mut self, source: S) -> &mut Self {
        self.sources.push(Box::new(source));
        self
    }

    /// Download data for a specific exchange/symbol/date.
    /// Returns error if no source is registered for that exchange.
    pub fn download(
        &self,
        exchange: Exchange,
        venue: Venue,
        symbol: &str,
        date: NaiveDate,
    ) -> Result<RawTradeData, DownloadError> {
        let source = self.sources.iter().find(|s| s.exchange() == exchange)
            .ok_or_else(|| DownloadError::Unsupported(format!("No download source for {:?}", exchange)))?;
        let mut data = source.download(symbol, date)?;
        data.venue = venue;
        Ok(data)
    }

    /// Returns true if a source is registered for the given exchange.
    pub fn has_source(&self, exchange: Exchange) -> bool {
        self.sources.iter().any(|s| s.exchange() == exchange)
    }
}