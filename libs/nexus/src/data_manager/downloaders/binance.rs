//! Binance historical data downloader.
//!
//! Downloads raw trade data from Binance's public data archive.
//!
//! **URL pattern:**
//! `https://data.binance.vision/data/spot/daily/trades/{symbol}/{symbol}-trades-{YYYY-MM-DD}.zip`
//!
//! **CSV format (per row):**
//! `trade_id, price, qty, quoteQty, time, isBuyerMaker, isBestMatch`
//!
//! **Notes:**
//! - Timestamp from Jan 1 2025 onwards is in **microseconds** (not nanoseconds)
//! - Earlier data uses nanoseconds. We normalize everything to nanoseconds.

use std::io::{BufReader, Read};
use chrono::NaiveDate;
use zip::ZipArchive;

use crate::data_manager::downloader::{DownloadSource, DownloadError, RawTradeData};
use crate::data_manager::downloaders::parsers::{parse_price_to_int, parse_qty_to_int};
use crate::data_manager::types::Exchange;

const BASE_URL: &str = "https://data.binance.vision/data/spot/daily/trades";

pub struct BinanceDownloader {
    client: reqwest::blocking::Client,
    precision: u8,
}

impl BinanceDownloader {
    pub fn new() -> Self {
        Self {
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
            precision: 9,
        }
    }

    pub fn with_precision(mut self, precision: u8) -> Self {
        self.precision = precision;
        self
    }

    fn download_zip(&self, symbol: &str, date: NaiveDate) -> Result<Vec<u8>, DownloadError> {
        let filename = format!("{}-trades-{}.zip", symbol, date);
        let url = format!("{}/{}/{}", BASE_URL, symbol, filename);

        let response = self.client.get(&url).send()
            .map_err(|e| DownloadError::Network(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            return Err(DownloadError::Http(status.as_u16(), format!("Failed to fetch {}", url)));
        }

        let bytes = response.bytes()
            .map_err(|e| DownloadError::Network(e.to_string()))?;

        Ok(bytes.to_vec())
    }

    fn parse_zip(&self, zip_data: &[u8], date: NaiveDate) -> Result<RawTradeData, DownloadError> {
        let cursor = std::io::Cursor::new(zip_data);
        let reader = BufReader::new(cursor);
        let mut archive = ZipArchive::new(reader)
            .map_err(|e| DownloadError::Parse(format!("Invalid ZIP: {}", e)))?;

        if archive.len() != 1 {
            return Err(DownloadError::Parse(format!("Expected 1 file in ZIP, got {}", archive.len())));
        }

        let mut file = archive.by_index(0)
            .map_err(|e| DownloadError::Parse(format!("Failed to read ZIP entry: {}", e)))?;

        let mut csv_data = String::new();
        file.read_to_string(&mut csv_data)
            .map_err(|e| DownloadError::Parse(format!("Failed to read CSV content: {}", e)))?;

        self.parse_csv(&csv_data, date)
    }

    fn parse_csv(&self, csv_data: &str, date: NaiveDate) -> Result<RawTradeData, DownloadError> {
        let mut trades = Vec::new();
        let mut lines = csv_data.lines();

        // Skip header
        let _header = lines.next()
            .ok_or_else(|| DownloadError::Parse("Empty CSV".to_string()))?;

        for (i, line) in lines.enumerate() {
            let fields: Vec<&str> = line.split(',').collect();
            if fields.len() < 6 {
                continue; // skip malformed rows
            }

            // trade_id, price, qty, quoteQty, time, isBuyerMaker, isBestMatch
            let price_str = fields[1].trim();
            let qty_str = fields[2].trim();
            let time_str = fields[4].trim();
            // isBuyerMaker: true = the buyer was the maker (so the taker sold) → side=1
            //               false = the buyer was the taker → side=0
            let is_buyer_maker_str = fields[5].trim();
            let is_buyer_maker: bool = is_buyer_maker_str.parse()
                .map_err(|_| DownloadError::Parse(format!("Invalid isBuyerMaker at line {}: {}", i + 2, is_buyer_maker_str)))?;
            let side: u8 = if is_buyer_maker { 1 } else { 0 };

            // Parse timestamp — microseconds (from 2025-01-01) or nanoseconds
            let raw_ts: u64 = time_str.parse()
                .map_err(|_| DownloadError::Parse(format!("Invalid timestamp at line {}: {}", i + 2, time_str)))?;

            let timestamp_ns = normalize_timestamp_ns(raw_ts);
            let price_int = parse_price_to_int(price_str, self.precision);
            let size_int = parse_qty_to_int(qty_str, self.precision);

            trades.push((timestamp_ns, price_int, size_int, side));
        }

        Ok(RawTradeData {
            exchange: Exchange::Binance,
            venue: crate::data_manager::types::Venue::Spot,
            symbol: "BTCUSDT".to_string(), // will be overridden by caller
            date,
            trades,
        })
    }

    /// Download and parse for a given symbol and date.
    pub fn fetch(&self, symbol: &str, date: NaiveDate) -> Result<RawTradeData, DownloadError> {
        let zip_data = self.download_zip(symbol, date)?;
        let mut data = self.parse_zip(&zip_data, date)?;
        data.symbol = symbol.to_string();
        Ok(data)
    }
}

impl Default for BinanceDownloader {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadSource for BinanceDownloader {
    fn name(&self) -> &str {
        "Binance Historical Archive"
    }

    fn exchange(&self) -> Exchange {
        Exchange::Binance
    }

    fn download(&self, symbol: &str, date: NaiveDate) -> Result<RawTradeData, DownloadError> {
        self.fetch(symbol, date)
    }
}

/// Normalize timestamp to nanoseconds.
/// Binance uses:
/// - Microseconds for dates from 2025-01-01 onwards (16 digit numbers like 1735689600010866)
/// - Nanoseconds for earlier data (19 digit numbers like 1640995200000000000)
///
/// We detect microseconds by checking if the value is < 10^16.
/// Microseconds for year 2025 are ~1.7×10^15, nanoseconds are ~1.7×10^18.
fn normalize_timestamp_ns(ts: u64) -> u64 {
    if ts < 10_000_000_000_000_000_u64 {
        // Microseconds → multiply by 1000
        ts * 1000
    } else {
        // Nanoseconds → already in correct unit
        ts
    }
}

// `parse_price_to_int` and `parse_qty_to_int` are imported from `parsers` module.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_timestamp_ns() {
        // Microsecond timestamp for 2025-01-01 (16 digits)
        // 1735689600010866 μs → 1735689600010866000 ns
        assert_eq!(
            normalize_timestamp_ns(1735689600010866_u64),
            1735689600010866000_u64
        );
        // Nanosecond timestamp (19 digits) - stays unchanged
        assert_eq!(
            normalize_timestamp_ns(1640995200000000000_u64),
            1640995200000000000_u64
        );
    }
}