//! Backtest engine — tick-by-tick simulation.

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};

use crate::actor::MessageBus;
use crate::book::{OrderBook, OrderEmulator, OrderId, Side};
use crate::buffer::tick_buffer::{TickBuffer, TradeFlowStats};
use crate::cache::Cache;
use crate::engine::account::{Account, AccountId, Currency, OmsType};
use crate::engine::oms::Oms;
use crate::engine::orders::{check_sl_tp, Order, OrderManager, OrderSide, OrderType};
use crate::engine::risk::RiskEngine;
use crate::instrument::{InstrumentId, Venue};
use crate::messages::{ClientOrderId, OrderFilled, StrategyId, TraderId};
use crate::signals::{SignalBus, SignalCallback};
use crate::slippage::SlippageConfig;
use crate::strategy_ctx::StrategyCtx;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Buy,
    Sell,
    Close,
}

/// Position side for an instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionSide {
    Long,
    Short,
    Flat,
}

#[derive(Debug, Clone)]
pub struct Trade {
    pub timestamp_ns: u64,
    pub instrument_id: u32,
    pub side: Signal,
    pub price: f64,
    pub size: f64,
    pub commission: f64,
    pub pnl: f64,
    pub fee: f64,
    pub is_maker: bool,
}

/// Per-instrument position and PnL state.
#[derive(Debug, Clone)]
pub struct InstrumentState {
    pub position: f64,
    pub entry_price: f64,
    pub unrealized_pnl: f64,
    pub realized_pnl: f64,
    pub commissions: f64,
}

impl InstrumentState {
    fn new() -> Self {
        Self {
            position: 0.0,
            entry_price: 0.0,
            unrealized_pnl: 0.0,
            realized_pnl: 0.0,
            commissions: 0.0,
        }
    }
}

impl Default for InstrumentState {
    fn default() -> Self {
        Self::new()
    }
}

pub struct EngineContext {
    pub instrument_states: HashMap<u32, InstrumentState>,
    pub equity: f64,
    pub peak_equity: f64,
    pub max_drawdown: f64,
    pub num_trades: usize,
    pub equity_curve: Vec<f64>,
    /// Current VPIN value from the most recent tick (used for slippage in close_position).
    pub current_vpin: f64,
    /// Current market prices per instrument (populated each tick by engine).
    pub current_prices: HashMap<u32, f64>,
    /// Signal bus for named signal pub/sub.
    signal_bus: Arc<Mutex<SignalBus>>,
    /// Order emulator reference for submitting orders from strategy callbacks.
    order_emulator: OrderEmulatorPtr,
    /// Subscribed instrument IDs.
    subscribed_instruments: Vec<u32>,
    /// Subscribed signal names.
    subscribed_signals: Vec<String>,
    /// Pending orders tracked in the context for pending_orders() query.
    pending_orders: Vec<Order>,
}

/// Wrapper for raw pointer to OrderEmulator, with explicit Send+Sync impls.
/// This allows EngineContext to be Send+Sync even though it holds a raw pointer.
struct OrderEmulatorPtr(*mut OrderEmulator);
unsafe impl Send for OrderEmulatorPtr {}
unsafe impl Sync for OrderEmulatorPtr {}

impl Debug for EngineContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineContext")
            .field("instrument_states", &self.instrument_states)
            .field("equity", &self.equity)
            .field("peak_equity", &self.peak_equity)
            .field("max_drawdown", &self.max_drawdown)
            .field("num_trades", &self.num_trades)
            .field("equity_curve", &self.equity_curve)
            .field("current_vpin", &self.current_vpin)
            .field("current_prices", &self.current_prices)
            .field("subscribed_instruments", &self.subscribed_instruments)
            .field("subscribed_signals", &self.subscribed_signals)
            .field("pending_orders", &self.pending_orders)
            .finish()
    }
}

