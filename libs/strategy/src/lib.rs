//! Nexus strategy trait and example strategies.

pub mod context;
pub mod examples;
pub mod strategy_trait;
pub mod types;

pub use context::StrategyCtx;
pub use strategy_trait::Strategy;
pub use types::{
    BacktestMode, Bar, ParameterSchema, ParameterType, ParameterValue, PositionSide, Tick,
};
