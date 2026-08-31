//! Exchange K-line → TVCB bar ingestion.
//!
//! Fetches OHLCV data from exchange HTTP APIs and writes to yearly TVCB files.

use std::path::{Path, PathBuf};
use chrono::{NaiveDate, Datelike};
use reqwest::Client;
use serde::Deserialize;

use tvc::tvcb::writer::TvcbWriter;
use tvc::tvcb::types::Bar;

// =============================================================================
// InstrumentType
// =============================================================================

/// Instrument type for a given exchange (spot, futures, inverse).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstrumentType {
    Spot,
    Futures,
    Inverse,
}

impl InstrumentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            InstrumentType::Spot => "spot",
            InstrumentType::Futures => "futures",
            InstrumentType::Inverse => "inverse",
        }
    }
}

// =============================================================================
// ExchangeKind
// =============================================================================

/// Supported exchange for bar ingestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExchangeKind {
    Binance,
    Bybit,
    Okx,
}

impl ExchangeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExchangeKind::Binance => "binance",
            ExchangeKind::Bybit => "bybit",
            ExchangeKind::Okx => "okx",
        }
    }

    /// Convert timeframe string to exchange-specific interval keyword.
    pub fn interval_key(&self, tf: &str) -> String {
        match tf {
            "1m" => match self {
                ExchangeKind::Binance => "1m".into(),
                ExchangeKind::Bybit => "1".into(),
                ExchangeKind::Okx => "1m".into(),
            },
            "5m" => match self {
                ExchangeKind::Binance => "5m".into(),
                ExchangeKind::Bybit => "5".into(),
                ExchangeKind::Okx => "5m".into(),
            },
            "15m" => match self {
                ExchangeKind::Binance => "15m".into(),
                ExchangeKind::Bybit => "15".into(),
                ExchangeKind::Okx => "15m".into(),
            },
            "1h" => match self {
                ExchangeKind::Binance => "1h".into(),
                ExchangeKind::Bybit => "60".into(),
                ExchangeKind::Okx => "1h".into(),
            },
            "4h" => match self {
                ExchangeKind::Binance => "4h".into(),
                ExchangeKind::Bybit => "240".into(),
                ExchangeKind::Okx => "4h".into(),
            },
            "1d" => match self {
                ExchangeKind::Binance => "1d".into(),
                ExchangeKind::Bybit => "D".into(),
                ExchangeKind::Okx => "1d".into(),
            },
            _ => tf.to_string(),
        }
    }
}

// =============================================================================
// BarIngester
// =============================================================================

/// Exchange K-line → TVCB writer.
///
/// Fetches OHLCV data from exchange HTTP APIs and writes yearly TVCB files.
pub struct BarIngester {
    exchange: ExchangeKind,
    instrument_type: InstrumentType,
    timeframe_ns: u64,
    anchor_interval: u32,
    decimal_precision: u8,
    client: Client,
}

