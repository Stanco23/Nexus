//! Bybit historical data downloader.
//!
//! Downloads raw trade data from Bybit's public data archive.
//!
//! **URL patterns:**
//!
//! Linear/Perpetuals (trading):
//! `https://public.bybit.com/trading/{symbol}/{symbol}{YYYY-MM-DD}.csv.gz`
//!
//! Spot:
//! `https://public.bybit.com/spot/{symbol}/{symbol}-{YYYY-MM-DD}.csv.gz`
//!
//! **CSV format for trading (per row):**
//! `timestamp,symbol,side,size,price,tickDirection,trdMatchID,grossValue,homeNotional,foreignNotional`
//!
//! **CSV format for spot (per row):**
//! `timestamp,symbol,side,size,price,...,...`
//!
//! **Notes:**
//! - Timestamp is in seconds with fractional part (e.g., 1735689600.0974)
//! - Must be converted to nanoseconds
//! - Side: Buy/Sell or TickDirection can be used for aggressor side

use std::io::{BufReader, Read};
use chrono::NaiveDate;
use flate2::read::GzDecoder;

use crate::data_manager::downloader::{DownloadSource, DownloadError, RawTradeData};
use crate::data_manager::types::{Exchange, Venue};

pub struct BybitDownloader {
    client: reqwest::blocking::Client,
    precision: u8,
}

impl BybitDownloader {
    pub fn new() -> Self {
        Self {
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .user_agent("curl/8.1.0")  // Bybit blocks default reqwest UA
                .build()
                .expect("Failed to create HTTP client"),
            precision: 9,
        }
    }

    pub fn with_precision(mut self, precision: u8) -> Self {
        self.precision = precision;
        self
    }

    fn download_gz(&self, url: &str) -> Result<Vec<u8>, DownloadError> {
        let response = self.client.get(url).send()
            .map_err(|e| DownloadError::Network(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            return Err(DownloadError::Http(status.as_u16(), format!("Failed to fetch {}", url)));
        }

        let bytes = response.bytes()
            .map_err(|e| DownloadError::Network(e.to_string()))?;

        Ok(bytes.to_vec())
    }

    /// Parse trading (linear) CSV.GZ file — daily format: {symbol}{YYYY-MM-DD}.csv.gz
    fn parse_trading_gz(&self, gz_data: &[u8], date: NaiveDate) -> Result<RawTradeData, DownloadError> {
        let cursor = std::io::Cursor::new(gz_data);
        let decoder = GzDecoder::new(cursor);
        let reader = BufReader::new(decoder);
        self.parse_trading_csv(reader, date)
    }

    /// Parse trading CSV — format:
    /// `timestamp,symbol,side,size,price,tickDirection,trdMatchID,grossValue,homeNotional,foreignNotional`
    fn parse_trading_csv<R: Read>(&self, reader: BufReader<R>, date: NaiveDate) -> Result<RawTradeData, DownloadError> {
        use csv::ReaderBuilder;

        let mut csv_reader = ReaderBuilder::new()
            .has_headers(true)
            .flexible(true)
            .from_reader(reader);

        let mut trades = Vec::new();

        for (i, result) in csv_reader.records().enumerate() {
            let record = result.map_err(|e| DownloadError::Parse(format!("CSV error at row {}: {}", i, e)))?;

            // timestamp is field 0
            let ts_str = &record[0];
            let price_str = &record[4];
            let size_str = &record[3];
            // side is field 2: "Buy" or "Sell"

            // Parse timestamp — seconds with fractional part, convert to ns
            let ts_f: f64 = ts_str.parse()
                .map_err(|_| DownloadError::Parse(format!("Invalid timestamp at row {}: {}", i, ts_str)))?;
            let timestamp_ns = (ts_f * 1_000_000_000.0) as u64;

            let price_int = parse_price_to_int(price_str, self.precision);
            let size_int = parse_qty_to_int(size_str, self.precision);

            trades.push((timestamp_ns, price_int, size_int));
        }

        Ok(RawTradeData {
            exchange: Exchange::Bybit,
            venue: Venue::Linear,
            symbol: "BTCUSDT".to_string(), // will be overridden
            date,
            trades,
        })
    }

    /// Download trading (linear) data for a symbol on a specific date.
    pub fn fetch_trading(&self, symbol: &str, date: NaiveDate) -> Result<RawTradeData, DownloadError> {
        let filename = format!("{}{}.csv.gz", symbol, date);
        let url = format!("https://public.bybit.com/trading/{}/{}", symbol, filename);

        let gz_data = self.download_gz(&url)?;
        let mut data = self.parse_trading_gz(&gz_data, date)?;
        data.symbol = symbol.to_string();
        data.venue = Venue::Linear;
        Ok(data)
    }

    /// Download spot data for a symbol on a specific date.
    /// Spot data comes in monthly files: {symbol}-{YYYY-MM}.csv.gz
    /// But we can filter by date within the month.
    pub fn fetch_spot(&self, symbol: &str, date: NaiveDate) -> Result<RawTradeData, DownloadError> {
        let month_str = date.format("%Y-%m").to_string();
        let filename = format!("{}-{}.csv.gz", symbol, month_str);
        let url = format!("https://public.bybit.com/spot/{}/{}", symbol, filename);

        let gz_data = self.download_gz(&url)?;
        let cursor = std::io::Cursor::new(gz_data);
        let decoder = GzDecoder::new(cursor);
        let reader = BufReader::new(decoder);

        // Parse and filter to just the requested date
        use csv::ReaderBuilder;
        let mut csv_reader = ReaderBuilder::new()
            .has_headers(true)
            .flexible(true)
            .from_reader(reader);

        let mut trades = Vec::new();
        let date_str = date.format("%Y-%m-%d").to_string();

        for (i, result) in csv_reader.records().enumerate() {
            let record = result.map_err(|e| DownloadError::Parse(format!("CSV error at row {}: {}", i, e)))?;

            let ts_str = &record[0];
            // Skip rows that don't start with our date
            if !ts_str.starts_with(&date_str) {
                continue;
            }

            let ts_f: f64 = ts_str.parse()
                .map_err(|_| DownloadError::Parse(format!("Invalid timestamp at row {}: {}", i, ts_str)))?;
            let timestamp_ns = (ts_f * 1_000_000_000.0) as u64;

            let price_str = &record[4];
            let size_str = &record[3];

            let price_int = parse_price_to_int(price_str, self.precision);
            let size_int = parse_qty_to_int(size_str, self.precision);

            trades.push((timestamp_ns, price_int, size_int));
        }

        Ok(RawTradeData {
            exchange: Exchange::Bybit,
            venue: Venue::Spot,
            symbol: symbol.to_string(),
            date,
            trades,
        })
    }
}

impl Default for BybitDownloader {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadSource for BybitDownloader {
    fn name(&self) -> &str {
        "Bybit Public Data Archive"
    }

