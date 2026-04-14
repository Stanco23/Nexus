//! Exchange ingestion layer -- WebSocket adapters for market data ingestion.
//!
//! # Architecture
//! ```text
//! BinanceAdapter / BybitAdapter / OKXAdapter
//!       |
//!       v
//!   Normalized tick stream (TradeTick)
//!       |
//!       v
//!   TvcWriter (with checkpoint every N ticks)
//! ```
//!
//! # Usage
//! ```ignore
//! let adapter = BinanceAdapter::new("BTCUSDT", BinanceVenue::Spot);
//! let writer = TvcWriter::new(path, instrument_id, 1000, 9)?;
//! adapter.stream(|tick| writer.write_tick(tick))?;
//! writer.finalize()?;
//! ```

pub mod adapters;

pub use adapters::{BinanceAdapter, BinanceVenue, NormalizedTick};