impl BarIngester {
    /// Create a new ingester for the given exchange, instrument type, and timeframe.
    ///
    /// `timeframe_ns` is the bar period in nanoseconds (e.g. 900_000_000_000 for 15m).
    pub fn new(exchange: ExchangeKind, instrument_type: InstrumentType, timeframe_ns: u64) -> Self {
        let anchor_interval = Self::compute_anchor_interval(timeframe_ns);
        Self {
            exchange,
            instrument_type,
            timeframe_ns,
            anchor_interval,
            decimal_precision: 9, // nanodollars
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    /// Create for Binance spot with a timeframe string (e.g. "15m").
    pub fn binance_spot(timeframe: &str) -> Self {
        Self::new(ExchangeKind::Binance, InstrumentType::Spot, timeframe_to_ns(timeframe))
    }

    /// Create for Binance futures with a timeframe string.
    pub fn binance_futures(timeframe: &str) -> Self {
        Self::new(ExchangeKind::Binance, InstrumentType::Futures, timeframe_to_ns(timeframe))
    }

    /// Create for Bybit spot with a timeframe string.
    pub fn bybit_spot(timeframe: &str) -> Self {
        Self::new(ExchangeKind::Bybit, InstrumentType::Spot, timeframe_to_ns(timeframe))
    }

    /// Create for Bybit futures (linear) with a timeframe string.
    pub fn bybit_futures(timeframe: &str) -> Self {
        Self::new(ExchangeKind::Bybit, InstrumentType::Futures, timeframe_to_ns(timeframe))
    }

    /// Create for Bybit inverse with a timeframe string.
    pub fn bybit_inverse(timeframe: &str) -> Self {
        Self::new(ExchangeKind::Bybit, InstrumentType::Inverse, timeframe_to_ns(timeframe))
    }

    /// Create for OKX spot with a timeframe string.
    pub fn okx_spot(timeframe: &str) -> Self {
        Self::new(ExchangeKind::Okx, InstrumentType::Spot, timeframe_to_ns(timeframe))
    }

    /// Create for OKX futures with a timeframe string.
    pub fn okx_futures(timeframe: &str) -> Self {
        Self::new(ExchangeKind::Okx, InstrumentType::Futures, timeframe_to_ns(timeframe))
    }

    /// Fetch K-lines from exchange and write to TVCB yearly files.
    ///
    /// Returns paths to all created TVCB files.
    pub async fn ingest(
        &self,
        symbol: &str,
        start_date: NaiveDate,
        end_date: NaiveDate,
        output_dir: &Path,
    ) -> Result<Vec<PathBuf>, BarIngesterError> {
        let interval = self.exchange.interval_key(&ns_to_tf(self.timeframe_ns));
        let mut created = Vec::new();

        // Fetch and write year-by-year
        let mut current = start_date;
        while current <= end_date {
            let year = current.year();
            let year_start = NaiveDate::from_ymd_opt(year, 1, 1).unwrap();
            let year_end = NaiveDate::from_ymd_opt(year, 12, 31).unwrap();

            let fetch_start = if current < year_start { year_start } else { current };
            let fetch_end = if current > year_end { year_end } else { current };

            let start_ms = date_to_millis(fetch_start);
            let end_ms = date_to_millis(fetch_end) + 86_400_000 - 1; // inclusive end

            match self.fetch_year(symbol, &interval, start_ms, end_ms, output_dir, year).await {
                Ok(path) => {
                    created.push(path);
                }
                Err(e) => {
                    tracing::warn!("failed to fetch {} {} year {}: {}", symbol, self.exchange.as_str(), year, e);
                }
            }

            // Move to next year
            current = NaiveDate::from_ymd_opt(year + 1, 1, 1).unwrap().max(end_date + chrono::Duration::days(1));
        }

        Ok(created)
    }

    /// Fetch one year's data and write to a single TVCB file.
    async fn fetch_year(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: u64,
        end_ms: u64,
        output_dir: &Path,
        year: i32,
    ) -> Result<PathBuf, BarIngesterError> {
        // Fetch all bars in chunks of 1000 (exchange limit)
        // Pagination differs by exchange:
        // - Binance/Bybit: use startTime advancing forward
        // - OKX: use 'before' parameter going backward from end
        let mut all_bars = Vec::new();

        // For Binance/Bybit: start from start_ms and advance forward
        // For OKX: start from end_ms and go backward
        let mut current_start = start_ms;
        let mut current_before = end_ms;
        let mut loop_count = 0;
        let timeframe_ms = self.timeframe_ns / 1_000_000;
        let bars_per_call = 1000.min((end_ms.saturating_sub(start_ms)) / timeframe_ms + 1);

        loop {
            loop_count += 1;
            // Safety: limit to 500 loops (covers ~500k bars for 1min TF)
            if loop_count > 500 {
                eprintln!("  [{}] safety break at loop {}, have {} bars",
                    self.exchange.as_str(), loop_count, all_bars.len());
                break;
            }

            let bars = if self.exchange == ExchangeKind::Okx {
                // OKX: 'before' is exclusive cursor, paginate backward
                self.fetch_okx_before(symbol, interval, current_before).await?
            } else if self.exchange == ExchangeKind::Binance {
                // Binance: use startTime only (no endTime) to get full 1000-bar batches
                self.fetch_binance_start_only(symbol, interval, current_start).await?
            } else {
                // Bybit: use startTime only (no endTime) to get full 1000-bar batches
                self.fetch_bybit_start_only(symbol, interval, current_start).await?
            };

            eprintln!("  [{}] loop {}: got {} bars (total: {})",
                self.exchange.as_str(), loop_count, bars.len(), all_bars.len());

            if bars.is_empty() {
                eprintln!("  [{}] empty response, breaking", self.exchange.as_str());
                break;
            }

            let first_ts_ms = bars.first().map(|b| b.ts_event / 1_000_000).unwrap_or(0);
            let last_ts_ms = bars.last().map(|b| b.ts_event / 1_000_000).unwrap_or(0);
            all_bars.extend(bars);

            if self.exchange == ExchangeKind::Okx {
                // OKX: break when we've gone older than start_ms
                if last_ts_ms <= start_ms {
                    eprintln!("  [OKX] reached start_ms boundary ({} <= {}), breaking",
                        last_ts_ms, start_ms);
                    break;
                }
                current_before = last_ts_ms;
            } else {
                // Binance/Bybit: break when we've passed end_ms
                if last_ts_ms >= end_ms {
                    eprintln!("  [{}] reached end_ms boundary ({} >= {}), breaking",
                        self.exchange.as_str(), last_ts_ms, end_ms);
                    break;
                }
                // Advance to after the last bar we received
                // For Binance (1000 limit): if we got exactly 1000 bars, there might be more
                // so we advance by last bar + 1 timeframe. If we got < 1000, we're done.
                let next_start = last_ts_ms + timeframe_ms;
                if next_start <= current_start {
                    // Guard against stuck loop (shouldn't happen with 15m+ timeframes)
                    eprintln!("  [{}] next_start stalled at {}, breaking",
                        self.exchange.as_str(), next_start);
                    break;
                }
                current_start = next_start;
            }
        }

        if all_bars.is_empty() {
            return Err(BarIngesterError::NoData(symbol.to_string()));
        }

        // Write to TVCB file
        let tf_str = ns_to_tf(self.timeframe_ns);
        let file_path = output_dir
            .join(self.exchange.as_str())
            .join(self.instrument_type.as_str())
            .join(symbol.to_lowercase())
            .join(tf_str)
            .join(format!("{}.tvcb", year));

        // Create parent directories
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        self.write_yearly_file(&all_bars, &file_path)?;
        Ok(file_path)
    }

    /// Fetch a batch of bars from the exchange.
    async fn fetch_bars(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: u64,
        end_ms: u64,
    ) -> Result<Vec<Bar>, BarIngesterError> {
        match self.exchange {
            ExchangeKind::Binance => self.fetch_binance(symbol, interval, start_ms, end_ms).await,
            ExchangeKind::Bybit => self.fetch_bybit(symbol, interval, start_ms, end_ms).await,
            ExchangeKind::Okx => self.fetch_okx_before(symbol, interval, end_ms).await,
        }
    }

    async fn fetch_binance(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: u64,
        end_ms: u64,
    ) -> Result<Vec<Bar>, BarIngesterError> {
        let base_url = match self.instrument_type {
            InstrumentType::Spot => "https://api.binance.com/api/v3/klines",
            InstrumentType::Futures => "https://api.binance.com/fapi/v3/klines",
            InstrumentType::Inverse => return Err(BarIngesterError::Network("Binance does not support inverse futures via public klines API".to_string())),
        };
        // Note: startTime+endTime returns at most 1000 bars AND cuts off at boundary
        // Use fetch_binance_start_only() for paginated fetching with full 1000-bar batches
        let url = format!(
            "{}?symbol={}&interval={}&startTime={}&endTime={}&limit=1000",
            base_url,
            symbol.to_uppercase(),
            interval,
            start_ms,
            end_ms,
        );

        #[derive(Deserialize)]
        struct BinanceKline {
            #[serde(rename = "0")] open_time_ms: u64,
            #[serde(rename = "1")] open: String,
            #[serde(rename = "2")] high: String,
            #[serde(rename = "3")] low: String,
            #[serde(rename = "4")] close: String,
            #[serde(rename = "5")] volume: String,
            #[serde(rename = "8")] quote_volume: String,
            #[serde(rename = "9")] num_trades: u64,
            // skip fields 6,7 (ignore, taker buy volume)
        }

        let response: Vec<Vec<serde_json::Value>> = self.client
            .get(&url)
            .send()
            .await
            .map_err(|e| {
                eprintln!("  [Binance] HTTP send error: {}", e);
                BarIngesterError::Network(e.to_string())
            })?
            .json()
            .await
            .map_err(|e| {
                eprintln!("  [Binance] JSON parse error: {}", e);
                BarIngesterError::Parse(e.to_string())
            })?;

        if response.is_empty() {
            eprintln!("  [Binance] response is empty array");
        } else {
            eprintln!("  [Binance] got {} klines from API", response.len());
        }

        let num_klines = response.len();
        let bars: Vec<Bar> = response.into_iter()
            .filter_map(|arr| {
                if arr.len() < 10 {
                    eprintln!("  [Binance] arr len {} < 10, skipping", arr.len());
                    return None;
                }
                let open_time_ms: u64 = serde_json::from_value(arr[0].clone()).ok()?;
                let open: f64 = match arr[1].clone() {
                    serde_json::Value::String(s) => s.parse().unwrap_or_default(),
                    serde_json::Value::Number(n) => n.as_f64().unwrap_or_default(),
                    v => { eprintln!("  [Binance] field 1 type {:?}", v); return None; }
                };
                let high: f64 = match &arr[2] {
                    serde_json::Value::String(s) => s.parse().unwrap_or_default(),
                    serde_json::Value::Number(n) => n.as_f64().unwrap_or_default(),
                    v => { eprintln!("  [Binance] field 2 type {:?}", v); return None; }
                };
                let low: f64 = match &arr[3] {
                    serde_json::Value::String(s) => s.parse().unwrap_or_default(),
                    serde_json::Value::Number(n) => n.as_f64().unwrap_or_default(),
                    v => { eprintln!("  [Binance] field 3 type {:?}", v); return None; }
                };
                let close: f64 = match &arr[4] {
                    serde_json::Value::String(s) => s.parse().unwrap_or_default(),
                    serde_json::Value::Number(n) => n.as_f64().unwrap_or_default(),
                    v => { eprintln!("  [Binance] field 4 type {:?}", v); return None; }
                };
                let volume: f64 = match &arr[5] {
                    serde_json::Value::String(s) => s.parse().unwrap_or_default(),
                    serde_json::Value::Number(n) => n.as_f64().unwrap_or_default(),
                    v => { eprintln!("  [Binance] field 5 type {:?}", v); return None; }
                };
                let quote_vol: f64 = match &arr[8] {
                    serde_json::Value::String(s) => s.parse().unwrap_or_default(),
                    serde_json::Value::Number(n) => n.as_f64().unwrap_or_default(),
                    v => { eprintln!("  [Binance] field 8 type {:?}", v); return None; }
                };
                let num_trades: u64 = match &arr[9] {
                    serde_json::Value::String(s) => s.parse().unwrap_or_default(),
                    serde_json::Value::Number(n) => n.as_u64().unwrap_or_default(),
                    v => { eprintln!("  [Binance] field 9 type {:?}", v); return None; }
                };

                let vwap = if volume > 0.0 { quote_vol / volume } else { close };
                let ts_event = open_time_ms * 1_000_000;

                Some(Bar::from_floats(
                    ts_event, open, high, low, close, vwap,
                    volume, volume * 0.6, volume * 0.4,
                    num_trades as u32, self.decimal_precision,
                ))
            })
            .collect();

        eprintln!("  [Binance] parsed {} bars from {} klines", bars.len(), num_klines);
        Ok(bars)
    }

    /// Binance fetch using startTime only (no endTime) — lets Binance cap at 1000 bars
    async fn fetch_binance_start_only(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: u64,
    ) -> Result<Vec<Bar>, BarIngesterError> {
        let base_url = match self.instrument_type {
            InstrumentType::Spot => "https://api.binance.com/api/v3/klines",
            InstrumentType::Futures => "https://api.binance.com/fapi/v3/klines",
            InstrumentType::Inverse => return Err(BarIngesterError::Network("Binance does not support inverse futures via public klines API".to_string())),
        };
        let url = format!(
            "{}?symbol={}&interval={}&startTime={}&limit=1000",
            base_url,
            symbol.to_uppercase(),
            interval,
            start_ms,
        );

        let response: Vec<Vec<serde_json::Value>> = self.client
            .get(&url)
            .send()
            .await
            .map_err(|e| BarIngesterError::Network(e.to_string()))?
            .json()
            .await
            .map_err(|e| BarIngesterError::Parse(e.to_string()))?;

        if response.is_empty() {
            eprintln!("  [Binance] response is empty");
            return Ok(Vec::new());
        }

        let num_klines = response.len();
        let bars: Vec<Bar> = response.into_iter()
            .filter_map(|arr| {
                if arr.len() < 10 { return None; }
                let open_time_ms: u64 = serde_json::from_value(arr[0].clone()).ok()?;
                let open: f64 = match arr[1].clone() {
                    serde_json::Value::String(s) => s.parse().unwrap_or_default(),
                    serde_json::Value::Number(n) => n.as_f64().unwrap_or_default(),
                    _ => return None,
                };
                let high: f64 = match &arr[2] {
                    serde_json::Value::String(s) => s.parse().unwrap_or_default(),
                    serde_json::Value::Number(n) => n.as_f64().unwrap_or_default(),
                    _ => return None,
                };
                let low: f64 = match &arr[3] {
                    serde_json::Value::String(s) => s.parse().unwrap_or_default(),
                    serde_json::Value::Number(n) => n.as_f64().unwrap_or_default(),
                    _ => return None,
                };
                let close: f64 = match &arr[4] {
                    serde_json::Value::String(s) => s.parse().unwrap_or_default(),
                    serde_json::Value::Number(n) => n.as_f64().unwrap_or_default(),
                    _ => return None,
                };
                let volume: f64 = match &arr[5] {
                    serde_json::Value::String(s) => s.parse().unwrap_or_default(),
                    serde_json::Value::Number(n) => n.as_f64().unwrap_or_default(),
                    _ => return None,
                };
                let quote_vol: f64 = match &arr[8] {
                    serde_json::Value::String(s) => s.parse().unwrap_or_default(),
                    serde_json::Value::Number(n) => n.as_f64().unwrap_or_default(),
                    _ => 0.0,
                };
                let num_trades: u64 = match &arr[9] {
                    serde_json::Value::String(s) => s.parse().unwrap_or_default(),
                    serde_json::Value::Number(n) => n.as_u64().unwrap_or_default(),
                    _ => 0,
                };
                let vwap = if volume > 0.0 { quote_vol / volume } else { close };
                let ts_event = open_time_ms * 1_000_000;
                Some(Bar::from_floats(
                    ts_event, open, high, low, close, vwap,
                    volume, volume * 0.6, volume * 0.4,
                    num_trades as u32, self.decimal_precision,
                ))
            })
            .collect();

        eprintln!("  [Binance] parsed {} bars from {} klines", bars.len(), num_klines);
        Ok(bars)
    }

    /// Bybit fetch using start only (no end) — lets Bybit return up to 1000 bars
    async fn fetch_bybit_start_only(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: u64,
    ) -> Result<Vec<Bar>, BarIngesterError> {
        let category = match self.instrument_type {
            InstrumentType::Spot => "spot",
            InstrumentType::Futures => "linear",
            InstrumentType::Inverse => "inverse",
        };
        let url = format!(
            "https://api.bybit.com/v5/market/kline?category={}&symbol={}&interval={}&start={}&limit=1000",
            category,
            symbol.to_uppercase(),
            interval,
            start_ms,
        );

        #[derive(Deserialize)]
        struct BybitResponse { result: BybitResult }
        #[derive(Deserialize)]
        struct BybitResult { list: Vec<Vec<serde_json::Value>> }

        let resp: BybitResponse = self.client
            .get(&url)
            .send()
            .await
            .map_err(|e| BarIngesterError::Network(e.to_string()))?
            .json()
            .await
            .map_err(|e| BarIngesterError::Parse(e.to_string()))?;

        let bars: Vec<Bar> = resp.result.list.into_iter()
            .filter_map(|arr| {
                if arr.len() < 7 { return None; }
                let start_time_ms: u64 = match &arr[0] {
                    serde_json::Value::String(s) => s.parse().unwrap_or(0),
                    serde_json::Value::Number(n) => n.as_u64().unwrap_or(0),
                    _ => return None,
                };
                let open: f64 = arr[1].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let high: f64 = arr[2].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let low: f64 = arr[3].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let close: f64 = arr[4].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let volume: f64 = arr[5].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let turnover: f64 = arr[6].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let vwap = if volume > 0.0 { turnover / volume } else { close };
                let ts_event = start_time_ms * 1_000_000;
                Some(Bar::from_floats(
                    ts_event, open, high, low, close, vwap,
                    volume, volume * 0.6, volume * 0.4,
                    1, self.decimal_precision,
                ))
            })
            .collect();

        eprintln!("  [Bybit] parsed {} bars", bars.len());
        Ok(bars)
    }

    async fn fetch_bybit(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: u64,
        end_ms: u64,
    ) -> Result<Vec<Bar>, BarIngesterError> {
        let category = match self.instrument_type {
            InstrumentType::Spot => "spot",
            InstrumentType::Futures => "linear",
            InstrumentType::Inverse => "inverse",
        };
        let url = format!(
            "https://api.bybit.com/v5/market/kline?category={}&symbol={}&interval={}&start={}&end={}&limit=1000",
            category,
            symbol.to_uppercase(),
            interval,
            start_ms,
            end_ms,
        );

        #[derive(Deserialize)]
        struct BybitResponse { result: BybitResult }
        #[derive(Deserialize)]
        struct BybitResult { list: Vec<Vec<serde_json::Value>> }

        let resp: BybitResponse = self.client
            .get(&url)
            .send()
            .await
            .map_err(|e| BarIngesterError::Network(e.to_string()))?
            .json()
            .await
            .map_err(|e| BarIngesterError::Parse(e.to_string()))?;

        let bars: Vec<Bar> = resp.result.list.into_iter()
            .filter_map(|arr| {
                if arr.len() < 7 { return None; }
                let start_time_ms: u64 = match &arr[0] {
                    serde_json::Value::String(s) => s.parse().unwrap_or(0),
                    serde_json::Value::Number(n) => n.as_u64().unwrap_or(0),
                    _ => return None,
                };
                let open: f64 = arr[1].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let high: f64 = arr[2].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let low: f64 = arr[3].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let close: f64 = arr[4].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let volume: f64 = arr[5].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let turnover: f64 = arr[6].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let vwap = if volume > 0.0 { turnover / volume } else { close };
                let ts_event = start_time_ms * 1_000_000;
                Some(Bar::from_floats(
                    ts_event, open, high, low, close, vwap,
                    volume, volume * 0.6, volume * 0.4,
                    1, self.decimal_precision,
                ))
            })
            .collect();

        eprintln!("  [Bybit] parsed {} bars", bars.len());
        Ok(bars)
    }

    async fn fetch_okx_before(
        &self,
        _symbol: &str,
        interval: &str,
        before_ms: u64,
    ) -> Result<Vec<Bar>, BarIngesterError> {
        let inst_id = self.format_inst_id(_symbol);
        // OKX: 'before' parameter gets data from newest down to 'before' timestamp (exclusive)
        // OKX returns data newest first when using 'before'
        // We paginate by using 'before' with the oldest timestamp we've seen
        let url = format!(
            "https://www.okx.com/api/v5/market/candles?instId={}&bar={}&before={}&limit=1000",
            inst_id,
            interval,
            before_ms,
        );

        eprintln!("  [OKX] fetching before={}", before_ms);

        #[derive(Deserialize)]
        struct OkxResponse {
            #[serde(rename = "data")]
            data: Vec<Vec<serde_json::Value>>,
            #[serde(rename = "code")]
            code: String,
            #[serde(rename = "msg")]
            msg: String,
        }

        let resp: OkxResponse = self.client
            .get(&url)
            .send()
            .await
            .map_err(|e| {
                eprintln!("  [OKX] HTTP error: {}", e);
                BarIngesterError::Network(e.to_string())
            })?
            .json()
            .await
            .map_err(|e| {
                eprintln!("  [OKX] JSON error: {}", e);
                BarIngesterError::Parse(e.to_string())
            })?;

        if resp.code != "0" {
            eprintln!("  [OKX] API error {}: {}", resp.code, resp.msg);
        }
        eprintln!("  [OKX] API returned {} items", resp.data.len());

        let bars: Vec<Bar> = resp.data.into_iter()
            .filter_map(|arr| {
                if arr.len() < 7 {
                    return None;
                }
                let ts_ms_str = arr[0].as_str()?;
                let ts_event = ts_ms_str.parse::<u64>().ok()? * 1_000_000;
                let open: f64 = arr[1].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let high: f64 = arr[2].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let low: f64 = arr[3].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let close: f64 = arr[4].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let volume: f64 = arr[5].as_str().and_then(|s| s.parse().ok()).unwrap_or_default();
                let quote_vol: f64 = arr.get(6).and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0);

                let vwap = if volume > 0.0 { quote_vol / volume } else { close };

                Some(Bar::from_floats(
                    ts_event, open, high, low, close, vwap,
                    volume, volume * 0.6, volume * 0.4,
                    1, self.decimal_precision,
                ))
            })
            .collect();

        eprintln!("  [OKX] parsed {} bars", bars.len());
        Ok(bars)
    }

    /// Format symbol into exchange-specific instId for OKX.
    ///
    /// Spot:     BTC-USDT  → BTC-USDT
    /// Futures:  BTC-USDT  → BTC-USDT-SWAP
    fn format_inst_id(&self, symbol: &str) -> String {
        // Normalize symbol: e.g. "BTCUSDT" → "BTC-USDT"
        let normalized = normalize_okx_symbol(symbol);
        match self.instrument_type {
            InstrumentType::Spot => normalized,
            InstrumentType::Futures => format!("{}-SWAP", normalized),
            InstrumentType::Inverse => normalized, // inverse uses same format as spot
        }
    }

    /// Write a batch of bars to a yearly TVCB file.
    fn write_yearly_file(&self, bars: &[Bar], path: &Path) -> Result<(), BarIngesterError> {
        if bars.is_empty() {
            return Ok(());
        }

        let year = bars.first().map(|b| {
            let secs = b.ts_event / 1_000_000_000;
            let nd = chrono::DateTime::from_timestamp(secs as i64, 0)
                .map(|dt| dt.naive_utc().date().year())
                .unwrap_or(2024);
            nd
        }).unwrap_or(2024) as u64;

        let instrument_hash = fnv1a_hash(path.file_stem().and_then(|s| s.to_str()).unwrap_or(""));
        let mut writer = TvcbWriter::new(
            path,
            instrument_hash,
            self.anchor_interval,
            self.decimal_precision,
            year,
            self.timeframe_ns,
        ).map_err(|e| BarIngesterError::Write(e.to_string()))?;

        for bar in bars {
            writer.write_bar(bar)
                .map_err(|e| BarIngesterError::Write(e.to_string()))?;
        }

        writer.finalize()
            .map_err(|e| BarIngesterError::Write(e.to_string()))?;

        Ok(())
    }

    /// Compute anchor interval for a given timeframe.
    ///
    /// 15m bars → anchor every 10 bars
    /// 1h bars → anchor every 10 bars
    /// 1d bars → anchor every 10 bars
    fn compute_anchor_interval(timeframe_ns: u64) -> u32 {
        let bars_per_day = 86_400_000_000_000u64 / timeframe_ns;
        // Anchor every ~10 bars (regardless of timeframe)
        10u32
    }
}

// =============================================================================
// Error types
// =============================================================================

#[derive(Debug)]
pub enum BarIngesterError {
    Network(String),
    Parse(String),
    Write(String),
    NoData(String),
}

impl std::fmt::Display for BarIngesterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BarIngesterError::Network(s) => write!(f, "network error: {}", s),
            BarIngesterError::Parse(s) => write!(f, "parse error: {}", s),
            BarIngesterError::Write(s) => write!(f, "write error: {}", s),
            BarIngesterError::NoData(s) => write!(f, "no data for {}", s),
        }
    }
}

