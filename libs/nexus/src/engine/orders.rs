//! Order management — stop-loss, take-profit, and pending orders.

use serde::{Deserialize, Serialize};
use crate::engine::core::Signal;
use crate::instrument::{InstrumentId, Venue};
use crate::messages::{ClientOrderId, PositionId, StrategyId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    Market,
    Limit,
    Stop,
    StopLimit, // triggers when price crosses stop level, fills immediately at current price
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: u64,
    pub client_order_id: ClientOrderId,
    pub strategy_id: StrategyId,
    pub instrument_id: InstrumentId,
    pub venue: Venue,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub price: f64,
    pub size: f64,
    pub sl: Option<f64>,
    pub tp: Option<f64>,
    pub filled: bool,
    pub triggered: bool,
    pub position_id: Option<PositionId>,
}

impl Order {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u64,
        client_order_id: ClientOrderId,
        strategy_id: StrategyId,
        instrument_id: InstrumentId,
        venue: Venue,
        side: OrderSide,
        order_type: OrderType,
        price: f64,
        size: f64,
        sl: Option<f64>,
        tp: Option<f64>,
    ) -> Self {
        Self {
            id,
            client_order_id,
            strategy_id,
            instrument_id,
            venue,
            side,
            order_type,
            price,
            size,
            sl,
            tp,
            filled: false,
            triggered: false,
            position_id: None,
        }
    }

    pub fn with_sl(mut self, sl: f64) -> Self {
        self.sl = Some(sl);
        self
    }

    pub fn with_tp(mut self, tp: f64) -> Self {
        self.tp = Some(tp);
        self
    }

    pub fn is_buy(&self) -> bool {
        self.side == OrderSide::Buy
    }

    pub fn is_sell(&self) -> bool {
        self.side == OrderSide::Sell
    }

    pub fn trigger_price(&self) -> Option<f64> {
        match self.order_type {
            OrderType::Stop if !self.triggered => Some(self.price),
            _ => None,
        }
    }
}

/// Standalone pending order check — evaluates limit orders for fill conditions.
pub fn check_pending_orders(
    pending_orders: &mut Vec<Order>,
    filled_orders: &mut Vec<Order>,
    current_price: f64,
) -> Option<Signal> {
    let mut to_fill = Vec::new();

    for (i, order) in pending_orders.iter_mut().enumerate() {
        if order.filled {
            continue;
        }

        match order.order_type {
            OrderType::Market => {
                if order.triggered {
                    to_fill.push(i);
                }
            }
            OrderType::Limit => {
                let crosses = if order.is_buy() {
                    current_price <= order.price
                } else {
                    current_price >= order.price
                };
                if crosses {
                    to_fill.push(i);
                }
            }
            OrderType::Stop => {
                let crosses = if order.is_buy() {
                    current_price >= order.price
                } else {
                    current_price <= order.price
                };
                if crosses {
                    order.triggered = true;
                    to_fill.push(i);
                }
            }
            OrderType::StopLimit => {
                let crosses = if order.is_buy() {
                    current_price >= order.price
                } else {
                    current_price <= order.price
                };
                if crosses {
                    to_fill.push(i);
                }
            }
        }
    }

    let mut fill_signals: Vec<Signal> = Vec::new();

    for i in to_fill.into_iter().rev() {
        pending_orders[i].filled = true;
        let filled_order = pending_orders.remove(i);
        let sig = if filled_order.side == OrderSide::Buy {
            Signal::Buy
        } else {
            Signal::Sell
        };
        fill_signals.push(sig);
        filled_orders.push(filled_order);
    }

    if fill_signals.len() == 1 {
        Some(fill_signals[0])
    } else if fill_signals.len() > 1 {
        Some(Signal::Close)
    } else {
        None
    }
}

/// Standalone SL/TP check — evaluates stop-loss and take-profit conditions.
pub fn check_sl_tp(pending_orders: &[Order], position: f64, current_price: f64) -> Option<Signal> {
    if position == 0.0 {
        return None;
    }

    let position_is_long = position > 0.0;

    // Check stop-loss
    if let Some(sl_order) = pending_orders.iter().find(|o| {
        if !o.is_buy() {
            if let Some(sl_price) = o.sl {
                if position_is_long {
                    current_price <= sl_price
                } else {
                    current_price >= sl_price
                }
            } else {
                false
            }
        } else {
            false
        }
    }) {
        if sl_order.triggered {
            return Some(Signal::Close);
        }
    }

    // Check take-profit
    if let Some(tp_order) = pending_orders.iter().find(|o| {
        if !o.is_buy() {
            if let Some(tp_price) = o.tp {
                if position_is_long {
                    current_price >= tp_price
                } else {
                    current_price <= tp_price
                }
            } else {
                false
            }
        } else {
            false
        }
    }) {
        if tp_order.triggered {
            return Some(Signal::Close);
        }
    }

    None
}

pub struct OrderManager {
    next_id: u64,
    pending_orders: Vec<Order>,
    filled_orders: Vec<Order>,
}