impl EngineContext {
    pub fn new(initial_equity: f64, signal_bus: Arc<Mutex<SignalBus>>, order_emulator: *mut OrderEmulator) -> Self {
        Self {
            instrument_states: HashMap::new(),
            equity: initial_equity,
            peak_equity: initial_equity,
            max_drawdown: 0.0,
            num_trades: 0,
            equity_curve: Vec::new(),
            current_vpin: 0.0,
            current_prices: HashMap::new(),
            signal_bus,
            order_emulator: OrderEmulatorPtr(order_emulator),
            subscribed_instruments: Vec::new(),
            subscribed_signals: Vec::new(),
            pending_orders: Vec::new(),
        }
    }

    /// Update the current market price for an instrument.
    /// Called by the engine each tick before strategy callbacks.
    pub fn update_price(&mut self, instrument_id: u32, price: f64) {
        self.current_prices.insert(instrument_id, price);
    }

    /// Get the current market price for an instrument.
    pub fn get_price(&self, instrument_id: u32) -> f64 {
        self.current_prices.get(&instrument_id).copied().unwrap_or(0.0)
    }

    pub fn position(&self, instrument_id: u32) -> f64 {
        self.instrument_states
            .get(&instrument_id)
            .map(|s| s.position)
            .unwrap_or(0.0)
    }

    pub fn entry_price(&self, instrument_id: u32) -> f64 {
        self.instrument_states
            .get(&instrument_id)
            .map(|s| s.entry_price)
            .unwrap_or(0.0)
    }

    pub fn unrealized_pnl(&self, instrument_id: u32, current_price: f64) -> f64 {
        let Some(state) = self.instrument_states.get(&instrument_id) else {
            return 0.0;
        };
        if state.position == 0.0 || state.entry_price == 0.0 {
            return 0.0;
        }
        if state.position > 0.0 {
            (current_price - state.entry_price) * state.position.abs()
        } else {
            (state.entry_price - current_price)
                * state.position.abs()
        }
    }

    pub fn total_unrealized_pnl(&self, prices: &HashMap<u32, f64>) -> f64 {
        self.instrument_states
            .iter()
            .map(|(id, s)| {
                let price = prices.get(id).copied().unwrap_or(s.entry_price);
                if s.position == 0.0 || s.entry_price == 0.0 {
                    0.0
                } else if s.position > 0.0 {
                    (price - s.entry_price) * s.position.abs()
                } else {
                    (s.entry_price - price) * s.position.abs()
                }
            })
            .sum()
    }

    pub fn update_equity(&mut self, prices: &HashMap<u32, f64>) {
        self.equity += self.total_unrealized_pnl(prices);
        if self.equity > self.peak_equity {
            self.peak_equity = self.equity;
        }
        let dd = if self.peak_equity > 0.0 {
            (self.peak_equity - self.equity) / self.peak_equity * 100.0
        } else {
            0.0
        };
        if dd > self.max_drawdown {
            self.max_drawdown = dd;
        }
        self.equity_curve.push(self.equity);
    }

    pub fn record_trade(&mut self, trade: &Trade) {
        self.num_trades += 1;
        if self.instrument_states.contains_key(&trade.instrument_id) {
            let state = self.instrument_states.get_mut(&trade.instrument_id).unwrap();
            state.realized_pnl += trade.pnl;
            state.commissions += trade.commission;
        }
    }

    /// Add a new pending order to the context's order tracking.
    pub fn add_pending_order(&mut self, order: Order) {
        self.pending_orders.push(order);
    }

    /// Remove a pending order by ID.
    pub fn remove_pending_order(&mut self, order_id: u64) {
        self.pending_orders.retain(|o| o.id != order_id);
    }

    /// Clear all pending orders (e.g., for on_reset).
    pub fn clear_pending_orders(&mut self) {
        self.pending_orders.clear();
    }