    fn exchange(&self) -> Exchange {
        Exchange::Bybit
    }

    fn download(&self, symbol: &str, date: NaiveDate) -> Result<RawTradeData, DownloadError> {
        // For Bybit, we'll default to trading (linear) which is more commonly used
        self.fetch_trading(symbol, date)
    }
}

/// Parse a decimal price string to a nano-integer with target precision.
///
/// This handles variable decimal lengths correctly. For example:
/// - "93530.00" (2 decimals) with precision 9 → 93530000000
/// - "0.003" (3 decimals) with precision 9 → 3000000
fn parse_price_to_int(s: &str, precision: u8) -> i64 {
    let parts: Vec<&str> = s.split('.').collect();
    let int_part: i64 = parts.get(0).unwrap_or(&"0").parse().unwrap_or(0);
    let dec_str = parts.get(1).unwrap_or(&"0");
    let dec_len = dec_str.len() as i32;

    // Combined digits: "93530" (for "93530.00")
    let mut combined = String::new();
    combined.push_str(parts.get(0).unwrap_or(&"0"));
    combined.push_str(dec_str);
    let combined_int: i64 = combined.parse().unwrap_or(0);

    let diff = precision as i32 - dec_len;
    if diff >= 0 {
        combined_int * 10_i64.pow(diff as u32)
    } else {
        combined_int / 10_i64.pow((-diff) as u32)
    }
}

/// Parse a decimal qty string to a nano-integer.
fn parse_qty_to_int(s: &str, precision: u8) -> i64 {
    parse_price_to_int(s, precision)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_price_to_int() {
        // "93530.00" has 2 decimals, precision 9: combined=9353000, diff=7 → 9353000 * 10^7 = 93530000000000
        let p = parse_price_to_int("93530.00", 9);
        assert_eq!(p, 93530000000000);
    }

    #[test]
    fn test_parse_small_qty() {
        // "0.003" has 3 decimals, precision 9: combined=3, diff=6 → 3 * 10^6 = 3000000
        let q = parse_qty_to_int("0.003", 9);
        assert_eq!(q, 3000000);
    }
}