//! Order Management System — central order state machine for backtest, live, and paper.
//!
//! Phase 5.3: Tracks complete order lifecycle from submit through fill/cancel/reject.
//! All state transitions are published to the MessageBus.

use std::sync::{Arc, Mutex};

use crate::actor::MessageBus;
use crate::cache::Cache;
use crate::engine::account::{OmsType, PositionIdGenerator};
use crate::messages::{
    ClientOrderId, OrderCancelled, OrderFilled,
    OrderPartiallyFilled, OrderRejected, OrderSide, OrderSubmitted, OrderType,
    PositionId, StrategyId, SubmitOrder, TraderId, VenueOrderId,
};

// ============ OrderState ============

/// Explicit order lifecycle state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderState {
    Pending,        // Submitted, awaiting exchange acknowledgment
    Accepted,       // Exchange accepted, resting on book
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
}

// ============ OmsOrder ============

/// Canonical order state for OMS — tracks complete order lifecycle.
#[derive(Debug, Clone)]
pub struct OmsOrder {
    pub client_order_id: ClientOrderId,
    pub venue_order_id: Option<VenueOrderId>,
    pub position_id: PositionId,
    pub state: OrderState,
    pub filled_qty: f64,
    pub last_fill_price: f64,
    pub submitted_at_ns: u64,
    pub strategy_id: StrategyId,
    pub instrument_id: String,
    pub order_side: OrderSide,
    pub order_type: OrderType,
    pub quantity: f64,
    pub price: Option<f64>,
}

impl OmsOrder {
    pub fn new(
        client_order_id: ClientOrderId,
        position_id: PositionId,
        strategy_id: StrategyId,
        instrument_id: String,
        order_side: OrderSide,
        order_type: OrderType,
        quantity: f64,
        price: Option<f64>,
        submitted_at_ns: u64,
    ) -> Self {
        Self {
            client_order_id,
            venue_order_id: None,
            position_id,
            state: OrderState::Pending,
            filled_qty: 0.0,
            last_fill_price: 0.0,
            submitted_at_ns,
            strategy_id,
            instrument_id,
            order_side,
            order_type,
            quantity,
            price,
        }
    }
}

// ============ Oms ============

/// Shared Order Management System.
/// Stores Arc<Mutex<Cache>> + Arc<MessageBus> + PositionIdGenerator + OmsType.
/// All methods self-publish to MsgBus via internal Arc<MessageBus>.
///
/// Thread-safe interior mutability via Arc<Mutex<Cache>>.
pub struct Oms {
    cache: Arc<Mutex<Cache>>,
    msgbus: Arc<MessageBus>,
    position_id_gen: PositionIdGenerator,
    oms_type: OmsType,
}

impl Oms {
    pub fn new(cache: Arc<Mutex<Cache>>, msgbus: Arc<MessageBus>, oms_type: OmsType) -> Self {
        Self {
            cache,
            msgbus,
            position_id_gen: PositionIdGenerator::new(),
            oms_type,
        }
    }

    /// Generate position_id using PositionIdGenerator, insert into cache as Pending,
    /// publish OrderSubmitted to MsgBus.
    /// Returns PositionId (ClientOrderId comes from SubmitOrder argument).
    pub fn submit_order(&mut self, submit: &SubmitOrder, strategy_id: StrategyId) -> PositionId {
        // Extract instrument_id from the submit's instrument_id string
        // Format may be "BTCUSDT.BINANCE" or just the raw string
        let instrument_id_str = submit.instrument_id.clone();

        // Parse instrument_id from the string
        let (symbol, venue_str) = if let Some(dot) = instrument_id_str.find('.') {
            (
                instrument_id_str[..dot].to_string(),
                instrument_id_str[dot + 1..].to_string(),
            )
        } else {
            (instrument_id_str.clone(), String::new())
        };

        // Build a simple instrument for PositionIdGenerator
        let instrument_id = crate::instrument::InstrumentId::new(&symbol, &venue_str);
        let venue = crate::instrument::Venue::new(&venue_str);

        // Generate position_id using OmsType-aware generator
        let position_id = self.position_id_gen.next_with_oms(
            &venue,
            &strategy_id,
            &instrument_id,
            self.oms_type,
        );

        let oms_order = OmsOrder::new(
            submit.client_order_id.clone(),
            position_id.clone(),
            strategy_id.clone(),
            instrument_id_str.clone(),
            submit.order_side,
            submit.order_type,
            submit.quantity,
            submit.price,
            submit.ts_init,
        );

        // Persist to cache
        self.cache.lock().unwrap().update_oms_order(oms_order.clone());

        // Build and publish OrderSubmitted event
        let order_submitted = OrderSubmitted::new(
            submit.trader_id.clone(),
            strategy_id,
            submit.client_order_id.clone(),
            VenueOrderId::new(""), // Placeholder — venue assigns on acceptance
            instrument_id_str,
            submit.order_side,
            submit.order_type,
            submit.quantity,
            submit.ts_init,
            submit.ts_init,
        );

        self.msgbus.publish("order.submitted", &order_submitted);

        position_id
    }