impl From<std::io::Error> for BarIngesterError {
    fn from(e: std::io::Error) -> Self {
        BarIngesterError::Write(e.to_string())
    }
}

impl std::error::Error for BarIngesterError {}

// =============================================================================
// Utilities
// =============================================================================

/// Convert timeframe string to nanoseconds.
pub fn timeframe_to_ns(tf: &str) -> u64 {
    match tf {
        "1m" => 60 * 1_000_000_000,
        "5m" => 300 * 1_000_000_000,
        "15m" => 900 * 1_000_000_000,
        "1h" => 3_600 * 1_000_000_000,
        "4h" => 14_400 * 1_000_000_000,
        "1d" => 86_400 * 1_000_000_000,
        _ => 900 * 1_000_000_000,
    }
}

/// Convert nanoseconds to timeframe string (best-effort).
pub fn ns_to_tf(ns: u64) -> String {
    match ns {
        60_000_000_000 => "1m".to_string(),
        300_000_000_000 => "5m".to_string(),
        900_000_000_000 => "15m".to_string(),
        3_600_000_000_000 => "1h".to_string(),
        14_400_000_000_000 => "4h".to_string(),
        86_400_000_000_000 => "1d".to_string(),
        _ => format!("{}ns", ns),
    }
}

/// Convert NaiveDate to milliseconds since epoch.
fn date_to_millis(date: NaiveDate) -> u64 {
    date.and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis() as u64
}