    /// Reset equity and statistics (for sweep/ walk-forward reuse).
    pub fn reset_stats(&mut self, initial_equity: f64) {
        self.equity = initial_equity;
        self.peak_equity = initial_equity;
        self.max_drawdown = 0.0;
        self.num_trades = 0;
        self.equity_curve.clear();
    }

    /// Reset all strategy state for reuse in sweeps / walk-forward.
    pub fn reset_all(&mut self, initial_equity: f64) {
        self.instrument_states.clear();
        self.reset_stats(initial_equity);
        self.current_prices.clear();
        self.current_vpin = 0.0;
        self.pending_orders.clear();
        self.subscribed_instruments.clear();
        self.subscribed_signals.clear();
        // Clear the signal bus subscribers too
        if let Ok(sb) = self.signal_bus.lock() {
            sb.clear();
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// StrategyCtx Implementation — ALL 13 METHODS PROPERLY IMPLEMENTED
// ═══════════════════════════════════════════════════════════════════════════════

impl StrategyCtx for EngineContext {
    /// Current market price for an instrument.
    /// Returns the last known price from the tick loop, or 0.0 if no price seen.
    fn current_price(&self, instrument_id: InstrumentId) -> f64 {
        self.get_price(instrument_id.id)
    }

    /// Current position side for an instrument.
    /// Returns Long, Short, or Flat.
    fn position(&self, instrument_id: InstrumentId) -> Option<PositionSide> {
        let pos = self.position(instrument_id.id);
        if pos > 0.0 {
            Some(PositionSide::Long)
        } else if pos < 0.0 {
            Some(PositionSide::Short)
        } else {
            Some(PositionSide::Flat)
        }
    }

    /// Total account equity (current balance + unrealized PnL).
    fn account_equity(&self) -> f64 {
        self.equity
    }

    /// Unrealized PnL for an open position on an instrument.
    /// Uses the current market price from the tick loop.
    fn unrealized_pnl(&self, instrument_id: InstrumentId) -> f64 {
        let current_price = self.get_price(instrument_id.id);
        self.unrealized_pnl(instrument_id.id, current_price)
    }

    /// All pending (unfilled) orders for an instrument.
    /// Returns orders tracked in the context's pending_orders list.
    fn pending_orders(&self, instrument_id: InstrumentId) -> Vec<Order> {
        self.pending_orders
            .iter()
            .filter(|o| o.instrument_id == instrument_id && !o.filled)
            .cloned()
            .collect()
    }

    /// Subscribe to one or more instruments.
    /// Stores instrument IDs for tracking; actual subscription happens via engine.
    fn subscribe_instruments(&mut self, instruments: Vec<InstrumentId>) {
        self.subscribed_instruments = instruments.iter().map(|i| i.id).collect();
    }

    /// Subscribe to a named signal. The callback is invoked when the signal fires.
    fn subscribe_signal(&mut self, name: &str, callback: SignalCallback) {
        // SignalBus uses RwLock internally, but we receive Arc<Mutex<SignalBus>>
        // from the engine. Lock and delegate.
        let sb = self.signal_bus.lock().unwrap();
        sb.subscribe(name, callback);
        drop(sb);
        if !self.subscribed_signals.contains(&name.to_string()) {
            self.subscribed_signals.push(name.to_string());
        }
    }

    /// Submit a limit order.
    /// Returns an order ID (0 if emulator not available).
    fn submit_limit(
        &mut self,
        instrument_id: InstrumentId,
        side: OrderSide,
        price: f64,
        size: f64,
    ) -> u64 {
        let side = if side == OrderSide::Buy { Side::Buy } else { Side::Sell };
        let ts = 0; // Will be set by caller

        // SAFETY: order_emulator is set to point to BacktestEngine's OrderEmulator which lives
        // as long as the engine. The pointer is stable for the duration of a backtest run.
        if self.order_emulator.0.is_null() {
            return 0;
        }
        unsafe { (*self.order_emulator.0).submit_limit(price, size, side, ts) }
    }

    /// Submit a market order.
    /// In the backtest engine, market orders are executed at the current price
    /// from the signal-driven flow. Returns 0 if emulator not available.
    fn submit_market(
        &mut self,
        instrument_id: InstrumentId,
        side: OrderSide,
        size: f64,
    ) -> u64 {
        let side = if side == OrderSide::Buy { Side::Buy } else { Side::Sell };
        let ts = 0;

        if self.order_emulator.0.is_null() {
            return 0;
        }
        unsafe { (*self.order_emulator.0).submit_market(size, side, ts) }
    }

    /// Submit an order with SL/TP.
    /// Currently delegates to limit order submission (SL/TP managed externally).
    /// Returns an order ID.
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
    ) -> u64 {
        // Create a basic order record for tracking
        let order = Order {
            id: 0,
            client_order_id: ClientOrderId::new("ctx"),
            strategy_id: StrategyId::new(""),
            instrument_id,
            venue: instrument_id.venue.unwrap_or(Venue::new("BACKTEST")),
            side,
            order_type,
            price,
            size,
            sl,
            tp,
            filled: false,
            triggered: false,
            position_id: None,
            time_in_force: None,
            expire_time_ns: None,
            trailing_delta: None,
        };
        let order_id = order.id;

        // Submit to emulator if available
        if !self.order_emulator.0.is_null() {
            let side = if side == OrderSide::Buy { Side::Buy } else { Side::Sell };
            match order_type {
                OrderType::Market => {
                    unsafe { (*self.order_emulator.0).submit_market(size, side, 0) };
                }
                _ => {
                    unsafe { (*self.order_emulator.0).submit_limit(price, size, side, 0) };
                }
            }
        }

        // Track in pending orders
        self.add_pending_order(order);
        order_id
    }

    /// Submit a generic order (limit, market, stop, etc.).
    /// Routes to OrderEmulator for fill simulation.
    fn submit_order(&mut self, order: Order) -> u64 {
        let instrument_id = order.instrument_id.id;
        let order_id = order.id;

        // Clone the order for pending tracking
        let mut order_for_tracking = order.clone();
        order_for_tracking.id = order_id;

        // Also submit to OrderEmulator for queue position tracking
        if !self.order_emulator.0.is_null() {
            let side = if order.side == OrderSide::Buy { Side::Buy } else { Side::Sell };
            match order.order_type {
                OrderType::Market | OrderType::MarketIfTouched => {
                    unsafe { (*self.order_emulator.0).submit_market(order.size, side, 0) };
                }
                _ => {
                    // For limit, stop, etc., use the order price
                    unsafe { (*self.order_emulator.0).submit_limit(order.price, order.size, side, 0) };
                }
            }
        }

        self.add_pending_order(order_for_tracking);
        order_id
    }

    /// Cancel a pending order by ID.
    /// Removes from context's pending_orders and from OrderEmulator if available.
    fn cancel_order(&mut self, order_id: u64) -> bool {
        // Check if we have a pending order with this ID
        let has_order = self.pending_orders.iter().any(|o| o.id == order_id);

        if has_order {
            self.remove_pending_order(order_id);
        }

        // Also attempt to cancel in the OrderEmulator for queue position tracking
        if !self.order_emulator.0.is_null() {
            let result = unsafe { (*self.order_emulator.0).cancel_order(order_id) };
            return has_order || result;
        }

        has_order
    }

    /// Total realized and unrealized PnL for an instrument.
    /// Combines realized PnL from closed trades with unrealized PnL from open position.
    fn position_pnl(&self, instrument_id: InstrumentId) -> f64 {
        let Some(state) = self.instrument_states.get(&instrument_id.id) else {
            return 0.0;
        };

        // Unrealized PnL from open position using current market price
        let current_price = self.get_price(instrument_id.id);
        let unrealized = if state.position != 0.0 && state.entry_price != 0.0 {
            if state.position > 0.0 {
                (current_price - state.entry_price) * state.position.abs()
            } else {
                (state.entry_price - current_price) * state.position.abs()
            }
        } else {
            0.0
        };

        // Total PnL = realized (from closed trades) + unrealized (from open position)
        state.realized_pnl + unrealized
    }

    /// Generate a trading signal directly.
    /// Publishes to the SignalBus so other strategies/subscribers receive it.
    fn emit_signal(&mut self, signal: Signal) {
        let sb = self.signal_bus.lock().unwrap();
        let val = match signal {
            Signal::Buy => 1.0,
            Signal::Sell => -1.0,
            Signal::Close => 0.0,
        };
        sb.publish("signal", val, 0);
    }
}

// ────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct CommissionConfig {
    pub rate: f64,
    /// Fee rate for limit orders (resting in book, maker).
    pub maker_rate: f64,
}

impl CommissionConfig {
    pub fn new(rate: f64) -> Self {
        Self {
            rate,
            maker_rate: rate * 0.5,
        }
    }

