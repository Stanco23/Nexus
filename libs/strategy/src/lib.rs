//! Nexus strategy trait and example strategies.

pub mod context;
pub mod examples;
pub mod indicators;
pub mod signals;
pub mod strategy_trait;
pub mod types;

pub use context::StrategyCtx;
pub use indicators::{
    Atr, Ema, Indicator, Macd, Rsi, Sma, Stochastic, Vwap,
    atr_update, stochastic_update,
};
pub use signals::{SignalBus, SignalCondition, SignalEvent, SignalIndicator};
pub use strategy_trait::Strategy;
pub use types::{
    BacktestMode, Bar, InstrumentId, OmsType, Order, OrderSide, OrderType,
    ParameterSchema, ParameterType, ParameterValue, PositionId, PositionSide, Signal,
    StrategyId, Tick,
};

// live_strategy and actor_wrapper require the nexus crate (live trading bridging).
// They are compiled only when the "live" feature is enabled.
#[cfg(feature = "live")]
pub mod live_strategy;

#[cfg(feature = "live")]
pub mod actor_wrapper;