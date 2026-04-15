//! Order management — stop-loss, take-profit, and pending orders.

use super::{EngineContext, Signal};
use crate::instrument::InstrumentId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderType {
    Market,
    Limit,
    Stop,
    StopLimit, // triggers when price crosses stop level, fills immediately at current price
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone)]
pub struct Order {
    pub id: u64,
    pub instrument_id: InstrumentId,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub price: f64,
    pub size: f64,
    pub sl: Option<f64>,
    pub tp: Option<f64>,
    pub filled: bool,
    pub triggered: bool,
}

impl Order {
    pub fn new(
        id: u64,
        instrument_id: InstrumentId,
        side: OrderSide,
        order_type: OrderType,
        price: f64,
        size: f64,
    ) -> Self {
        Self {
            id,
            instrument_id,
            side,
            order_type,
            price,
            size,
            sl: None,
            tp: None,
            filled: false,
            triggered: false,
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
        side: OrderSide,
        order_type: OrderType,
        price: f64,
        size: f64,
    ) -> Order {
        let order = Order::new(self.next_id, instrument_id, side, order_type, price, size);
        self.next_id += 1;
        order
    }

    pub fn submit(&mut self, mut order: Order) {
        order.filled = false;
        order.triggered = false;
        self.pending_orders.push(order);
    }

    pub fn check_pending_orders(
        &mut self,
        current_price: f64,
        _ctx: &EngineContext,
    ) -> Option<Signal> {
        let mut to_fill = Vec::new();
        let mut to_remove = Vec::new();

        for (i, order) in self.pending_orders.iter_mut().enumerate() {
            if order.filled {
                to_remove.push(i);
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
                    // Buy stop: triggers when price rises to or above stop level, then fills at market
                    // Sell stop: triggers when price falls to or below stop level, then fills at market
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

        for i in to_fill.into_iter().rev() {
            self.pending_orders[i].filled = true;
            let filled_order = self.pending_orders.remove(i);
            self.filled_orders.push(filled_order);
        }

        None
    }

    pub fn check_sl_tp(&self, current_price: f64, ctx: &EngineContext) -> Option<Signal> {
        if ctx.position == 0.0 {
            return None;
        }

        let position_is_long = ctx.position > 0.0;

        if let Some(sl) = self.find_active_sl(ctx.position, current_price, position_is_long) {
            if sl.triggered {
                return Some(Signal::Close);
            }
        }

        if let Some(tp) = self.find_active_tp(ctx.position, current_price, position_is_long) {
            if tp.triggered {
                return Some(Signal::Close);
            }
        }

        None
    }

    fn find_active_sl(&self, _position: f64, current_price: f64, is_long: bool) -> Option<&Order> {
        self.pending_orders.iter().find(|o| {
            if !o.is_buy() {
                if let Some(sl_price) = o.sl {
                    if is_long {
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
        })
    }

    fn find_active_tp(&self, _position: f64, current_price: f64, is_long: bool) -> Option<&Order> {
        self.pending_orders.iter().find(|o| {
            if !o.is_buy() {
                if let Some(tp_price) = o.tp {
                    if is_long {
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
        })
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
        let order = Order::new(1, btc_id, OrderSide::Buy, OrderType::Limit, 100.0, 1.0)
            .with_sl(95.0)
            .with_tp(110.0);

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
        let ctx = EngineContext::new(10000.0);

        let order = manager.new_order(btc_id, OrderSide::Buy, OrderType::Limit, 100.0, 1.0);
        manager.submit(order);

        assert_eq!(manager.num_pending(), 1);

        manager.check_pending_orders(101.0, &ctx);
        assert_eq!(manager.num_pending(), 1);

        manager.check_pending_orders(100.0, &ctx);
        assert_eq!(manager.num_pending(), 0);
        assert_eq!(manager.num_filled(), 1);
    }

    #[test]
    fn test_stop_order_trigger() {
        let mut manager = OrderManager::new();
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let ctx = EngineContext::new(10000.0);

        let order = manager.new_order(btc_id, OrderSide::Sell, OrderType::Stop, 95.0, 1.0);
        manager.submit(order);

        assert_eq!(manager.num_pending(), 1);

        manager.check_pending_orders(96.0, &ctx);
        assert_eq!(manager.num_pending(), 1);

        manager.check_pending_orders(94.0, &ctx);
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
        let ctx = EngineContext::new(10000.0);

        // Buy StopLimit at 95 — triggers when price rises to or above 95
        let order = manager.new_order(btc_id, OrderSide::Buy, OrderType::StopLimit, 95.0, 1.0);
        manager.submit(order);

        assert_eq!(manager.num_pending(), 1);

        // Price still below trigger — no fill
        manager.check_pending_orders(94.0, &ctx);
        assert_eq!(manager.num_pending(), 1);

        // Price rises to trigger level — fills immediately
        manager.check_pending_orders(95.0, &ctx);
        assert_eq!(manager.num_pending(), 0);
        assert_eq!(manager.num_filled(), 1);
    }

    #[test]
    fn test_stoplimit_sell_triggers_on_fall() {
        let mut manager = OrderManager::new();
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let ctx = EngineContext::new(10000.0);

        // Sell StopLimit at 95 — triggers when price falls to or below 95
        let order = manager.new_order(btc_id, OrderSide::Sell, OrderType::StopLimit, 95.0, 1.0);
        manager.submit(order);

        assert_eq!(manager.num_pending(), 1);

        // Price still above trigger — no fill
        manager.check_pending_orders(96.0, &ctx);
        assert_eq!(manager.num_pending(), 1);

        // Price falls to trigger level — fills immediately
        manager.check_pending_orders(94.0, &ctx);
        assert_eq!(manager.num_pending(), 0);
        assert_eq!(manager.num_filled(), 1);
    }

    #[test]
    fn test_stoplimit_does_not_fill_before_trigger() {
        let mut manager = OrderManager::new();
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let ctx = EngineContext::new(10000.0);

        // Sell StopLimit at 95
        let order = manager.new_order(btc_id, OrderSide::Sell, OrderType::StopLimit, 95.0, 1.0);
        manager.submit(order);

        // Price far above — no fill
        for price in [100.0, 99.0, 97.0, 96.0].iter() {
            manager.check_pending_orders(*price, &ctx);
            assert_eq!(manager.num_pending(), 1, "price={}", price);
        }
        assert_eq!(manager.num_filled(), 0);
    }
}
