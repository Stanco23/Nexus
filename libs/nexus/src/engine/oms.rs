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
    PositionId, StrategyId, SubmitOrder, TimeInForce, TraderId, VenueOrderId,
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
    // NEW fields:
    pub avg_fill_price: f64,
    pub num_fills: u32,
    pub last_trade_ns: u64,
    pub time_in_force: Option<TimeInForce>,
    pub expire_time_ns: Option<u64>,
    pub last_venue_order_id: Option<VenueOrderId>,
}

impl OmsOrder {
    #[allow(clippy::too_many_arguments)]
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
            avg_fill_price: 0.0,
            num_fills: 0,
            last_trade_ns: 0,
            time_in_force: None,
            expire_time_ns: None,
            last_venue_order_id: None,
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
    /// Transitions Pending -> Accepted, records venue_order_id, updates secondary index.
    pub fn apply_accepted(&self, client_order_id: &ClientOrderId, venue_order_id: VenueOrderId) {
        let mut cache = self.cache.lock().unwrap();
        if let Some(order) = cache.oms_orders_mut().get_mut(client_order_id) {
            order.last_venue_order_id = order.venue_order_id.clone();
            order.venue_order_id = Some(venue_order_id.clone());
            order.state = OrderState::Accepted;
        }
        cache.oms_update_venue_index(venue_order_id, client_order_id.clone());
    }

