//! Backtest engine — tick-by-tick and bar-mode simulation.
//!
//! Re-exports core types: `BacktestEngine`, `EngineContext`, `Signal`, `Trade`, `Strategy`.

pub mod core;
pub mod orders;
pub mod risk;
pub mod sizing;

pub use core::{
    BacktestEngine, BacktestResult, CommissionConfig, EngineContext, Signal, Strategy, Trade,
};
pub use orders::{Order, OrderManager, OrderSide, OrderType};
pub use risk::{RiskConfig, RiskEngine};
pub use sizing::{Sizing, SizingConfig, SizingMethod};