    /// Called when exchange sends NEW event via WebSocket.
    /// Transitions Pending -> Accepted, records venue_order_id.
    pub fn apply_accepted(&self, client_order_id: &ClientOrderId, venue_order_id: VenueOrderId) {
        let mut cache = self.cache.lock().unwrap();
        if let Some(order) = cache.oms_orders_mut().get_mut(client_order_id) {
            order.venue_order_id = Some(venue_order_id);
            order.state = OrderState::Accepted;
        }
    }

    /// Apply a fill (partial or full). Updates cache, publishes OrderFilled/OrderPartiallyFilled.
    pub fn apply_fill(&self, client_order_id: &ClientOrderId, fill: &OrderFilled) {
        let mut cache = self.cache.lock().unwrap();
        let order = match cache.oms_orders_mut().get_mut(client_order_id) {
            Some(o) => o,
            None => return,
        };

        order.filled_qty += fill.filled_qty;
        order.last_fill_price = fill.fill_price;

        // Determine new state
        let new_state = if order.filled_qty >= order.quantity {
            OrderState::Filled
        } else {
            OrderState::PartiallyFilled
        };
        order.state = new_state.clone();
        let last_fill_price = order.last_fill_price;
        let instrument_id = order.instrument_id.clone();
        let strategy_id = order.strategy_id.clone();

        // Publish appropriate event
        if matches!(new_state, OrderState::Filled) {
            let event = OrderFilled {
                trader_id: fill.trader_id.clone(),
                strategy_id,
                client_order_id: fill.client_order_id.clone(),
                venue_order_id: fill.venue_order_id.clone(),
                position_id: fill.position_id.clone(),
                trade_id: fill.trade_id.clone(),
                instrument_id,
                order_side: fill.order_side,
                filled_qty: fill.filled_qty,
                fill_price: fill.fill_price,
                commission: fill.commission,
                slippage_bps: fill.slippage_bps,
                is_maker: fill.is_maker,
                ts_event: fill.ts_event,
                ts_init: fill.ts_init,
            };
            self.msgbus.publish("order.filled", &event);
        } else {
            let order = match cache.oms_orders_mut().get_mut(client_order_id) {
                Some(o) => o,
                None => return,
            };
            let remaining_qty = order.quantity - order.filled_qty;
            let event = OrderPartiallyFilled {
                trader_id: fill.trader_id.clone(),
                strategy_id,
                client_order_id: fill.client_order_id.clone(),
                venue_order_id: fill.venue_order_id.clone(),
                position_id: fill.position_id.clone(),
                trade_id: fill.trade_id.clone(),
                instrument_id,
                order_side: fill.order_side,
                filled_qty: fill.filled_qty,
                remaining_qty,
                fill_price: last_fill_price,
                commission: fill.commission,
                slippage_bps: fill.slippage_bps,
                ts_event: fill.ts_event,
                ts_init: fill.ts_init,
            };
            self.msgbus.publish("order.partially_filled", &event);
        }
    }

    /// Apply fill WITHOUT publishing to MsgBus — for PaperBroker where caller already has fills.
    pub fn apply_fill_no_publish(&self, client_order_id: &ClientOrderId, fill: &OrderFilled) {
        let mut cache = self.cache.lock().unwrap();
        let order = match cache.oms_orders_mut().get_mut(client_order_id) {
            Some(o) => o,
            None => return,
        };

        order.filled_qty += fill.filled_qty;
        order.last_fill_price = fill.fill_price;

        order.state = if order.filled_qty >= order.quantity {
            OrderState::Filled
        } else {
            OrderState::PartiallyFilled
        };
    }

    /// Cancel an order. Transitions to Cancelled.
    /// Returns true if the order was found and cancelled.
    pub fn cancel(&self, client_order_id: &ClientOrderId) -> bool {
        let (instrument_id, strategy_id) = {
            let mut cache = self.cache.lock().unwrap();
            let order = match cache.oms_orders_mut().get_mut(client_order_id) {
                Some(o) => o,
                None => return false,
            };
            order.state = OrderState::Cancelled;
            (order.instrument_id.clone(), order.strategy_id.clone())
        };

        let event = OrderCancelled {
            trader_id: strategy_id.0.split('-').next()
                .map(TraderId::new)
                .unwrap_or_else(|| TraderId::new("UNKNOWN")),
            strategy_id,
            client_order_id: client_order_id.clone(),
            venue_order_id: None,
            instrument_id,
            ts_event: 0,
            ts_init: 0,
            reason: None,
        };
        self.msgbus.publish("order.cancelled", &event);
        true
    }

    /// Reject an order. Transitions to Rejected.
    pub fn apply_rejection(&self, client_order_id: &ClientOrderId, reason: &str) {
        let (instrument_id, strategy_id) = {
            let mut cache = self.cache.lock().unwrap();
            let order = match cache.oms_orders_mut().get_mut(client_order_id) {
                Some(o) => o,
                None => return,
            };
            order.state = OrderState::Rejected;
            (order.instrument_id.clone(), order.strategy_id.clone())
        };

        let event = OrderRejected {
            trader_id: strategy_id.0.split('-').next()
                .map(TraderId::new)
                .unwrap_or_else(|| TraderId::new("UNKNOWN")),
            strategy_id,
            client_order_id: client_order_id.clone(),
            venue_order_id: None,
            instrument_id,
            ts_event: 0,
            ts_init: 0,
            reason: reason.to_string(),
        };
        self.msgbus.publish("order.rejected", &event);
    }
}
