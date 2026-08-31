//! Strategy trait re-export.
//!
//! The canonical `Strategy` trait lives in `nexus_strategy::strategy_trait`.
//! This module re-exports it so `nexus` live code can use it without a circular dep.

pub use nexus_strategy::strategy_trait::Strategy;
