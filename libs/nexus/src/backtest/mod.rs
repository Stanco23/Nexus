//! Backtest infrastructure — clean API for running strategy backtests and parameter sweeps.
//!
//! Key concepts:
//! - `BacktestEngine`: Single backtest run with a strategy + date range
//! - `SweepRunner`: Multiple parameter combinations against one loaded dataset
//! - `DataIndex`: Maps instruments to TVC files with timestamp ranges for sweep-aware loading
//! - `Strategy`: Pure signal logic — on_tick() returns Signal, knows nothing about data loading

pub mod capital;
pub mod engine;
pub mod data_index;

pub use data_index::DataIndex;
pub use engine::{BacktestEngine, BacktestError, BacktestResult};
pub use capital::{CapitalSpread, CapitalSpreadError};