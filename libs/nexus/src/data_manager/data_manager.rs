//! DataManager — orchestrates catalog, download, TVC3 build, and load.
//!
//! Primary entry point: `DataManager::load_ring_buffer_set(config)` → `RingBufferSet`
//!
//! ```ignore
//! let config = DataManagerConfig {
//!     data_root: PathBuf::from("/data"),
//!     exchange: Exchange::Binance,
//!     venue: Venue::Spot,
//!     symbol: "BTCUSDT".into(),
//!     start_date: NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
//!     end_date: NaiveDate::from_ymd_opt(2025, 1, 7).unwrap(),
//!     download_on_miss: true,
//! };
//!
//! let dm = DataManager::with_downloader(data_dir, downloader)?;
//! let ring_set = dm.load_ring_buffer_set(&config)?;
//! // Iterate:
//! let state = ring_set.iter_state_from_global_tick(0);
//! while let Some(event) = state.next() {
//!     // handle tick
//! }
//! ```
//!
//! Flow per date:
//! 1. Check catalog for existing TVC3 file
//! 2. If miss and `download_on_miss`: call downloader → tvc_builder → write file
//! 3. Load file via RingBuffer (mmap + on-the-fly delta decompression)
//! 4. Merge all dates into RingBufferSet

use std::path::{Path, PathBuf};
use chrono::{NaiveDate, Datelike};
use crate::buffer::buffer_set::{RingBufferSet, TickBufferSet};
use crate::buffer::RingBuffer;
use crate::instrument::InstrumentId;
use super::catalog::Catalog;
use super::downloader::Downloader;
use super::types::{DataManagerConfig, Exchange, Venue};
use super::bar_ingester::{BarIngester, ExchangeKind, InstrumentType};
use tvc::tvcb::reader::BarIter;

/// Main data management struct. Scan once, load many.
pub struct DataManager {
    data_root: PathBuf,
    catalog: Catalog,
    downloader: Option<Downloader>,
}

impl DataManager {
    /// Create a new DataManager with the given data root.
    /// Scans the folder tree immediately to build the catalog.
    pub fn new(data_root: PathBuf) -> std::io::Result<Self> {
        let catalog = Catalog::scan(data_root.clone())?;
        Ok(Self {
            data_root,
            catalog,
            downloader: None,
        })
    }

    /// Create with a pre-configured downloader (for download-on-miss).
    pub fn with_downloader(data_root: PathBuf, downloader: Downloader) -> std::io::Result<Self> {
        let catalog = Catalog::scan(data_root.clone())?;
        Ok(Self {
            data_root,
            catalog,
            downloader: Some(downloader),
        })
    }

    /// Create with a default Binance downloader (auto-downloads missing data).
    pub fn with_default_downloader(data_root: PathBuf) -> std::io::Result<Self> {
        let downloader = Downloader::new();
        let mut dl = downloader;
        dl.register(crate::data_manager::downloaders::binance::BinanceDownloader::new());
        let catalog = Catalog::scan(data_root.clone())?;
        Ok(Self {
            data_root,
            catalog,
            downloader: Some(dl),
        })
    }

    /// Load data as a `RingBufferSet` with optional download-on-miss.
    ///
    /// This is the primary method for backtesting — it returns a `RingBufferSet`
    /// which does on-the-fly delta decompression (memory-efficient for large files).
    ///
    /// If `download_on_miss` is true in config, missing files are downloaded
    /// via the registered `BinanceDownloader` (or other sources) and converted to
    /// proper TVC3 format using `TvcBuilder`.
    ///
    /// RingBufferSet stores all files in a Vec (not HashMap) to support multi-file
    /// same-instrument backtests (e.g. multiple trading days for BTCUSDT).
    pub fn load_ring_buffer_set(
        &self,
        config: &DataManagerConfig,
    ) -> Result<RingBufferSet, DataManagerError> {
        let paths = config.all_tvc_paths();
        let mut files_to_load: Vec<PathBuf> = Vec::new();

        for path in &paths {
            if !path.exists() {
                if !config.download_on_miss {
                    return Err(DataManagerError::FileNotFound(path.clone()));
                }

                // Download and build TVC3
                let date = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
                    .unwrap_or(config.start_date);

                self.download_and_build(config.exchange, config.venue, &config.symbol, date)?;
            }
            files_to_load.push(path.clone());
        }

        if files_to_load.is_empty() {
            return Err(DataManagerError::NoData);
        }

        // Build file list with instrument IDs
        let instrument_id = InstrumentId::new(&config.symbol, config.exchange.as_str());
        let files: Vec<_> = files_to_load
            .into_iter()
            .map(|p| (p, instrument_id.clone()))
            .collect();

        // Load as RingBufferSet — on-the-fly delta decompression
        RingBufferSet::from_files(files).map_err(|e| DataManagerError::LoadFailed(e.to_string()))
    }

