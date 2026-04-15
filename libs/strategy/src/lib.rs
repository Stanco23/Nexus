//! Nexus strategy trait and example strategies.

pub mod context;
pub mod examples;
pub mod indicators;
pub mod strategy_trait;
pub mod types;

pub use context::StrategyCtx;
pub use indicators::{
    Atr, BollingerBands, Ema, Indicator, Macd, Rsi, Sma, Stochastic, Vwap,
    atr_update, stochastic_update,
};
pub use strategy_trait::Strategy;
pub use types::{
    BacktestMode, Bar, InstrumentId, Order, OrderSide, OrderType, ParameterSchema,
    ParameterType, ParameterValue, PositionSide, Signal, Tick,
};