impl OrderManager {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            pending_orders: Vec::new(),
            filled_orders: Vec::new(),
        }
    }

    pub fn new_order(
        &mut self,
        instrument_id: InstrumentId,
        venue: Venue,
        side: OrderSide,
        order_type: OrderType,
        price: f64,
        size: f64,
    ) -> Order {
        let order = Order::new(
            self.next_id,
            ClientOrderId::new(&format!("order-{}", self.next_id)),
            StrategyId::new("test-strategy"),
            instrument_id,
            venue,
            side,
            order_type,
            price,
            size,
            None,
            None,
        );
        self.next_id += 1;
        order
    }

    pub fn submit(&mut self, mut order: Order) {
        order.filled = false;
        order.triggered = false;
        self.pending_orders.push(order);
    }

    pub fn pending_orders(&self) -> &[Order] {
        &self.pending_orders
    }

    pub fn check_pending_orders(&mut self, current_price: f64) -> Option<Signal> {
        check_pending_orders(&mut self.pending_orders, &mut self.filled_orders, current_price)
    }

    pub fn check_sl_tp(&self, current_price: f64, position: f64) -> Option<Signal> {
        check_sl_tp(&self.pending_orders, position, current_price)
    }

    pub fn num_pending(&self) -> usize {
        self.pending_orders.len()
    }

    pub fn num_filled(&self) -> usize {
        self.filled_orders.len()
    }

    pub fn clear_pending(&mut self) {
        self.pending_orders.clear();
    }
}

impl Default for OrderManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_order_builder() {
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let venue = Venue::new("BINANCE");
        let order = Order::new(
            1,
            ClientOrderId::new("test-order-1"),
            StrategyId::new("test-strategy"),
            btc_id,
            venue,
            OrderSide::Buy,
            OrderType::Limit,
            100.0,
            1.0,
            Some(95.0),
            Some(110.0),
        );

        assert_eq!(order.id, 1);
        assert!(order.is_buy());
        assert!(!order.is_sell());
        assert_eq!(order.order_type, OrderType::Limit);
        assert_eq!(order.sl, Some(95.0));
        assert_eq!(order.tp, Some(110.0));
        assert!(!order.filled);
        assert!(!order.triggered);
    }

    #[test]
    fn test_limit_order_fill() {
        let mut manager = OrderManager::new();
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let venue = Venue::new("BINANCE");

        let order = manager.new_order(btc_id, venue, OrderSide::Buy, OrderType::Limit, 100.0, 1.0);
        manager.submit(order);

        assert_eq!(manager.num_pending(), 1);

        manager.check_pending_orders(101.0);
        assert_eq!(manager.num_pending(), 1);

        manager.check_pending_orders(100.0);
        assert_eq!(manager.num_pending(), 0);
        assert_eq!(manager.num_filled(), 1);
    }

    #[test]
    fn test_stop_order_trigger() {
        let mut manager = OrderManager::new();
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let venue = Venue::new("BINANCE");

        let order = manager.new_order(btc_id, venue, OrderSide::Sell, OrderType::Stop, 95.0, 1.0);
        manager.submit(order);

        assert_eq!(manager.num_pending(), 1);

        manager.check_pending_orders(96.0);
        assert_eq!(manager.num_pending(), 1);

        manager.check_pending_orders(94.0);
        assert_eq!(manager.num_pending(), 0);
        assert_eq!(manager.num_filled(), 1);
    }

    #[test]
    fn test_order_manager_default() {
        let manager = OrderManager::default();
        assert_eq!(manager.num_pending(), 0);
        assert_eq!(manager.num_filled(), 0);
    }

    #[test]
    fn test_stoplimit_buy_triggers_on_rise() {
        let mut manager = OrderManager::new();
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let venue = Venue::new("BINANCE");

        // Buy StopLimit at 95 — triggers when price rises to or above 95
        let order = manager.new_order(btc_id, venue, OrderSide::Buy, OrderType::StopLimit, 95.0, 1.0);
        manager.submit(order);

        assert_eq!(manager.num_pending(), 1);

        // Price still below trigger — no fill
        manager.check_pending_orders(94.0);
        assert_eq!(manager.num_pending(), 1);

        // Price rises to trigger level — fills immediately
        manager.check_pending_orders(95.0);
        assert_eq!(manager.num_pending(), 0);
        assert_eq!(manager.num_filled(), 1);
    }

    #[test]
    fn test_stoplimit_sell_triggers_on_fall() {
        let mut manager = OrderManager::new();
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let venue = Venue::new("BINANCE");

        // Sell StopLimit at 95 — triggers when price falls to or below 95
        let order = manager.new_order(btc_id, venue, OrderSide::Sell, OrderType::StopLimit, 95.0, 1.0);
        manager.submit(order);

        assert_eq!(manager.num_pending(), 1);

        // Price still above trigger — no fill
        manager.check_pending_orders(96.0);
        assert_eq!(manager.num_pending(), 1);

        // Price falls to trigger level — fills immediately
        manager.check_pending_orders(94.0);
        assert_eq!(manager.num_pending(), 0);
        assert_eq!(manager.num_filled(), 1);
    }

    #[test]
    fn test_stoplimit_does_not_fill_before_trigger() {
        let mut manager = OrderManager::new();
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let venue = Venue::new("BINANCE");

        // Sell StopLimit at 95
        let order = manager.new_order(btc_id, venue, OrderSide::Sell, OrderType::StopLimit, 95.0, 1.0);
        manager.submit(order);

        // Price far above — no fill
        for price in [100.0, 99.0, 97.0, 96.0].iter() {
            manager.check_pending_orders(*price);
            assert_eq!(manager.num_pending(), 1, "price={}", price);
        }
        assert_eq!(manager.num_filled(), 0);
    }
}