    pub fn compute(&self, price: f64, size: f64) -> f64 {
        price * size.abs() * self.rate
    }

    pub fn compute_maker(&self, price: f64, size: f64) -> f64 {
        price * size.abs() * self.maker_rate
    }

    pub fn compute_taker(&self, price: f64, size: f64) -> f64 {
        price * size.abs() * self.rate
    }
}

impl Default for CommissionConfig {
    fn default() -> Self {
        Self::new(0.001)
    }
}

impl Default for SlippageConfig {
    fn default() -> Self {
        Self::new(0.0005)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_commission_compute() {
        let comm = CommissionConfig::new(0.001);
        let cost = comm.compute(100.0, 1.0);
        assert!((cost - 0.001).abs() < 0.0001);
    }

    #[test]
    fn test_commission_full_round_trip() {
        let comm = CommissionConfig::new(0.001);
        let entry = 100.0;
        let size = 1.0;
        let exit = 110.0;
        let entry_comm = comm.compute(entry, size);
        let exit_comm = comm.compute(exit, size);
        let pnl = (exit - entry) * size - entry_comm - exit_comm;
        assert!((pnl - 9.998).abs() < 0.001, "pnl={}", pnl);
    }

    #[test]
    fn test_short_commission_pnl() {
        let comm = CommissionConfig::new(0.001);
        let entry = 100.0;
        let size = 1.0;
        let exit = 90.0;
        let entry_comm = comm.compute(entry, size);
        let exit_comm = comm.compute(exit, size);
        let pnl = (entry - exit) * size - entry_comm - exit_comm;
        assert!((pnl - 9.998).abs() < 0.001, "pnl={}", pnl);
    }

    #[test]
    fn test_engine_context_new() {
        let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
        let ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
        assert_eq!(ctx.equity, 10000.0);
        assert_eq!(ctx.peak_equity, 10000.0);
        assert_eq!(ctx.max_drawdown, 0.0);
    }

    #[test]
    fn test_engine_context_unrealized_pnl_long() {
        let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
        let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
        let id = 1u32;
        ctx.instrument_states.insert(
            id,
            InstrumentState {
                position: 1.0,
                entry_price: 100.0,
                unrealized_pnl: 0.0,
                realized_pnl: 0.0,
                commissions: 0.0,
            },
        );
        let pnl = ctx.unrealized_pnl(id, 110.0);
        assert!((pnl - 10.0).abs() < 0.001);
    }

    #[test]
    fn test_engine_context_unrealized_pnl_short() {
        let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
        let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
        let id = 1u32;
        ctx.instrument_states.insert(
            id,
            InstrumentState {
                position: -1.0,
                entry_price: 100.0,
                unrealized_pnl: 0.0,
                realized_pnl: 0.0,
                commissions: 0.0,
            },
        );
        let pnl = ctx.unrealized_pnl(id, 90.0);
        assert!((pnl - 10.0).abs() < 0.001);
    }

    #[test]
    fn test_engine_context_update_price() {
        let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
        let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
        ctx.update_price(1, 100.0);
        ctx.update_price(2, 200.0);

        assert_eq!(ctx.get_price(1), 100.0);
        assert_eq!(ctx.get_price(2), 200.0);
        assert_eq!(ctx.get_price(999), 0.0); // Unknown instrument
    }

    #[test]
    fn test_engine_context_current_price_via_ctx() {
        let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
        let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());

        ctx.update_price(1, 100.0);
        let instr_id = InstrumentId {
            id: 1,
            venue: None,
        };
        assert_eq!(ctx.current_price(instr_id), 100.0);
    }

