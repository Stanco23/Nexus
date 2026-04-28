//! DataManager — orchestrates catalog, download, TVC3 build, and load.
//!
//! Primary entry point: `DataManager::load(config)` → `TickBufferSet`
//!
//! ```
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
//! let dm = DataManager::new(PathBuf::from("/data"));
//! let buffer_set = dm.load(config)?;
//! let mut cursor = buffer_set.merge_cursor();
//! // → pass to portfolio.run_portfolio()
//! ```
//!
//! Flow per date:
//! 1. Check catalog for existing TVC3 file
//! 2. If miss and `download_on_miss`: call downloader → tvc_builder → write file
//! 3. Load file into TickBuffer via DataLoader
//! 4. Merge all dates into TickBufferSet

use std::path::PathBuf;
use chrono::NaiveDate;
use crate::data::buffer::TickBufferSet;
use super::catalog::Catalog;
use super::downloader::{Downloader, DownloadError};
use super::loader::{DataLoader, LoadError};
use super::tvc_builder::TvcBuilder;
use super::types::{DataManagerConfig, Exchange, Venue};

/// Main data management struct. Scan once, load many.
#[derive(Debug)]
pub struct DataManager {
    data_root: PathBuf,
    catalog: Catalog,
    downloader: Option<Downloader>,
    tvc_builder: TvcBuilder,
}

impl DataManager {
    /// Create a new DataManager with the given data root.
    /// Scans the folder tree immediately to build the catalog.
    pub fn new(data_root: PathBuf) -> std::io::Result<Self> {
        let catalog = Catalog::scan(data_root.clone())?;
        let tvc_builder = TvcBuilder::new(data_root.clone());
        Ok(Self {
            data_root,
            catalog,
            downloader: None,
            tvc_builder,
        })
    }

    /// Create with a pre-configured downloader (for download-on-miss).
    pub fn with_downloader(data_root: PathBuf, downloader: Downloader) -> std::io::Result<Self> {
        let catalog = Catalog::scan(data_root.clone())?;
        let tvc_builder = TvcBuilder::new(data_root.clone());
        Ok(Self {
            data_root,
            catalog,
            downloader: Some(downloader),
            tvc_builder,
        })
    }

    /// Load data according to config, returning a merged TickBufferSet.
    ///
    /// If `download_on_miss` is true and files are missing, attempts to download
    /// and convert them. If false, returns error on any missing file.
    pub fn load(&self, config: &DataManagerConfig) -> Result<TickBufferSet, DataManagerError> {
        let paths = config.all_tvc_paths();
        let mut loaded_buffers = Vec::new();

        for path in &paths {
            if path.exists() {
                // Load existing file
                match DataLoader::load_single(path, config.exchange, config.venue, &config.symbol) {
                    Ok(buffer_set) => loaded_buffers.push(buffer_set),
                    Err(e) => {
                        eprintln!("WARN: failed to load {:?}: {}", path, e);
                    }
                }
            } else if config.download_on_miss {
                // Download on miss — requires downloader to be configured
                let date = path.file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
                    .unwrap_or(config.start_date);

                self.download_and_build(config.exchange, config.venue, &config.symbol, date)?;
                
                // Retry load after download
                match DataLoader::load_single(path, config.exchange, config.venue, &config.symbol) {
                    Ok(buffer_set) => loaded_buffers.push(buffer_set),
                    Err(e) => return Err(DataManagerError::LoadFailed(e.to_string())),
                }
            } else {
                return Err(DataManagerError::FileNotFound(path.clone()));
            }
        }

        if loaded_buffers.is_empty() {
            return Err(DataManagerError::NoData);
        }

        // Merge all TickBufferSets into one
        let mut merged = TickBufferSet::new();
        for bs in loaded_buffers {
            for id in bs.instrument_ids() {
                if let Some(tb) = bs.get(&id) {
                    merged.add_buffer(id, (*tb).clone())?;
                }
            }
        }

        Ok(merged)
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
            .ok_or_else(|| DataManagerError::NoDownloader)?;

        let data = downloader.download(exchange, venue, symbol, date)
            .map_err(|e| DataManagerError::DownloadFailed(e.to_string()))?;

        self.tvc_builder.build(data)
            .map_err(|e| DataManagerError::BuildFailed(e.to_string()))?;

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
}

impl std::fmt::Display for DataManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataManagerError::FileNotFound(p) => write!(f, "TVC3 file not found: {:?}", p),
            DataManagerError::NoData => write!(f, "No TVC3 data available"),
            DataManagerError::NoDownloader => write!(f, "No downloader configured — set download_on_miss=false or provide a downloader"),
            DataManagerError::DownloadFailed(msg) => write!(f, "Download failed: {}", msg),
            DataManagerError::BuildFailed(msg) => write!(f, "TVC3 build failed: {}", msg),
            DataManagerError::LoadFailed(msg) => write!(f, "Load failed: {}", msg),
        }
    }
}

impl std::error::Error for DataManagerError {}