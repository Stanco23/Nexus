//! Order management — stop-loss, take-profit, and pending orders.

use serde::{Deserialize, Serialize};
use crate::engine::core::Signal;
use crate::instrument::{InstrumentId, Venue};
use crate::messages::{ClientOrderId, PositionId, StrategyId, TimeInForce};

/// Trailing offset type determines how the offset is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrailingOffsetType {
    /// Trailing offset is a absolute price value.
    Absolute,
    /// Trailing offset is a percentage of the price (0.01 = 1%).
    Percentage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    Market,
    Limit,
    Stop,
    StopLimit,
    MarketIfTouched,
    LimitIfTouched,
    TrailingStopMarket,
    TrailingStopLimit,
    MarketToLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerType {
    Default,
    BidAsk,
    LastPrice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide {
    Buy,
    Sell,
}

/// Contingency type for linked orders (OCO, OTO, OTOCA).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContingencyType {
    None,
    /// One-Triggers-The-Other: when order A fills, order B submits automatically
    OneTriggersTheOther,
    /// One-Cancels-The-Other: when order A fills/cancels, linked orders cancel
    OneCancelsTheOther,
    /// One-Triggers-One-Cancels-All: chain of orders
    OneTriggersOneCancelsAll,
}

/// Identifier for an order list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrderListId(pub u64);

impl OrderListId {
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for OrderListId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An order list grouping multiple orders that submit together.
/// Linked contingency orders share a common order_list_id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderList {
    pub id: OrderListId,
    pub orders: Vec<ClientOrderId>,
}

impl OrderList {
    pub fn new(id: OrderListId, orders: Vec<ClientOrderId>) -> Self {
        Self { id, orders }
    }
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
    pub time_in_force: Option<TimeInForce>,
    pub expire_time_ns: Option<u64>,
    /// Trailing delta for trailing stop orders.
    pub trailing_delta: Option<f64>,
    /// Trailing offset value for trailing stop orders.
    pub trailing_offset: Option<f64>,
    /// The type of trailing offset (Absolute or Percentage).
    pub trailing_offset_type: Option<TrailingOffsetType>,
    /// Optional activation price - trailing starts when market reaches this price.
    pub activation_price: Option<f64>,
    /// Whether the order has been activated (for orders with activation_price).
    pub is_activated: bool,
    /// The current trigger price for trailing stop orders (updated as market moves).
    pub trigger_price: Option<f64>,
    /// The limit offset for TrailingStopLimit orders.
    pub limit_offset: Option<f64>,
    /// Contingency type for linked orders (OCO, OTO, OTOCA).
    pub contingency_type: ContingencyType,
    /// Linked order IDs for OCO/OTO chains.
    pub linked_order_ids: Vec<ClientOrderId>,
    /// Order list ID for grouping orders that submit together.
    pub order_list_id: Option<OrderListId>,
    /// The trigger type for MIT/LIT orders.
    pub trigger_type: TriggerType,
    /// Post-only order flag — reject if order would cross the spread (maker only).
    pub post_only: bool,
    /// Reduce-only order flag — reject if order would open/increase a position.
    pub reduce_only: bool,
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
            time_in_force: None,
            expire_time_ns: None,
            trailing_delta: None,
            trailing_offset: None,
            trailing_offset_type: None,
            activation_price: None,
            is_activated: false,
            trigger_price: None,
            limit_offset: None,
            contingency_type: ContingencyType::None,
            linked_order_ids: Vec::new(),
            order_list_id: None,
            trigger_type: TriggerType::Default,
            post_only: false,
            reduce_only: false,
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

    pub fn with_trailing_offset(mut self, offset: f64, offset_type: TrailingOffsetType) -> Self {
        self.trailing_offset = Some(offset);
        self.trailing_offset_type = Some(offset_type);
        self
    }

    pub fn with_activation_price(mut self, price: f64) -> Self {
        self.activation_price = Some(price);
        self
    }