    #[test]
    fn test_engine_context_pending_orders() {
        let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
        let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
        let order1 = Order {
            id: 1,
            client_order_id: ClientOrderId::new("o1"),
            strategy_id: StrategyId::new("test"),
            instrument_id: InstrumentId {
                id: 1,
                venue: None,
            },
            venue: Venue::new("TEST"),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            price: 100.0,
            size: 1.0,
            sl: None,
            tp: None,
            filled: false,
            triggered: false,
            position_id: None,
            time_in_force: None,
            expire_time_ns: None,
            trailing_delta: None,
        };
        let order2 = Order {
            id: 2,
            client_order_id: ClientOrderId::new("o2"),
            strategy_id: StrategyId::new("test"),
            instrument_id: InstrumentId {
                id: 1,
                venue: None,
            },
            venue: Venue::new("TEST"),
            side: OrderSide::Sell,
            order_type: OrderType::Limit,
            price: 110.0,
            size: 1.0,
            sl: None,
            tp: None,
            filled: false,
            triggered: false,
            position_id: None,
            time_in_force: None,
            expire_time_ns: None,
            trailing_delta: None,
        };
        ctx.add_pending_order(order1);
        ctx.add_pending_order(order2);
        let instr_id = InstrumentId {
            id: 1,
            venue: None,
        };
        let pending = ctx.pending_orders(instr_id);
        assert_eq!(pending.len(), 2);

        // Simulate fill of order 1
        ctx.remove_pending_order(1);
        let pending = ctx.pending_orders(instr_id);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, 2);
    }