    /// Download data for a specific exchange/symbol/date and write TVC3.
    fn download_and_build(
        &self,
        exchange: Exchange,
        venue: Venue,
        symbol: &str,
        date: NaiveDate,
    ) -> Result<(), DataManagerError> {
        let downloader = self.downloader
            .as_ref()
            .ok_or(DataManagerError::NoDownloader)?;

        use crate::data_manager::tvc_builder::TvcBuilder;

        let data = downloader
            .download(exchange, venue, symbol, date)
            .map_err(|e| DataManagerError::DownloadFailed(e.to_string()))?;

        let builder = TvcBuilder::new(self.data_root.clone());
        builder
            .build(data)
            .map_err(|e| format!("{}", e))  // e is BuildError which has Debug but not Display
            .map_err(DataManagerError::BuildFailed)?;

        Ok(())
    }

    /// Rescan the catalog (e.g. after adding new files).
    pub fn rescan(&mut self) -> std::io::Result<()> {
        self.catalog = Catalog::scan(self.data_root.clone())?;
        Ok(())
    }

    /// Check if a specific file exists in the catalog.
    pub fn exists(&self, exchange: Exchange, venue: Venue, symbol: &str, date: NaiveDate) -> bool {
        self.catalog.contains(exchange, venue, symbol, date)
    }

    /// List all missing dates for a given config's date range.
    pub fn missing_dates(&self, config: &DataManagerConfig) -> Vec<NaiveDate> {
        self.catalog.missing_dates(
            config.exchange,
            config.venue,
            &config.symbol,
            config.start_date,
            config.end_date,
        )
    }

    /// Load data as a `TickBufferSet` for multi-instrument backtesting.
    ///
    /// Each instrument is loaded separately from its symbol TVC files.
    /// A `MergeCursor` from the returned `TickBufferSet` delivers ticks
    /// in time-order across all instruments.
    ///
    /// # Arguments
    /// * `instruments` — Vec of (symbol, exchange)) pairs for multi-symbol load
    pub fn load_tick_buffer_set(
        &self,
        instruments: &[(String, Exchange)],
        start_date: NaiveDate,
        end_date: NaiveDate,
    ) -> Result<TickBufferSet, DataManagerError> {
        let mut all_files: Vec<(PathBuf, InstrumentId)> = Vec::new();

        for (symbol, exchange) in instruments {
            let symbol_str = symbol.to_string();
            // Collect all TVC paths for this symbol within the date range
            let mut current = start_date;
            while current <= end_date {
                let path = self.data_root
                    .join(exchange.as_str())
                    .join(Venue::Spot.as_str())
                    .join(&symbol_str)
                    .join(format!("{}.tvc", current));

                if path.exists() {
                    let iid = InstrumentId::new(&symbol_str, exchange.as_str());
                    all_files.push((path, iid));
                }
                current = current.succ_opt().unwrap_or(current);
            }
        }

        if all_files.is_empty() {
            return Err(DataManagerError::NoData);
        }

        TickBufferSet::from_files(all_files)
            .map_err(|e| DataManagerError::LoadFailed(e.to_string()))
    }

    /// Ingest bars from exchange → TVCB files for a given symbol/timeframe.
    ///
    /// Fetches OHLCV data from the configured exchange and writes yearly TVCB files.
    /// Returns paths to all created files.
    pub async fn ingest_bars(
        &self,
        exchange: Exchange,
        instrument_type: InstrumentType,
        symbol: &str,
        timeframe: &str,
        start_date: NaiveDate,
        end_date: NaiveDate,
    ) -> Result<Vec<PathBuf>, DataManagerError> {
        use super::bar_ingester::timeframe_to_ns;

        // Map Exchange enum to ExchangeKind
        let exchange_kind = match exchange {
            Exchange::Binance => ExchangeKind::Binance,
            Exchange::Bybit => ExchangeKind::Bybit,
            Exchange::Okx => ExchangeKind::Okx,
            Exchange::Coinbase => return Err(DataManagerError::LoadFailed("Coinbase not yet supported".to_string())),
        };

        let timeframe_ns = timeframe_to_ns(timeframe);
        let ingester = BarIngester::new(exchange_kind, instrument_type, timeframe_ns);

        ingester.ingest(symbol, start_date, end_date, &self.data_root)
            .await
            .map_err(|e| DataManagerError::LoadFailed(e.to_string()))
    }

