//! Exchange-agnostic historical data manager.
//!
//! Loads TVC3 files into `RingBuffer` for backtesting. Handles catalog management,
//! download-on-miss, TVC3 conversion, and structured folder layout.
//!
//! # Folder Structure
//! ```
//! data/
//!   binance/
//!     spot/
//!       BTCUSDT/
//!         2025-01-01.tvc
//!         2025-01-02.tvc
//!   bybit/
//!     spot/
//!       BTCUSDT/
//!         2025-01-01.tvc
//! ```
//!
//! # Core Flow
//! ```
//! DataManager::load(config) → RingBuffer
//!       |
//!       v
//!   catalog.check(symbol, date)
//!       |
//!       v (miss)
//!   downloader.download(exchange, symbol, date_range)
//!       |
//!       v
//!   tvc_builder.build(exchange, raw_data) → TVC3 file
//!       |
//!       v
//!   loader.load_into_buffer(path) → RingBuffer
//! ```
//!
//! # Exchange Data Sources
//! - **Binance**: Historical Data Archive (CSV ZIP)
//! - **Bybit**: HTTP klines + trade APIs
//! - **OKX**: HTTP klines + trade APIs
//! - **Coinbase**: HTTP product candles + trades

pub mod bar_ingester;
pub mod catalog;
pub mod data_manager;
pub mod downloader;
pub mod downloaders;
pub mod loader;
#[macro_use]
pub mod macros;
pub mod tvc_builder;
pub mod types;

pub use bar_ingester::{BarIngester, ExchangeKind, InstrumentType};
pub use catalog::Catalog;
pub use data_manager::DataManager;
pub use downloader::Downloader;
pub use downloaders::{BinanceDownloader, BybitDownloader};
pub use loader::DataLoader;
pub use tvc_builder::TvcBuilder;
pub use types::{DataManagerConfig, Exchange, Venue};