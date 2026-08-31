//! Core types for the data manager.

use std::path::PathBuf;
use std::str::FromStr;
use serde::{Deserialize, Serialize};

/// Supported exchanges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Exchange {
    Binance,
    Bybit,
    Okx,
    Coinbase,
}

impl Exchange {
    pub fn as_str(&self) -> &'static str {
        match self {
            Exchange::Binance => "binance",
            Exchange::Bybit => "bybit",
            Exchange::Okx => "okx",
            Exchange::Coinbase => "coinbase",
        }
    }
}

impl FromStr for Exchange {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "binance" => Ok(Exchange::Binance),
            "bybit" => Ok(Exchange::Bybit),
            "okx" | "okex" => Ok(Exchange::Okx),
            "coinbase" => Ok(Exchange::Coinbase),
            other => Err(format!("unknown exchange: {}", other)),
        }
    }
}

/// Venue type within an exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Venue {
    Spot,
    Futures,
    Linear,   // USDT perpetuals (Bybit/OKX)
    Swap,     // Coin-M perpetuals
}

impl Venue {
    pub fn as_str(&self) -> &'static str {
        match self {
            Venue::Spot => "spot",
            Venue::Futures => "futures",
            Venue::Linear => "linear",
            Venue::Swap => "swap",
        }
    }
}

/// A specific TVC3 data file on disk.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TvcFile {
    pub exchange: Exchange,
    pub venue: Venue,
    pub symbol: String,
    pub date: NaiveDate,
    pub path: PathBuf,
    pub size_bytes: u64,
}

use chrono::NaiveDate;

/// Configuration for loading data into a backtest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataManagerConfig {
    /// Root data directory (e.g. "/home/user/Nexus/data")
    pub data_root: PathBuf,
    /// Exchange to load from
    pub exchange: Exchange,
    /// Venue type
    pub venue: Venue,
    /// Symbol (e.g. "BTCUSDT")
    pub symbol: String,
    /// Start date (inclusive)
    pub start_date: NaiveDate,
    /// End date (inclusive)
    pub end_date: NaiveDate,
    /// If true, missing files trigger download. If false, error on miss.
    pub download_on_miss: bool,
}

impl DataManagerConfig {
    /// Build the expected TVC3 path for a given date.
    /// Pattern: `{data_root}/{exchange}/{venue}/{symbol}/{date}.tvc`
    pub fn tvc_path(&self, date: NaiveDate) -> PathBuf {
        self.data_root
            .join(self.exchange.as_str())
            .join(self.venue.as_str())
            .join(&self.symbol)
            .join(format!("{}.tvc", date))
    }

    /// Collect all expected TVC3 paths for the configured date range.
    pub fn all_tvc_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let mut current = self.start_date;
        while current <= self.end_date {
            paths.push(self.tvc_path(current));
            current = current.succ_opt().unwrap_or(current);
        }
        paths
    }
}