    /// Load bars from TVCB files as a `BarIter` for backtesting.
    ///
    /// Finds all TVCB files for the given exchange/instrument_type/symbol/timeframe covering
    /// the date range and returns a `BarIter` that yields bars in time order.
    ///
    /// File naming: `{data_dir}/{exchange}/{instrument_type}/{symbol}/{timeframe}/{year}.tvcb`
    ///
    /// If files are missing, auto-ingests bars from the exchange (blocking — may take
    /// minutes for large date ranges). Use `load_bars_with_option(download_on_miss=false)`
    /// to disable auto-ingestion.
    pub fn load_bars(
        &self,
        exchange: Exchange,
        instrument_type: InstrumentType,
        symbol: &str,
        timeframe: &str,
        start_date: NaiveDate,
        end_date: NaiveDate,
    ) -> Result<BarIter, DataManagerError> {
        self.load_bars_with_option(exchange, instrument_type, symbol, timeframe, start_date, end_date, true)
    }

    /// Like `load_bars` but with explicit `download_on_miss` control.
    pub fn load_bars_with_option(
        &self,
        exchange: Exchange,
        instrument_type: InstrumentType,
        symbol: &str,
        timeframe: &str,
        start_date: NaiveDate,
        end_date: NaiveDate,
        download_on_miss: bool,
    ) -> Result<BarIter, DataManagerError> {
        let files = self.find_tvcb_files(exchange, instrument_type, symbol, timeframe, start_date, end_date);
        if files.is_empty() {
            if download_on_miss {
                // Auto-ingest: spawn a blocking tokio runtime to run async ingestion
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| DataManagerError::LoadFailed(format!("tokio: {}", e)))?;
                rt.block_on(async {
                    self.ingest_bars(exchange, instrument_type, symbol, timeframe, start_date, end_date).await
                })?;
                // Retry after ingestion
                let files = self.find_tvcb_files(exchange, instrument_type, symbol, timeframe, start_date, end_date);
                if files.is_empty() {
                    return Err(DataManagerError::TvcbFileNotFound(format!(
                        "no TVCB files for {} {} {} [{} to {}] after auto-ingestion",
                        exchange.as_str(), instrument_type.as_str(), symbol, start_date, end_date
                    )));
                }
                let start_ns = date_to_ns(start_date);
                let end_ns = date_to_ns(end_date + chrono::Duration::days(1));
                return BarIter::new(files, start_ns, end_ns)
                    .map_err(|e| DataManagerError::LoadFailed(e.to_string()));
            }
            return Err(DataManagerError::TvcbFileNotFound(format!(
                "no TVCB files for {} {} {} [{} to {}]",
                exchange.as_str(), instrument_type.as_str(), symbol, start_date, end_date
            )));
        }
        let start_ns = date_to_ns(start_date);
        let end_ns = date_to_ns(end_date + chrono::Duration::days(1));
        BarIter::new(files, start_ns, end_ns)
            .map_err(|e| DataManagerError::LoadFailed(e.to_string()))
    }

    /// Get paths for TVCB files covering the date range.
    fn find_tvcb_files(
        &self,
        exchange: Exchange,
        instrument_type: InstrumentType,
        symbol: &str,
        timeframe: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut current = start;
        while current <= end {
            let year = current.year();
            let path = self.data_root
                .join(exchange.as_str())
                .join(instrument_type.as_str())
                .join(symbol.to_lowercase())
                .join(timeframe)
                .join(format!("{}.tvcb", year));
            if path.exists() {
                files.push(path);
            }
            // Advance to next year
            if current.month() == 12 && current.day() == 31 {
                current = NaiveDate::from_ymd_opt(year + 1, 1, 1).unwrap();
            } else {
                current = current.succ_opt().unwrap_or(end);
            }
        }
        files
    }
}

/// Convert NaiveDate to UTC nanoseconds (midnight).
fn date_to_ns(date: NaiveDate) -> u64 {
    // Use UTC midnight to be consistent with TVCB timestamps (UTC).
    // NOTE: When integrating with BarIter, ensure date ranges are chosen
    // so the resulting ns range includes the bar timestamps.
    date.and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_nanos_opt().unwrap_or(0) as u64
}

/// Errors from DataManager operations.
#[derive(Debug)]
pub enum DataManagerError {
    FileNotFound(PathBuf),
    NoData,
    NoDownloader,
    DownloadFailed(String),
    BuildFailed(String),
    LoadFailed(String),
    TvcbFileNotFound(String),
}

impl std::fmt::Display for DataManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataManagerError::FileNotFound(p) => write!(f, "TVC3 file not found: {:?}", p),
            DataManagerError::NoData => write!(f, "No TVC3 data available"),
            DataManagerError::NoDownloader => {
                write!(f, "No downloader configured — set download_on_miss=false or provide a downloader")
            }
            DataManagerError::DownloadFailed(msg) => write!(f, "Download failed: {}", msg),
            DataManagerError::BuildFailed(msg) => write!(f, "TVC3 build failed: {}", msg),
            DataManagerError::LoadFailed(msg) => write!(f, "Load failed: {}", msg),
            DataManagerError::TvcbFileNotFound(msg) => write!(f, "TVCB file not found: {}", msg),
        }
    }
}

impl std::error::Error for DataManagerError {}