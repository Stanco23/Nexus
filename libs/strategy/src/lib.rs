//! Nexus strategy trait and example strategies.

pub mod strategy_trait;
pub mod context;

pub use strategy_trait::{Strategy, BacktestMode, Signal, Tick, Bar, Action};
pub use context::StrategyCtx;
