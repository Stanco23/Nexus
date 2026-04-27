//! Backtest engine — tick-by-tick and bar-mode simulation.
//!
//! Re-exports core types: `BacktestEngine`, `EngineContext`, `Signal`, `Trade`, `Strategy`.

pub mod core;
pub mod account;
pub mod margin;
pub mod oms;
pub mod orders;
pub mod reports;
pub mod risk;
pub mod sizing;

pub use core::{
    CommissionConfig, EngineContext, InstrumentState, PositionSide, Signal, Trade,
};
pub use crate::strategy_ctx::StrategyCtx;
pub use orders::{Order, OrderManager, OrderSide, OrderType, check_pending_orders, check_sl_tp};
pub use account::{Account, AccountEvent, AccountId, Balance, Currency, OmsType, Position};
pub use oms::{Oms, OmsOrder, OrderState};
pub use margin::{MarginAccount, MarginConfig, MarginMode, MarginState};
pub use risk::{RiskConfig, RiskEngine};
pub use sizing::{Sizing, SizingConfig, SizingMethod};
pub use reports::{Commission, CommissionSide, FillReport, LiquiditySide, OrderReport, OrderReportStatus, OrderStatusReport, PositionStatusReport};
pub use crate::database::{Database, DatabaseError, SqliteDatabase, MemoryDatabase};