    pub fn with_limit_offset(mut self, offset: f64) -> Self {
        self.limit_offset = Some(offset);
        self
    }

    /// Set the post-only flag (maker-only, reject if would cross spread).
    pub fn with_post_only(mut self) -> Self {
        self.post_only = true;
        self
    }

    /// Set the reduce-only flag (close-only, reject if would open/increase position).
    pub fn with_reduce_only(mut self) -> Self {
        self.reduce_only = true;
        self
    }

    /// Update the trailing trigger price based on current market conditions.
    /// For BUY: trigger_price = market_low - trailing_offset (moves up as price rises)
    /// For SELL: trigger_price = market_high + trailing_offset (moves down as price falls)
    pub fn update_trailing_trigger_price(&mut self, market_price: f64, market_high: f64, market_low: f64) {
        // If has activation price, only start trailing once market reaches it
        if let Some(activation) = self.activation_price {
            if !self.is_activated {
                if self.is_buy() {
                    if market_price < activation {
                        return; // Not yet activated
                    }
                } else {
                    if market_price > activation {
                        return; // Not yet activated
                    }
                }
                self.is_activated = true;
            }
        }

        let offset = match self.trailing_offset {
            Some(o) => o,
            None => return,
        };

        let offset_value = match self.trailing_offset_type {
            Some(TrailingOffsetType::Percentage) => market_price * offset,
            Some(TrailingOffsetType::Absolute) | None => offset,
        };

        let new_trigger_price = if self.is_buy() {
            // BUY trailing stop: trigger moves UP as price rises
            let calculated = market_low - offset_value;
            match self.trigger_price {
                Some(current) if calculated > current => calculated,
                Some(current) => current,
                None => calculated,
            }
        } else {
            // SELL trailing stop: trigger moves DOWN as price falls
            let calculated = market_high + offset_value;
            match self.trigger_price {
                Some(current) if calculated < current => calculated,
                Some(current) => current,
                None => calculated,
            }
        };

        self.trigger_price = Some(new_trigger_price);
    }

    /// Check if the trailing stop order should trigger based on current price.
    /// Returns true if the market has moved past the trigger price in the adverse direction.
    pub fn check_trailing_stop_trigger(&self, current_price: f64) -> bool {
        // If has activation price, must be activated first
        if let Some(_activation) = self.activation_price {
            if !self.is_activated {
                return false;
            }
        }

        let trigger = match self.trigger_price {
            Some(t) => t,
            None => return false,
        };

        if self.is_buy() {
            // BUY trailing stop triggers when price falls to or below trigger
            current_price <= trigger
        } else {
            // SELL trailing stop triggers when price rises to or above trigger
            current_price >= trigger
        }
    }

