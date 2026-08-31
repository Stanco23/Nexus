//! Nexus strategy trait and example strategies.

pub mod context;
pub mod examples;
pub mod indicators;
pub mod signals;
pub mod strategy_trait;
pub mod types;

pub use examples::{EmaCrossStrategy, RsiStrategy, SmaCrossTrailingStrategy, TrendFollowingStrategy, VwapMomentumStrategy};

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

// actor_wrapper lives in nexus (libs/nexus/src/live/actor_wrapper.rs) when the
// "live" feature is enabled — it cannot live here due to cyclic deps.
// nexus depends on strategy (for Strategy trait), strategy cannot depend on nexus.

#[cfg(feature = "live")]
pub mod live_strategy;

#[cfg(feature = "live")]
pub mod actor_wrapper;