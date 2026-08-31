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
pub mod data_manager;
pub mod database;
pub mod backtest;
pub mod runner;
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
pub mod strategy_trait;
pub mod sweep;
pub mod trader;

/// Re-exports from nexus-types (shared types with nexus-strategy).
pub mod types {
    pub use nexus_types::{
        Bar, InstrumentId, OmsType, Order, OrderSide, OrderType, ParameterSchema,
        ParameterType, ParameterValue, PositionId, PositionSide, Signal, StrategyId, Tick,
    };
}

// Re-exports from nexus-strategy for strategy authoring
pub use nexus_strategy::{Strategy, StrategyCtx};

pub use database::{Database, DatabaseError, SqliteDatabase, MemoryDatabase};
pub use engine::core::Signal;
pub use nexus_types::{OrderSide, OrderType, PositionSide};

// Re-export BacktestEngine and BacktestResult from backtest engine module
pub use backtest::engine::{BacktestEngine, BacktestError, BacktestResult};

// Re-export BacktestMode and Runner trait from runner module
pub use runner::{BacktestMode, Runner};