    pub fn trigger_price_display(&self) -> Option<f64> {
        match self.order_type {
            OrderType::Stop if !self.triggered => Some(self.price),
            OrderType::MarketIfTouched | OrderType::LimitIfTouched if !self.triggered => {
                self.trigger_price.or(Some(self.price))
            }
            OrderType::TrailingStopMarket | OrderType::TrailingStopLimit => self.trigger_price,
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
    check_pending_orders_with_market(
        pending_orders,
        filled_orders,
        current_price,
        current_price,
        current_price,
        0,
    )
}

/// Extended pending order check with market high/low for trailing stop updates.
pub fn check_pending_orders_with_market(
    pending_orders: &mut Vec<Order>,
    filled_orders: &mut Vec<Order>,
    current_price: f64,
    market_high: f64,
    market_low: f64,
    timestamp_ns: u64,
) -> Option<Signal> {
    let mut to_fill = Vec::new();

    for (i, order) in pending_orders.iter_mut().enumerate() {
        if order.filled {
            continue;
        }

        // GAP 1: GTD — expire orders whose expire_time_ns has passed
        if timestamp_ns != 0 {
            if let Some(expire_ns) = order.expire_time_ns {
                if timestamp_ns >= expire_ns {
                    order.filled = true;
                    continue;
                }
            }
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
                    // GAP 3: post_only — reject if would cross spread (taker)
                    if order.post_only {
                        continue;
                    }
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
            OrderType::MarketIfTouched | OrderType::LimitIfTouched => {
                // MIT/LIT trigger logic (opposite of stops):
                // BUY: triggers when market touches DOWN to trigger_price (ask <= trigger_price)
                // SELL: triggers when market touches UP to trigger_price (bid >= trigger_price)
                let trigger = order.trigger_price.unwrap_or(order.price);
                let crosses = if order.is_buy() {
                    current_price <= trigger
                } else {
                    current_price >= trigger
                };
                if crosses && !order.triggered {
                    order.triggered = true;
                    to_fill.push(i);
                }
            }
            OrderType::TrailingStopMarket | OrderType::TrailingStopLimit => {
                // Update trailing trigger price based on market movement
                order.update_trailing_trigger_price(current_price, market_high, market_low);

                // Check if should trigger (market moved adversely past trigger price)
                if order.check_trailing_stop_trigger(current_price) && !order.triggered {
                    order.triggered = true;
                    to_fill.push(i);
                }
            }
            OrderType::MarketToLimit => {
                // Market-to-limit: triggers like a market order and converts to limit at last price
                // For pending orders, we treat it like a limit order at the trigger price
                if order.triggered {
                    to_fill.push(i);
                }
            }
        }
    }

    let mut fill_signals: Vec<Signal> = Vec::new();
    // GAP 2: collect IDs of filled OCO orders for cascade cancel
    let mut oco_filled_client_ids: Vec<String> = Vec::new();

    for i in to_fill.into_iter().rev() {
        // Check OCO before cloning so we still have stable indices
        if !pending_orders[i].linked_order_ids.is_empty()
            && pending_orders[i].contingency_type == ContingencyType::OneCancelsTheOther
        {
            oco_filled_client_ids.push(pending_orders[i].client_order_id.0.clone());
        }

        pending_orders[i].filled = true;
        let removed = pending_orders.remove(i);
        let sig = if removed.side == OrderSide::Buy {
            Signal::Buy
        } else {
            Signal::Sell
        };
        fill_signals.push(sig);
        filled_orders.push(removed);
    }

    // GAP 2: OCO cascade — remove any pending linked orders of filled OCO orders
    if !oco_filled_client_ids.is_empty() {
        pending_orders.retain(|o| {
            !oco_filled_client_ids.iter().any(|filled_id| {
                o.linked_order_ids.iter().any(|id| id.0 == *filled_id)
            })
        });
    }

    if fill_signals.len() == 1 {
        Some(fill_signals[0].clone())
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

    #[test]
    fn test_trailing_stop_buy_updates_trigger() {
        // Test that BUY trailing stop updates trigger price as market rises
        let mut order = Order::new(
            1,
            ClientOrderId::new("test-trailing"),
            StrategyId::new("test-strategy"),
            InstrumentId::new("BTCUSDT", "BINANCE"),
            Venue::new("BINANCE"),
            OrderSide::Buy,
            OrderType::TrailingStopMarket,
            0.0,
            1.0,
            None,
            None,
        )
        .with_trailing_offset(100.0, TrailingOffsetType::Absolute);

        // Simulate market going up: price rises from 50000 to 51000
        // For BUY trailing stop, trigger should move UP (protecting more profit)
        order.update_trailing_trigger_price(50000.0, 50000.0, 50000.0);
        assert_eq!(order.trigger_price, Some(49900.0)); // 50000 - 100

        // Market rises to 51000, trigger should move up to 50900
        order.update_trailing_trigger_price(51000.0, 51000.0, 51000.0);
        assert_eq!(order.trigger_price, Some(50900.0));

        // Market falls back to 50500, trigger should NOT go lower (stays at 50900)
        order.update_trailing_trigger_price(50500.0, 51000.0, 50500.0);
        assert_eq!(order.trigger_price, Some(50900.0));
    }

    #[test]
    fn test_trailing_stop_sell_updates_trigger() {
        // Test that SELL trailing stop updates trigger price as market falls
        let mut order = Order::new(
            1,
            ClientOrderId::new("test-trailing"),
            StrategyId::new("test-strategy"),
            InstrumentId::new("BTCUSDT", "BINANCE"),
            Venue::new("BINANCE"),
            OrderSide::Sell,
            OrderType::TrailingStopMarket,
            0.0,
            1.0,
            None,
            None,
        )
        .with_trailing_offset(100.0, TrailingOffsetType::Absolute);

        // Simulate market going down: price falls from 50000 to 49000
        // For SELL trailing stop, trigger should move DOWN (protecting more profit)
        order.update_trailing_trigger_price(50000.0, 50000.0, 50000.0);
        assert_eq!(order.trigger_price, Some(50100.0)); // 50000 + 100

        // Market falls to 49000, trigger stays at 50100 (market_high still 50000 from last high)
        order.update_trailing_trigger_price(49000.0, 50000.0, 49000.0);
        assert_eq!(order.trigger_price, Some(50100.0));

        // Market rises back to 49500, trigger stays at 50100 (doesn't trail upward for SELL)
        order.update_trailing_trigger_price(49500.0, 50000.0, 49500.0);
        assert_eq!(order.trigger_price, Some(50100.0));
    }

    #[test]
    fn test_trailing_stop_with_activation_price() {
        // Test that activation price must be reached before trailing starts
        let mut order = Order::new(
            1,
            ClientOrderId::new("test-trailing"),
            StrategyId::new("test-strategy"),
            InstrumentId::new("BTCUSDT", "BINANCE"),
            Venue::new("BINANCE"),
            OrderSide::Buy,
            OrderType::TrailingStopMarket,
            0.0,
            1.0,
            None,
            None,
        )
        .with_trailing_offset(100.0, TrailingOffsetType::Absolute)
        .with_activation_price(50000.0);

        // Market below activation price - should not update trigger
        order.update_trailing_trigger_price(49000.0, 49000.0, 49000.0);
        assert_eq!(order.trigger_price, None);
        assert!(!order.is_activated);

        // Market reaches activation price
        order.update_trailing_trigger_price(50000.0, 50000.0, 50000.0);
        assert!(order.is_activated);
        assert_eq!(order.trigger_price, Some(49900.0));

        // Market continues to rise
        order.update_trailing_trigger_price(51000.0, 51000.0, 51000.0);
        assert_eq!(order.trigger_price, Some(50900.0));
    }

    #[test]
    fn test_trailing_stop_trigger_condition() {
        // Test that trailing stop only triggers when price moves against position
        let mut order = Order::new(
            1,
            ClientOrderId::new("test-trailing"),
            StrategyId::new("test-strategy"),
            InstrumentId::new("BTCUSDT", "BINANCE"),
            Venue::new("BINANCE"),
            OrderSide::Sell,
            OrderType::TrailingStopMarket,
            0.0,
            1.0,
            None,
            None,
        )
        .with_trailing_offset(100.0, TrailingOffsetType::Absolute);

        // Set initial trigger at 50100 (50000 + 100)
        order.update_trailing_trigger_price(50000.0, 50000.0, 50000.0);
        assert_eq!(order.trigger_price, Some(50100.0));

        // Market falls to 49000 (favorable for SELL) - should NOT trigger
        assert!(!order.check_trailing_stop_trigger(49000.0));

        // Market rises to 50200 (adverse for SELL) - should trigger
        assert!(order.check_trailing_stop_trigger(50200.0));
    }
}
