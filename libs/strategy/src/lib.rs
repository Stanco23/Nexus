//! Nexus strategy trait and example strategies.

pub mod context;
pub mod strategy_trait;

pub use context::StrategyCtx;
pub use strategy_trait::{Action, BacktestMode, Bar, Signal, Strategy, Tick};
