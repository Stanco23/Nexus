//! Exchange ingestion layer — file-based and live market data ingestion.
//!
//! # File-based ingestion
//! Use `BinanceFileIngestor` to download/parse Binance Data Archive CSV files
//! and write them directly to TVC3 format.
//!
//! # Architecture
//! ```text
//! BinanceFileIngestor  /  BinanceHttpIngestor
//!       |
//!       v
//!   Parsed BinanceTradeRow
//!       |
//!       v
//!   TradeTick (nano-integer)
//!       |
//!       v
//!   TvcWriter → TVC3 file
//! ```
//!
//! # Usage — File-based (offline)
//! ```ignore
//! // Download from Binance Data Archive and convert to TVC3:
//! cargo run -p nexus --bin ingest -- \
//!     --exchange binance \
//!     --symbol BTCUSDT \
//!     --input ./data \
//!     --output ./tvc_data \
//!     --precision 9
//! ```
//!
//! # Usage — Live
//! See `adapters::BinanceAdapter` for WebSocket streaming ingestion.

pub mod adapters;
pub mod binance_file;

pub use binance_file::{
    BinanceFileIngestor, GenericCsvIngestor, BinanceTradeRow, IngestError,
    IngestResult,
};
pub use adapters::{BinanceAdapter, BinanceVenue, NormalizedTick};
