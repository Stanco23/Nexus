//! Strategy context trait re-export.
//!
//! The canonical `StrategyCtx` trait lives in `nexus_strategy::context`.
//! This module re-exports it so `nexus` code can use it without a circular dep.

pub use nexus_strategy::context::StrategyCtx;
