//! Nexus core backtesting engine.
//!
//! High-performance tick-by-tick backtesting with:
//! - Ring buffer for zero-copy TVC file access
//! - Pre-decoded TickBuffer with VPIN bucketing
//! - Multi-instrument portfolio support
//! - Parameter sweeps via rayon parallelism
//! - Monte Carlo + Walk-Forward analysis

pub mod actor;
pub mod book;
pub mod buffer;
pub mod cache;
pub mod calibrate;
pub mod catalog;
pub mod data;
pub mod database;
pub mod engine;
pub mod ingestion;
pub mod instrument;
pub mod live;
pub mod messages;
pub mod mc_wf;
pub mod optim;
pub mod paper;
pub mod portfolio;
pub mod signals;
pub mod slippage;
pub mod strategy_ctx;
pub mod sweep;

pub use database::{Database, DatabaseError, SqliteDatabase, MemoryDatabase};
