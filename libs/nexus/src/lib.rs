//! Nexus core backtesting engine.
//!
//! High-performance tick-by-tick backtesting with:
//! - Ring buffer for zero-copy TVC file access
//! - Pre-decoded TickBuffer with VPIN bucketing
//! - Multi-instrument portfolio support
//! - Parameter sweeps via rayon parallelism

pub mod buffer;
pub mod engine;
pub mod slippage;
pub mod portfolio;
pub mod sweep;