/// FNV-1a 32-bit hash for instrument ID.
fn fnv1a_hash(s: &str) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for byte in s.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

/// Normalize a symbol for OKX instId formatting.
///
/// Strips common quote suffixes and inserts a dash:
/// "BTCUSDT" → "BTC-USDT"
/// "ETHUSDT" → "ETH-USDT"
fn normalize_okx_symbol(symbol: &str) -> String {
    // Try to split common quote currencies
    let quote_suffixes = ["USDT", "USDC", "BTC", "ETH", "USD", "BNB"];
    for suffix in quote_suffixes {
        if let Some(stripped) = symbol.strip_suffix(suffix) {
            if !stripped.is_empty() {
                return format!("{}-{}", stripped, suffix);
            }
        }
    }
    // Fallback: just insert dash in the middle if length > 3
    let mid = symbol.len() / 2;
    format!("{}-{}", &symbol[..mid], &symbol[mid..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timeframe_to_ns() {
        assert_eq!(timeframe_to_ns("15m"), 900_000_000_000);
        assert_eq!(timeframe_to_ns("1h"), 3_600_000_000_000);
        assert_eq!(timeframe_to_ns("1d"), 86_400_000_000_000);
    }

    #[test]
    fn test_ns_to_tf() {
        assert_eq!(ns_to_tf(900_000_000_000), "15m");
        assert_eq!(ns_to_tf(3_600_000_000_000), "1h");
    }

    #[test]
    fn test_anchor_interval() {
        assert_eq!(BarIngester::compute_anchor_interval(900_000_000_000), 10);
        assert_eq!(BarIngester::compute_anchor_interval(60_000_000_000), 10);
    }

    #[test]
    fn test_fnv1a_hash() {
        let h = fnv1a_hash("BTCUSDT");
        assert_ne!(h, 0);
    }
}