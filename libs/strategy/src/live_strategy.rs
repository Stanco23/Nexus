//! Live Strategy trait — bridges `Actor` (Phase 5.0a) with `Strategy` (Phase 3.1).
//!
//! `LiveStrategy` extends `Actor` with strategy-specific fields:
//! - `strategy_id` — unique strategy identifier
//! - `oms_type` — order management system mode (SingleOrder, Hedge, Netting)
//! - `position_id` — assigned by OMS when a position is opened
//!
//! This trait is implemented by live trading strategies that receive 32+ event
//! handlers (`on_trade_tick`, `on_bar`, `on_order_filled`, etc.).
//!
//! For wrapping backtest-style `dyn Strategy` into an `Actor`, see `actor_wrapper.rs`.

use crate::types::{OmsType, PositionId, StrategyId};
use nexus::actor::{Actor, MessageBus};
use std::sync::Arc;

// =============================================================================
// LiveStrategy trait
// =============================================================================

/// Strategy-as-Actor for live trading.
///
/// Extends the `Actor` trait (32 on_* handlers from Phase 5.0h) with
/// strategy-specific identity fields.
///
/// Live strategies receive market data events (`on_trade_tick`, `on_bar`)
/// and order/position events (`on_order_filled`, `on_position_opened`, etc.)
/// and can submit orders via the message bus.
pub trait LiveStrategy: Actor {
    /// Return the strategy's unique identifier.
    fn strategy_id(&self) -> &StrategyId;

    /// Return the order management system type.
    fn oms_type(&self) -> OmsType;

    /// Return the current position ID assigned by the OMS.
    fn position_id(&self) -> Option<&PositionId>;

    /// Set the position ID (called by the OMS when a position is opened).
    fn set_position_id(&mut self, id: PositionId);
}

// =============================================================================
// LiveStrategyCtx
// =============================================================================

/// Context for live strategies — provides synchronous access to cache, msgbus, clock.
/// This differs from `StrategyCtx` (backtest) in that it provides direct access
/// to live system state rather than a snapshot.
pub trait LiveStrategyCtx {
    /// Return a reference to the cache.
    fn cache(&self) -> Arc<nexus::cache::Cache>;

    /// Return a reference to the message bus.
    fn msgbus(&self) -> &MessageBus;

    /// Return a reference to the clock.
    fn clock(&self) -> Arc<dyn nexus::actor::Clock>;
}