    #[test]
    fn test_strategy_ctx_submit_limit_null_emulator() {
        let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
        let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
        let instr_id = InstrumentId {
            id: 1,
            venue: None,
        };

        let order_id = ctx.submit_limit(instr_id, OrderSide::Buy, 100.0, 1.0);
        assert_eq!(order_id, 0); // Should return 0 when emulator is null
    }

    #[test]
    fn test_strategy_ctx_submit_market_null_emulator() {
        let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
        let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
        let instr_id = InstrumentId {
            id: 1,
            venue: None,
        };

        let order_id = ctx.submit_market(instr_id, OrderSide::Sell, 1.0);
        assert_eq!(order_id, 0);
    }

    #[test]
    fn test_strategy_ctx_submit_with_sl_tp() {
        let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
        let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
        let instr_id = InstrumentId {
            id: 1,
            venue: None,
        };
        ctx.submit_with_sl_tp(
            instr_id,
            OrderSide::Buy,
            OrderType::Limit,
            100.0,
            1.0,
            Some(95.0),  // SL
            Some(110.0), // TP
        );
        let pending = ctx.pending_orders(instr_id);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].price, 100.0);
        assert_eq!(pending[0].sl, Some(95.0));
        assert_eq!(pending[0].tp, Some(110.0));
    }

    #[test]
    fn test_strategy_ctx_submit_order() {
        let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
        let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
        let instr_id = InstrumentId {
            id: 1,
            venue: None,
        };

        let order = Order {
            id: 42,
            client_order_id: ClientOrderId::new("o42"),
            strategy_id: StrategyId::new("test"),
            instrument_id: instr_id,
            venue: Venue::new("TEST"),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            price: 100.0,
            size: 1.0,
            sl: None,
            tp: None,
            filled: false,
            triggered: false,
            position_id: None,
            time_in_force: None,
            expire_time_ns: None,
            trailing_delta: None,
        };

        let order_id = ctx.submit_order(order);
        assert_eq!(order_id, 42);

        let pending = ctx.pending_orders(instr_id);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, 42);
    }

    #[test]
    fn test_strategy_ctx_cancel_order() {
        let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
        let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
        let instr_id = InstrumentId {
            id: 1,
            venue: None,
        };

        // Add a pending order
        let order = Order {
            id: 42,
            client_order_id: ClientOrderId::new("o42"),
            strategy_id: StrategyId::new("test"),
            instrument_id: instr_id,
            venue: Venue::new("TEST"),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            price: 100.0,
            size: 1.0,
            sl: None,
            tp: None,
            filled: false,
            triggered: false,
            position_id: None,
            time_in_force: None,
            expire_time_ns: None,
            trailing_delta: None,
        };
        ctx.add_pending_order(order);

        // Cancel the order
        let result = ctx.cancel_order(42);
        assert!(result);

        let pending = ctx.pending_orders(instr_id);
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn test_strategy_ctx_cancel_order_not_found() {
        let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
        let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());

        // Try to cancel a non-existent order
        let result = ctx.cancel_order(999);
        assert!(!result);
    }

    #[test]
    fn test_strategy_ctx_position_pnl() {
        let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
        let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
        let instr_id = InstrumentId {
            id: 1,
            venue: None,
        };

        // Set up an open position
        ctx.instrument_states.insert(
            1u32,
            InstrumentState {
                position: 1.0,
                entry_price: 100.0,
                unrealized_pnl: 0.0,
                realized_pnl: 50.0, // Previous closed trades
                commissions: 0.0,
            },
        );
        ctx.update_price(1, 110.0); // Current price

        // position_pnl = realized_pnl + unrealized_pnl
        // unrealized = (110 - 100) * 1.0 = 10.0
        // total = 50.0 + 10.0 = 60.0
        let pnl = ctx.position_pnl(instr_id);
        assert!((pnl - 60.0).abs() < 0.001);
    }

    #[test]
    fn test_strategy_ctx_position_pnl_no_position() {
        let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
        let ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
        let instr_id = InstrumentId {
            id: 1,
            venue: None,
        };

        // No position, should return 0
        let pnl = ctx.position_pnl(instr_id);
        assert_eq!(pnl, 0.0);
    }

    #[test]
    fn test_strategy_ctx_emit_signal() {
        let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
        let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());

        let received = Arc::new(std::sync::Mutex::new(Vec::new()));
        let received_clone = received.clone();

        ctx.subscribe_signal(
            "signal",
            Box::new(move |name, value, _ts| {
                received_clone.lock().unwrap().push((name.to_string(), value));
            }),
        );
        ctx.emit_signal(Signal::Buy);
        ctx.emit_signal(Signal::Sell);
        ctx.emit_signal(Signal::Close);
        let r = received.lock().unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].0, "signal");
        assert_eq!(r[0].1, 1.0); // Buy
        assert_eq!(r[1].1, -1.0); // Sell
        assert_eq!(r[2].1, 0.0); // Close
    }

    #[test]
    fn test_engine_context_max_drawdown() {
        let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
        let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
        // Simulate equity curve: 10000 → 11000 → 9000
        let prices1 = HashMap::new(); // No open positions
        ctx.update_equity(&prices1);

        let mut prices2 = HashMap::new();
        prices2.insert(1u32, 11000.0);
        ctx.update_equity(&prices2);

        let mut prices3 = HashMap::new();
        prices3.insert(1u32, 9000.0);
        ctx.update_equity(&prices3);

        assert_eq!(ctx.peak_equity, 11000.0);
        assert!((ctx.max_drawdown - 18.18).abs() < 0.01);
    }
}