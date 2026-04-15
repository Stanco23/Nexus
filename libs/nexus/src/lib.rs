//! Nexus core backtesting engine.
//!
//! High-performance tick-by-tick backtesting with:
//! - Ring buffer for zero-copy TVC file access
//! - Pre-decoded TickBuffer with VPIN bucketing
//! - Multi-instrument portfolio support
//! - Parameter sweeps via rayon parallelism
//! - Monte Carlo + Walk-Forward analysis

pub mod book;
pub mod buffer;
pub mod catalog;
pub mod engine;
pub mod ingestion;
pub mod instrument;
pub mod mc_wf;
pub mod portfolio;
pub mod slippage;
pub mod sweep;
