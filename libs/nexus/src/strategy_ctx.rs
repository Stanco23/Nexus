//! Strategy context trait — what strategies can query from the engine.
//!
//! The backtesting engine implements this trait and passes `&mut dyn StrategyCtx` to
//! `Strategy::on_trade()` and `Strategy::on_bar()` callbacks.
//!
//! Lives in `nexus` crate so the backtest engine can implement it without
//! creating a cyclic dependency with `nexus-strategy`.

use crate::engine::orders::{Order, OrderSide, OrderType};
use crate::signals::SignalCallback;
use crate::instrument::InstrumentId;
use crate::engine::core::{PositionSide, Signal};

/// Strategy execution context — queryable state during backtest runs.
pub trait StrategyCtx: Send + Sync {
    /// Current market price for an instrument.
    fn current_price(&self, instrument_id: InstrumentId) -> f64;

    /// Current position side for an instrument.
    fn position(&self, instrument_id: InstrumentId) -> Option<PositionSide>;

    /// Total account equity.
    fn account_equity(&self) -> f64;

    /// Unrealized PnL for an open position on an instrument.
    fn unrealized_pnl(&self, instrument_id: InstrumentId) -> f64;

    /// All pending (unfilled) orders for an instrument.
    fn pending_orders(&self, instrument_id: InstrumentId) -> Vec<Order>;

    /// Subscribe to one or more instruments.
    fn subscribe_instruments(&mut self, instruments: Vec<InstrumentId>);

    /// Subscribe to a named signal. The callback is invoked when the signal fires.
    fn subscribe_signal(&mut self, name: &str, callback: SignalCallback);

    /// Submit a limit order.
    fn submit_limit(
        &mut self,
        instrument_id: InstrumentId,
        side: OrderSide,
        price: f64,
        size: f64,
    ) -> u64;

    /// Submit a market order.
    fn submit_market(
        &mut self,
        instrument_id: InstrumentId,
        side: OrderSide,
        size: f64,
    ) -> u64;

    /// Submit an order with SL/TP.
    #[allow(clippy::too_many_arguments)]
    fn submit_with_sl_tp(
        &mut self,
        instrument_id: InstrumentId,
        side: OrderSide,
        order_type: OrderType,
        price: f64,
        size: f64,
        sl: Option<f64>,
        tp: Option<f64>,
    ) -> u64;

    /// Submit a generic order (limit, market, stop, etc.).
    fn submit_order(&mut self, order: Order) -> u64;

    /// Cancel a pending order by ID.
    fn cancel_order(&mut self, order_id: u64) -> bool;

    /// Total realized and unrealized PnL for an instrument.
    fn position_pnl(&self, instrument_id: InstrumentId) -> f64;

    /// Generate a trading signal directly.
    fn emit_signal(&mut self, signal: Signal);
}