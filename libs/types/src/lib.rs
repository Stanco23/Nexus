//! Nexus shared types — Signal, InstrumentId, Tick, Bar, PositionSide, Order.
//!
//! These types are shared between the `nexus` engine (backtesting, data, execution)
//! and `nexus-strategy` (strategy trait definitions). Neither crate depends on the other.
//! Both depend on this crate instead.

pub mod types;

pub use types::{
    BacktestMode, Bar, InstrumentId, OmsType, Order, OrderHandle, OrderSide, OrderType,
    ParameterSchema, ParameterType, ParameterValue, PositionId, PositionSide,
    Signal, StrategyId, Tick,
};