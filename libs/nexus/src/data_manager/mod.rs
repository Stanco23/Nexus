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

pub mod catalog;
pub mod downloader;
pub mod loader;
pub mod tvc_builder;
pub mod types;

pub use catalog::Catalog;
pub use downloader::Downloader;
pub use loader::DataLoader;
pub use tvc_builder::TvcBuilder;
pub use types::{DataManagerConfig, Exchange, Venue};