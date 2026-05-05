//! Shared types for strategy definitions.
//!
//! Re-exports everything from `nexus-types` so strategies can import
//! all types from a single crate.

pub use nexus_types::{
    BacktestMode, Bar, InstrumentId, Order, OrderHandle, OrderSide, OrderType,
    OmsType, ParameterSchema, ParameterType, ParameterValue, PositionId, PositionSide,
    Signal, StrategyId, Tick,
};