    /// Apply a fill (partial or full). Updates cache, publishes OrderFilled/OrderPartiallyFilled.
    /// Follows Lock-Then-Publish: extracts data, drops lock, then publishes.
    pub fn apply_fill(&self, client_order_id: &ClientOrderId, fill: &OrderFilled) {
        let filled_event_data: (OrderState, String, StrategyId, VenueOrderId, PositionId, f64, f64) = {
            let mut cache = self.cache.lock().unwrap();
            let order = match cache.oms_get_mut(client_order_id) {
                Some(o) => o,
                None => return,
            };

            // Compute weighted average fill price
            order.num_fills += 1;
            let total_filled = order.filled_qty + fill.filled_qty;
            order.avg_fill_price = if total_filled > 0.0 {
                (order.avg_fill_price * order.filled_qty + fill.fill_price * fill.filled_qty) / total_filled
            } else {
                fill.fill_price
            };
            order.last_fill_price = fill.fill_price;
            order.last_trade_ns = fill.ts_event;
            order.filled_qty = total_filled;

            // Determine new state
            let new_state = if order.filled_qty >= order.quantity {
                OrderState::Filled
            } else {
                OrderState::PartiallyFilled
            };
            order.state = new_state.clone();

            // Extract all data needed for events BEFORE dropping lock
            let last_fill_price = order.last_fill_price;
            let instrument_id = order.instrument_id.clone();
            let strategy_id = order.strategy_id.clone();
            let venue_order_id = order.venue_order_id.clone().unwrap_or_else(|| VenueOrderId::new(""));
            let remaining_qty = order.quantity - order.filled_qty;
            let position_id = order.position_id.clone();

            (new_state, instrument_id, strategy_id, venue_order_id, position_id, last_fill_price, remaining_qty)
        };

        // Publish AFTER releasing lock (Lock-Then-Publish pattern)
        if matches!(filled_event_data.0, OrderState::Filled) {
            let event = OrderFilled {
                trader_id: fill.trader_id.clone(),
                strategy_id: filled_event_data.2,
                client_order_id: fill.client_order_id.clone(),
                venue_order_id: filled_event_data.3,
                position_id: filled_event_data.4,
                trade_id: fill.trade_id.clone(),
                instrument_id: filled_event_data.1,
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
            let event = OrderPartiallyFilled {
                trader_id: fill.trader_id.clone(),
                strategy_id: filled_event_data.2,
                client_order_id: fill.client_order_id.clone(),
                venue_order_id: filled_event_data.3,
                position_id: filled_event_data.4,
                trade_id: fill.trade_id.clone(),
                instrument_id: filled_event_data.1,
                order_side: fill.order_side,
                filled_qty: fill.filled_qty,
                remaining_qty: filled_event_data.6,
                fill_price: filled_event_data.5,
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
    /// Follows Lock-Then-Publish: extracts data, drops lock, then publishes.
    /// Returns true if the order was found and cancelled.
    pub fn cancel(&self, client_order_id: &ClientOrderId) -> bool {
        let (instrument_id, strategy_id, venue_order_id) = {
            let mut cache = self.cache.lock().unwrap();
            let order = match cache.oms_orders_mut().get_mut(client_order_id) {
                Some(o) => o,
                None => return false,
            };
            order.state = OrderState::Cancelled;
            (
                order.instrument_id.clone(),
                order.strategy_id.clone(),
                order.venue_order_id.clone(),
            )
        };

        let event = OrderCancelled {
            trader_id: strategy_id.0.split('-').next()
                .map(TraderId::new)
                .unwrap_or_else(|| TraderId::new("UNKNOWN")),
            strategy_id,
            client_order_id: client_order_id.clone(),
            venue_order_id,
            instrument_id,
            ts_event: 0,
            ts_init: 0,
            reason: None,
        };
        self.msgbus.publish("order.cancelled", &event);
        true
    }

    /// Reject an order. Transitions to Rejected.
    /// Follows Lock-Then-Publish: extracts data, drops lock, then publishes.
    pub fn apply_rejection(&self, client_order_id: &ClientOrderId, reason: &str) {
        let (instrument_id, strategy_id, venue_order_id) = {
            let mut cache = self.cache.lock().unwrap();
            let order = match cache.oms_orders_mut().get_mut(client_order_id) {
                Some(o) => o,
                None => return,
            };
            order.state = OrderState::Rejected;
            (
                order.instrument_id.clone(),
                order.strategy_id.clone(),
                order.venue_order_id.clone(),
            )
        };

        let event = OrderRejected {
            trader_id: strategy_id.0.split('-').next()
                .map(TraderId::new)
                .unwrap_or_else(|| TraderId::new("UNKNOWN")),
            strategy_id,
            client_order_id: client_order_id.clone(),
            venue_order_id,
            instrument_id,
            ts_event: 0,
            ts_init: 0,
            reason: reason.to_string(),
        };
        self.msgbus.publish("order.rejected", &event);
    }

    /// Apply an order modification (price and/or quantity change).
    ///
    /// Called ONLY after the exchange confirms the modification (not optimistically).
    /// Returns the updated order or None if not found/non-modifiable.
    pub fn apply_modify(
        &self,
        client_order_id: &ClientOrderId,
        new_price: Option<f64>,
        new_quantity: Option<f64>,
    ) -> Option<OmsOrder> {
        let (instrument_id, strategy_id, venue_order_id) = {
            let mut cache = self.cache.lock().unwrap();
            let order = cache.oms_orders_mut().get_mut(client_order_id)?;

            // Only modify orders in Pending or Accepted state
            if !matches!(order.state, OrderState::Pending | OrderState::Accepted) {
                return None;
            }

            let instrument_id = order.instrument_id.clone();
            let strategy_id = order.strategy_id.clone();
            let venue_order_id = order.venue_order_id.clone();

            // Apply changes
            if let Some(p) = new_price {
                order.price = Some(p);
            }
            if let Some(q) = new_quantity {
                order.quantity = q;
            }

            (instrument_id, strategy_id, venue_order_id)
        };

        // Publish OrderModified after releasing lock
        let event = crate::messages::OrderModified {
            trader_id: strategy_id.0.split('-').next()
                .map(TraderId::new)
                .unwrap_or_else(|| TraderId::new("UNKNOWN")),
            strategy_id,
            client_order_id: client_order_id.clone(),
            venue_order_id: venue_order_id.unwrap_or_else(|| VenueOrderId::new("")),
            instrument_id,
            ts_event: 0,
            ts_init: 0,
        };
        self.msgbus.publish("order.modified", &event);

        // Return the updated order
        let cache = self.cache.lock().unwrap();
        cache.get_oms_order(client_order_id).cloned()
    }

    /// Cancel all open orders for a given strategy.
    /// Returns the list of client_order_ids that were cancelled.
    /// Follows Lock-Then-Publish: collects IDs inside lock, releases lock, then calls cancel.
    pub fn apply_cancel_all(&self, strategy_id: &StrategyId) -> Vec<ClientOrderId> {
        let cancelled = {
            let mut cache = self.cache.lock().unwrap();
            let open_ids = cache.oms_get_open_for_strategy(strategy_id);

            let mut cancelled = Vec::new();
            for coid in open_ids {
                if let Some(order) = cache.oms_get_mut(&coid) {
                    if matches!(order.state, OrderState::Pending | OrderState::Accepted | OrderState::PartiallyFilled) {
                        cancelled.push(coid.clone());
                        order.state = OrderState::Cancelled;
                    }
                }
            }
            cancelled
        };

        // Call cancel() for each — each call acquires/releases its own lock
        for coid in &cancelled {
            self.cancel(coid);
        }

        cancelled
    }

    /// Submit a batch of orders. Each order goes through the normal submit flow.
    /// Returns the list of PositionIds.
    pub fn submit_batch(&mut self, orders: &[SubmitOrder], strategy_id: &StrategyId) -> Vec<PositionId> {
        orders
            .iter()
            .map(|submit| self.submit_order(submit, strategy_id.clone()))
            .collect()
    }

    /// Reconcile OMS state with exchange-reported state after reconnect.
    ///
    /// Returns: `(oms_missing_ids, exchange_only_ids)`
    /// - `oms_missing_ids`: orders in OMS but not on exchange (already transitioned)
    /// - `exchange_only_ids`: orders on exchange but not in OMS (caller fetches details)
    ///
    /// Locking contract: Acquires cache lock, performs reconciliation,
    /// returns exchange-only IDs, then RELEASES the lock.
    pub fn reconcile(
        &self,
        exchange_open_orders: &[(ClientOrderId, VenueOrderId)],
    ) -> (Vec<ClientOrderId>, Vec<(ClientOrderId, VenueOrderId)>) {
        let (oms_missing, exchange_only) = {
            let mut cache = self.cache.lock().unwrap();

            // Build set of exchange-reported open orders (by client_order_id)
            let exchange_ids: std::collections::HashSet<_> = exchange_open_orders
                .iter()
                .map(|(coid, _)| coid.clone())
                .collect();

            // Phase A: Check each OMS open order
            let oms_open_ids = cache.oms_get_open_order_ids();

            let mut oms_missing = Vec::new();
            for coid in oms_open_ids {
                if !exchange_ids.contains(&coid) {
                    oms_missing.push(coid.clone());
                    if let Some(order) = cache.oms_get_mut(&coid) {
                        if order.filled_qty > 0.0 {
                            order.state = OrderState::Filled;
                        } else {
                            order.state = OrderState::Cancelled;
                        }
                    }
                }
            }

            // Phase B: Detect exchange-only orders
            let mut exchange_only = Vec::new();
            for (coid, vid) in exchange_open_orders {
                if !cache.oms_contains(coid) {
                    exchange_only.push((coid.clone(), vid.clone()));
                }
            }

            (oms_missing, exchange_only)
        };

        (oms_missing, exchange_only)
    }

    /// Recover an order that exists on the exchange but was not in OMS
    /// (submitted while WS was disconnected).
    pub fn apply_recovered_order(&self, order: OmsOrder) {
        let mut cache = self.cache.lock().unwrap();
        cache.update_oms_order(order);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::MessageBus;
    use crate::engine::account::OmsType;
    use crate::messages::{ClientOrderId, OrderFilled, OrderSide, OrderType, PositionId, StrategyId, SubmitOrder, TimeInForce, TradeId, TraderId, VenueOrderId};
    use std::sync::Arc;

    fn make_test_oms() -> (Oms, Arc<Mutex<Cache>>, Arc<MessageBus>) {
        let cache = Arc::new(Mutex::new(Cache::new(1000, 1000)));
        let msgbus = Arc::new(MessageBus::new());
        let oms = Oms::new(cache.clone(), msgbus.clone(), OmsType::Hedge);
        (oms, cache, msgbus)
    }

    fn make_submit_order(client_order_id: &str, price: f64) -> SubmitOrder {
        SubmitOrder::new(
            TraderId::new("TRADER-001"),
            StrategyId::new("TEST-STRAT"),
            "BTCUSDT.BINANCE".to_string(),
            ClientOrderId::new(client_order_id),
            OrderSide::Buy,
            OrderType::Limit,
            1.0,
            1_000_000_000,
        )
        .with_price(price)
    }

    #[test]
    fn test_oms_avg_fill_price_weighted_average() {
        let (mut oms, _cache, _msgbus) = make_test_oms();
        let submit = make_submit_order("ORD-WAP-001", 100.0);

        oms.submit_order(&submit, StrategyId::new("TEST-STRAT"));

        // First partial fill: 0.5 @ 100
        let fill1 = OrderFilled::new(
            TraderId::new("TRADER-001"),
            StrategyId::new("TEST-STRAT"),
            ClientOrderId::new("ORD-WAP-001"),
            VenueOrderId::new("VENUE-001"),
            PositionId::new("POS-001"),
            TradeId::new("T1"),
            "BTCUSDT.BINANCE".to_string(),
            OrderSide::Buy,
            0.5,
            100.0,
            1_000_000_100,
            1_000_000_000,
        );
        oms.apply_fill(&ClientOrderId::new("ORD-WAP-001"), &fill1);

        // Second partial fill: 0.5 @ 110
        let fill2 = OrderFilled::new(
            TraderId::new("TRADER-001"),
            StrategyId::new("TEST-STRAT"),
            ClientOrderId::new("ORD-WAP-001"),
            VenueOrderId::new("VENUE-001"),
            PositionId::new("POS-001"),
            TradeId::new("T2"),
            "BTCUSDT.BINANCE".to_string(),
            OrderSide::Buy,
            0.5,
            110.0,
            1_000_000_200,
            1_000_000_000,
        );
        oms.apply_fill(&ClientOrderId::new("ORD-WAP-001"), &fill2);

        // avg_fill_price = (0.5*100 + 0.5*110) / 1.0 = 105
        let cache = _cache.lock().unwrap();
        let order = cache.get_oms_order(&ClientOrderId::new("ORD-WAP-001")).unwrap();
        assert!((order.avg_fill_price - 105.0).abs() < 0.01);
        assert_eq!(order.num_fills, 2);
    }

    #[test]
    fn test_oms_apply_modify_updates_price() {
        let (mut oms, _cache, _msgbus) = make_test_oms();
        let submit = make_submit_order("ORD-MOD-001", 100.0);

        oms.submit_order(&submit, StrategyId::new("TEST-STRAT"));

        let result = oms.apply_modify(
            &ClientOrderId::new("ORD-MOD-001"),
            Some(95.0),
            None,
        );

        assert!(result.is_some());
        let cache = _cache.lock().unwrap();
        let order = cache.get_oms_order(&ClientOrderId::new("ORD-MOD-001")).unwrap();
        assert_eq!(order.price, Some(95.0));
    }

    #[test]
    fn test_oms_apply_modify_rejects_filled_orders() {
        let (mut oms, _cache, _msgbus) = make_test_oms();
        let submit = make_submit_order("ORD-FILLED-001", 100.0);

        oms.submit_order(&submit, StrategyId::new("TEST-STRAT"));

        // Fill the order completely
        let fill = OrderFilled::new(
            TraderId::new("TRADER-001"),
            StrategyId::new("TEST-STRAT"),
            ClientOrderId::new("ORD-FILLED-001"),
            VenueOrderId::new("VENUE-001"),
            PositionId::new("POS-001"),
            TradeId::new("T1"),
            "BTCUSDT.BINANCE".to_string(),
            OrderSide::Buy,
            1.0,
            100.0,
            1_000_000_100,
            1_000_000_000,
        );
        oms.apply_fill(&ClientOrderId::new("ORD-FILLED-001"), &fill);

        // Try to modify a filled order — should return None
        let result = oms.apply_modify(
            &ClientOrderId::new("ORD-FILLED-001"),
            Some(95.0),
            None,
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_oms_reconcile_marks_missing_without_fills_as_cancelled() {
        let (mut oms, cache, _msgbus) = make_test_oms();
        let submit = make_submit_order("ORD-RECON-001", 100.0);

        oms.submit_order(&submit, StrategyId::new("TEST-STRAT"));

        // Reconcile with empty exchange list (order is gone from exchange)
        let (oms_missing, _exchange_only) = oms.reconcile(&[]);

        assert_eq!(oms_missing.len(), 1);
        assert_eq!(oms_missing[0].0, "ORD-RECON-001");

        // OMS order should be cancelled (no fills)
        let cache = cache.lock().unwrap();
        let order = cache.get_oms_order(&ClientOrderId::new("ORD-RECON-001")).unwrap();
        assert_eq!(order.state, OrderState::Cancelled);
    }

    #[test]
    fn test_oms_reconcile_marks_missing_with_fills_as_filled() {
        let (mut oms, cache, _msgbus) = make_test_oms();
        let submit = make_submit_order("ORD-FILLED-RECON-001", 100.0);

        oms.submit_order(&submit, StrategyId::new("TEST-STRAT"));

        // Partially fill the order
        let fill = OrderFilled::new(
            TraderId::new("TRADER-001"),
            StrategyId::new("TEST-STRAT"),
            ClientOrderId::new("ORD-FILLED-RECON-001"),
            VenueOrderId::new("VENUE-001"),
            PositionId::new("POS-001"),
            TradeId::new("T1"),
            "BTCUSDT.BINANCE".to_string(),
            OrderSide::Buy,
            0.5,
            100.0,
            1_000_000_100,
            1_000_000_000,
        );
        oms.apply_fill(&ClientOrderId::new("ORD-FILLED-RECON-001"), &fill);

        // Reconcile with empty exchange list
        let (oms_missing, _exchange_only) = oms.reconcile(&[]);

        assert_eq!(oms_missing.len(), 1);

        // OMS order should be filled (has partial fills)
        let cache = cache.lock().unwrap();
        let order = cache.get_oms_order(&ClientOrderId::new("ORD-FILLED-RECON-001")).unwrap();
        assert_eq!(order.state, OrderState::Filled);
    }

    #[test]
    fn test_oms_submit_batch() {
        let (mut oms, _cache, _msgbus) = make_test_oms();

        let orders = vec![
            make_submit_order("BATCH-001", 100.0),
            make_submit_order("BATCH-002", 101.0),
            make_submit_order("BATCH-003", 102.0),
        ];

        let position_ids = oms.submit_batch(&orders, &StrategyId::new("TEST-STRAT"));

        assert_eq!(position_ids.len(), 3);
        // All position IDs should be unique
        assert_ne!(position_ids[0].0, position_ids[1].0);
        assert_ne!(position_ids[1].0, position_ids[2].0);
    }

    #[test]
    fn test_oms_apply_cancel_all() {
        let (mut oms, cache, _msgbus) = make_test_oms();

        // Submit 3 orders for same strategy
        for i in 1..=3 {
            let submit = make_submit_order(&format!("ORD-CA-{}", i), 100.0);
            oms.submit_order(&submit, StrategyId::new("TEST-STRAT"));
        }

        // Cancel all
        let cancelled = oms.apply_cancel_all(&StrategyId::new("TEST-STRAT"));
        assert_eq!(cancelled.len(), 3);

        // Verify all are cancelled
        let cache = cache.lock().unwrap();
        for i in 1..=3 {
            let order = cache.get_oms_order(&ClientOrderId::new(&format!("ORD-CA-{}", i))).unwrap();
            assert_eq!(order.state, OrderState::Cancelled);
        }
    }

    #[test]
    fn test_oms_reconcile_exchange_only_returns_untracked_orders() {
        let (mut oms, _cache, _msgbus) = make_test_oms();

        // Submit ORD-A only (ORD-B was submitted directly on exchange, never reached OMS
        // because WS was disconnected during the submission)
        let submit_a = make_submit_order("ORD-A", 100.0);
        oms.submit_order(&submit_a, StrategyId::new("TEST-STRAT"));

        // Exchange reports both orders: ORD-A (OMS knows it) and ORD-B (OMS doesn't know it
        // because it was submitted on exchange while WS was disconnected)
        let exchange_orders = vec![
            (ClientOrderId::new("ORD-A"), VenueOrderId::new("VENUE-001")),
            (ClientOrderId::new("ORD-B"), VenueOrderId::new("VENUE-002")),
        ];
        let (oms_missing, exchange_only) = oms.reconcile(&exchange_orders);

        // ORD-A: on both sides → not missing; ORD-B: exchange has it but OMS doesn't
        assert!(oms_missing.is_empty());
        assert_eq!(exchange_only.len(), 1);
        assert_eq!(exchange_only[0].0, ClientOrderId::new("ORD-B"));
        assert_eq!(exchange_only[0].1, VenueOrderId::new("VENUE-002"));
    }

    #[test]
    fn test_oms_reconcile_partial_fill_marks_partially_filled() {
        let (mut oms, cache, _msgbus) = make_test_oms();
        let submit = make_submit_order("ORD-C", 100.0);

        oms.submit_order(&submit, StrategyId::new("TEST-STRAT"));

        // Partially fill: 0.3 of 1.0 filled
        let fill = OrderFilled::new(
            TraderId::new("TRADER-001"),
            StrategyId::new("TEST-STRAT"),
            ClientOrderId::new("ORD-C"),
            VenueOrderId::new("VENUE-001"),
            PositionId::new("POS-001"),
            TradeId::new("T1"),
            "BTCUSDT.BINANCE".to_string(),
            OrderSide::Buy,
            0.3,
            100.0,
            1_000_000_100,
            1_000_000_000,
        );
        oms.apply_fill(&ClientOrderId::new("ORD-C"), &fill);

        // Exchange reports ORD-C still open with 0.7 remaining
        let exchange_orders = vec![
            (ClientOrderId::new("ORD-C"), VenueOrderId::new("VENUE-001")),
        ];
        let (oms_missing, exchange_only) = oms.reconcile(&exchange_orders);

        // Exact match → both sides agree, nothing is missing
        assert!(oms_missing.is_empty());
        assert!(exchange_only.is_empty());

        let cache = cache.lock().unwrap();
        let order = cache.get_oms_order(&ClientOrderId::new("ORD-C")).unwrap();
        assert_eq!(order.state, OrderState::PartiallyFilled);
        assert_eq!(order.filled_qty, 0.3);
    }

    #[test]
    fn test_oms_reconcile_all_closed_on_both_sides() {
        let (mut oms, cache, _msgbus) = make_test_oms();

        // Submit 3 orders
        let submit_a = make_submit_order("ORD-A", 100.0);
        let submit_b = make_submit_order("ORD-B", 101.0);
        let submit_c = make_submit_order("ORD-C", 102.0);
        oms.submit_order(&submit_a, StrategyId::new("TEST-STRAT"));
        oms.submit_order(&submit_b, StrategyId::new("TEST-STRAT"));
        oms.submit_order(&submit_c, StrategyId::new("TEST-STRAT"));

        // ORD-A: filled; ORD-B: never filled (left in Accepted); ORD-C: filled
        let fill_a = OrderFilled::new(
            TraderId::new("TRADER-001"),
            StrategyId::new("TEST-STRAT"),
            ClientOrderId::new("ORD-A"),
            VenueOrderId::new("VENUE-001"),
            PositionId::new("POS-001"),
            TradeId::new("T1"),
            "BTCUSDT.BINANCE".to_string(),
            OrderSide::Buy,
            1.0,
            100.0,
            1_000_000_100,
            1_000_000_000,
        );
        oms.apply_fill(&ClientOrderId::new("ORD-A"), &fill_a);
        let fill_c = OrderFilled::new(
            TraderId::new("TRADER-001"),
            StrategyId::new("TEST-STRAT"),
            ClientOrderId::new("ORD-C"),
            VenueOrderId::new("VENUE-001"),
            PositionId::new("POS-001"),
            TradeId::new("T2"),
            "BTCUSDT.BINANCE".to_string(),
            OrderSide::Buy,
            1.0,
            102.0,
            1_000_000_200,
            1_000_000_000,
        );
        oms.apply_fill(&ClientOrderId::new("ORD-C"), &fill_c);

        // Exchange reports empty (all closed server-side)
        let (oms_missing, _exchange_only) = oms.reconcile(&[]);

        // All 3 are "missing from exchange"
        // ORD-A and ORD-C have fills → marked Filled; ORD-B has no fills → marked Cancelled
        assert_eq!(oms_missing.len(), 3);

        let cache = cache.lock().unwrap();
        let order_a = cache.get_oms_order(&ClientOrderId::new("ORD-A")).unwrap();
        let order_b = cache.get_oms_order(&ClientOrderId::new("ORD-B")).unwrap();
        let order_c = cache.get_oms_order(&ClientOrderId::new("ORD-C")).unwrap();
        assert_eq!(order_a.state, OrderState::Filled);
        assert_eq!(order_b.state, OrderState::Cancelled); // had no fills → Cancelled
        assert_eq!(order_c.state, OrderState::Filled);
    }
}
