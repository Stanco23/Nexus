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
use crate::data_manager::downloaders::parsers::{parse_price_to_int, parse_qty_to_int};
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

            trades.push((timestamp_ns, price_int, size_int, 0));
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

            trades.push((timestamp_ns, price_int, size_int, 0));
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

// `parse_price_to_int` and `parse_qty_to_int` are imported from `parsers` module.

#[cfg(test)]
mod tests {
    // parse_price/parse_qty tests moved to `parsers` module.
}