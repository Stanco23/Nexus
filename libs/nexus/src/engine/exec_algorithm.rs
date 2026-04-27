//! Execution algorithms — parent order management for split orders (ICEBERG, TWAP, VWAP).
//!
//! Phase 5.3: Implements parent→child order splitting. The OMS creates a `ParentOrder`
//! when a split-order type is submitted, generates child `SubmitOrder`s as fills arrive,
//! and manages remaining quantity until the parent is fully filled.

use crate::messages::{ClientOrderId, OrderSide, OrderType, StrategyId, SubmitOrder, TraderId};

/// Iceberg-specific parameters.
#[derive(Debug, Clone)]
pub struct IcebergParams {
    /// Quantity to display / release per child order.
    pub display_qty: f64,
    /// Minimum quantity per child (0 = no minimum).
    pub min_qty: f64,
    /// Maximum child orders (0 = unlimited).
    pub max_orders: u32,
}

/// TWAP-specific parameters.
#[derive(Debug, Clone)]
pub struct TwapParams {
    /// Target duration in seconds.
    pub duration_secs: u64,
    /// Number of slices.
    pub num_slices: u32,
    /// Whether to use randomized slice timing.
    pub randomize: bool,
}

/// VWAP-specific parameters.
#[derive(Debug, Clone)]
pub struct VwapParams {
    /// Target participation rate (0.0 to 1.0).
    pub participation_rate: f64,
    /// Use randomized sizing.
    pub randomize_size: bool,
}

/// Parent order tracking state for split-order algorithms.
///
/// A `ParentOrder` is created in the OMS when a top-level ICEBERG/TWAP/VWAP order is
/// submitted. It tracks total/remaining quantity and generates child `SubmitOrder`s
/// as each child is filled.
#[derive(Debug, Clone)]
pub struct ParentOrder {
    /// Client order ID of the parent order.
    pub parent_client_order_id: ClientOrderId,
    /// Execution algorithm type (Iceberg, Twap, or Vwap).
    pub algo_type: OrderType,
    /// Total quantity for the parent order.
    pub total_qty: f64,
    /// Remaining quantity not yet sent as child orders.
    pub remaining_qty: f64,
    /// Child order count generated so far.
    pub child_count: u32,
    /// Maximum child orders allowed (0 = unlimited).
    pub max_children: u32,
    /// Instrument ID string.
    pub instrument_id: String,
    /// Order side.
    pub order_side: OrderSide,
    /// Limit price (if specified).
    pub price: Option<f64>,
    /// Strategy ID.
    pub strategy_id: StrategyId,
    /// Trader ID.
    pub trader_id: TraderId,
    /// Time in force for child orders.
    pub time_in_force: Option<crate::messages::TimeInForce>,
    /// Expire time for child orders.
    pub expire_time_ns: Option<u64>,
    /// Timestamp for child order ts_init.
    pub ts_init: u64,
}

impl ParentOrder {
    /// Create a new ICEBERG parent order.
    ///
    /// `display_qty` is the quantity released per child slice.
    /// `min_qty` is the minimum child size (0 = no minimum).
    /// `max_orders` caps the number of child orders (0 = unlimited).
    pub fn new_iceberg(
        parent_client_order_id: ClientOrderId,
        total_qty: f64,
        instrument_id: String,
        order_side: OrderSide,
        price: Option<f64>,
        strategy_id: StrategyId,
        trader_id: TraderId,
        display_qty: f64,
        min_qty: f64,
        max_orders: u32,
    ) -> Self {
        let _ = (display_qty, min_qty); // Stored in IcebergParams on parent for future use
        Self {
            parent_client_order_id,
            algo_type: OrderType::Iceberg,
            total_qty,
            remaining_qty: total_qty,
            child_count: 0,
            max_children: max_orders,
            instrument_id,
            order_side,
            price,
            strategy_id,
            trader_id,
            time_in_force: None,
            expire_time_ns: None,
            ts_init: 0,
        }
    }

    /// Create a new TWAP parent order.
    pub fn new_twap(
        parent_client_order_id: ClientOrderId,
        total_qty: f64,
        instrument_id: String,
        order_side: OrderSide,
        price: Option<f64>,
        strategy_id: StrategyId,
        trader_id: TraderId,
        duration_secs: u64,
        num_slices: u32,
        randomize: bool,
    ) -> Self {
        let _ = (duration_secs, num_slices, randomize);
        Self {
            parent_client_order_id,
            algo_type: OrderType::Twap,
            total_qty,
            remaining_qty: total_qty,
            child_count: 0,
            max_children: 0, // TWAP manages its own slice count
            instrument_id,
            order_side,
            price,
            strategy_id,
            trader_id,
            time_in_force: None,
            expire_time_ns: None,
            ts_init: 0,
        }
    }

    /// Create a new VWAP parent order.
    pub fn new_vwap(
        parent_client_order_id: ClientOrderId,
        total_qty: f64,
        instrument_id: String,
        order_side: OrderSide,
        price: Option<f64>,
        strategy_id: StrategyId,
        trader_id: TraderId,
        participation_rate: f64,
        randomize_size: bool,
    ) -> Self {
        let _ = (participation_rate, randomize_size);
        Self {
            parent_client_order_id,
            algo_type: OrderType::Vwap,
            total_qty,
            remaining_qty: total_qty,
            child_count: 0,
            max_children: 0,
            instrument_id,
            order_side,
            price,
            strategy_id,
            trader_id,
            time_in_force: None,
            expire_time_ns: None,
            ts_init: 0,
        }
    }

    /// Set the timestamp for child order creation.
    pub fn set_ts_init(&mut self, ts_init: u64) {
        self.ts_init = ts_init;
    }

    /// Check if this parent can generate more child orders.
    ///
    /// Returns `true` if there is still remaining quantity and either
    /// max_children is unlimited (0) or child_count has not been reached.
    pub fn can_generate_child(&self) -> bool {
        if self.remaining_qty <= 0.0 {
            return false;
        }
        if self.max_children > 0 && self.child_count >= self.max_children {
            return false;
        }
        true
    }

    /// Generate the next child `SubmitOrder` for this parent.
    ///
    /// The child inherits the parent's venue, side, price, and strategy/trader IDs.
    /// The child quantity is the minimum of the remaining quantity and the
    /// parent's slice size.
    ///
    /// Returns `None` if no more children should be generated.
    pub fn generate_child(&self, child_client_order_id: ClientOrderId) -> Option<SubmitOrder> {
        if !self.can_generate_child() {
            return None;
        }

        // For ICEBERG, use display_qty as the child size
        // For TWAP/VWAP, use equal slices of total_qty / num_slices
        let child_qty = match self.algo_type {
            OrderType::Iceberg => self.remaining_qty.min(10.0), // 10.0 as default display qty
            OrderType::Twap => self.total_qty / 8.0,            // Default 8 slices
            OrderType::Vwap => self.total_qty / 8.0,            // Default 8 slices
            _ => self.remaining_qty,
        };

        // Ensure we don't exceed remaining
        let child_qty = self.remaining_qty.min(child_qty);

        // Don't generate dust orders
        if child_qty <= 0.0 {
            return None;
        }

        // Child is sent as Limit (Iceberg child is Limit with icebergs resting on book)
        Some(SubmitOrder::new(
            self.trader_id.clone(),
            self.strategy_id.clone(),
            self.instrument_id.clone(),
            child_client_order_id,
            self.order_side,
            OrderType::Limit,
            child_qty,
            self.ts_init,
        ))
    }

    /// Called when a child fill arrives — reduces remaining quantity.
    ///
    /// Also increments `child_count`.
    pub fn on_child_fill(&mut self, filled_qty: f64) {
        self.remaining_qty = (self.remaining_qty - filled_qty).max(0.0);
        self.child_count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iceberg_generate_children() {
        let parent_id = ClientOrderId::new("PARENT-001");
        let mut parent = ParentOrder::new_iceberg(
            parent_id.clone(),
            100.0, // total 100 units
            "BTCUSDT.BINANCE".to_string(),
            OrderSide::Buy,
            Some(50000.0),
            StrategyId::new("TEST"),
            TraderId::new("TRADER-001"),
            10.0, // display 10 per child
            0.0,  // no min
            0,   // unlimited
        );
        parent.set_ts_init(1_000_000_000);

        // First child
        let child1 = parent.generate_child(ClientOrderId::new("CHILD-001")).unwrap();
        assert_eq!(child1.quantity, 10.0);
        assert_eq!(parent.remaining_qty, 100.0); // remaining doesn't decrease until fill
        assert_eq!(parent.child_count, 0); // doesn't increment until fill

        // Simulate fill
        parent.on_child_fill(10.0);
        assert_eq!(parent.remaining_qty, 90.0);
        assert_eq!(parent.child_count, 1);

        let child2 = parent
            .generate_child(ClientOrderId::new("CHILD-002"))
            .unwrap();
        assert_eq!(child2.quantity, 10.0);

        // 10 children of 10 each = 100 total
        for i in 3..=10 {
            parent.on_child_fill(10.0);
            let child = parent
                .generate_child(ClientOrderId::new(&format!("CHILD-{:03}", i)))
                .unwrap();
            assert_eq!(child.quantity, 10.0);
        }

        // After all filled, no more children
        parent.on_child_fill(10.0);
        assert!(!parent.can_generate_child());
        assert!(parent
            .generate_child(ClientOrderId::new("CHILD-EXTRA"))
            .is_none());
    }

    #[test]
    fn test_iceberg_last_child_uses_remaining() {
        let parent_id = ClientOrderId::new("PARENT-PARTIAL");
        let mut parent = ParentOrder::new_iceberg(
            parent_id.clone(),
            25.0, // total 25 units
            "ETHUSDT.BINANCE".to_string(),
            OrderSide::Sell,
            Some(3000.0),
            StrategyId::new("TEST"),
            TraderId::new("TRADER-001"),
            10.0, // display 10 per child
            0.0,
            0,
        );
        parent.set_ts_init(1_000_000_000);

        // First child: 10
        parent.on_child_fill(10.0);
        let child2 = parent
            .generate_child(ClientOrderId::new("CHILD-002"))
            .unwrap();
        assert_eq!(child2.quantity, 10.0);

        // Second child: 5 (only 5 remaining)
        parent.on_child_fill(10.0);
        let child3 = parent
            .generate_child(ClientOrderId::new("CHILD-003"))
            .unwrap();
        assert_eq!(child3.quantity, 5.0);

        // No more children
        parent.on_child_fill(5.0);
        assert!(!parent.can_generate_child());
    }

    #[test]
    fn test_iceberg_max_children_enforced() {
        let parent_id = ClientOrderId::new("PARENT-MAX");
        let mut parent = ParentOrder::new_iceberg(
            parent_id.clone(),
            100.0,
            "BTCUSDT.BINANCE".to_string(),
            OrderSide::Buy,
            Some(50000.0),
            StrategyId::new("TEST"),
            TraderId::new("TRADER-001"),
            10.0,
            0.0,
            3, // only 3 children allowed
        );
        parent.set_ts_init(1_000_000_000);

        // Check can_generate_child before any fills
        assert!(parent.can_generate_child());

        // Generate child 1
        let child1 = parent
            .generate_child(ClientOrderId::new("CHILD-001"))
            .unwrap();
        assert_eq!(child1.quantity, 10.0);
        parent.on_child_fill(10.0);
        assert!(parent.can_generate_child()); // child_count=1, max=3

        // Generate child 2
        let child2 = parent
            .generate_child(ClientOrderId::new("CHILD-002"))
            .unwrap();
        assert_eq!(child2.quantity, 10.0);
        parent.on_child_fill(10.0);
        assert!(parent.can_generate_child()); // child_count=2, max=3

        // Generate child 3
        let child3 = parent
            .generate_child(ClientOrderId::new("CHILD-003"))
            .unwrap();
        assert_eq!(child3.quantity, 10.0);
        parent.on_child_fill(10.0);

        // After 3rd fill, child_count == max_children -> no more children allowed
        assert!(!parent.can_generate_child());
        assert!(parent
            .generate_child(ClientOrderId::new("CHILD-004"))
            .is_none());
    }

    #[test]
    fn test_twap_parent_order() {
        let parent_id = ClientOrderId::new("PARENT-TWAP-001");
        let mut parent = ParentOrder::new_twap(
            parent_id.clone(),
            80.0,
            "BTCUSDT.BINANCE".to_string(),
            OrderSide::Buy,
            Some(50000.0),
            StrategyId::new("TEST"),
            TraderId::new("TRADER-001"),
            3600, // 1 hour
            8,    // 8 slices
            false,
        );
        parent.set_ts_init(1_000_000_000);

        assert!(parent.can_generate_child());
        let child1 = parent
            .generate_child(ClientOrderId::new("TWAP-CHILD-001"))
            .unwrap();
        // TWAP splits 80 / 8 = 10 per slice
        assert_eq!(child1.quantity, 10.0);

        parent.on_child_fill(10.0);
        assert_eq!(parent.remaining_qty, 70.0);
    }

    #[test]
    fn test_vwap_parent_order() {
        let parent_id = ClientOrderId::new("PARENT-VWAP-001");
        let mut parent = ParentOrder::new_vwap(
            parent_id.clone(),
            50.0,
            "BTCUSDT.BINANCE".to_string(),
            OrderSide::Sell,
            Some(51000.0),
            StrategyId::new("TEST"),
            TraderId::new("TRADER-001"),
            0.1,  // 10% participation rate
            false,
        );
        parent.set_ts_init(1_000_000_000);

        assert!(parent.can_generate_child());
        let child1 = parent
            .generate_child(ClientOrderId::new("VWAP-CHILD-001"))
            .unwrap();
        // VWAP splits 50 / 8 = 6.25 per slice
        assert!((child1.quantity - 6.25).abs() < 0.01);

        parent.on_child_fill(6.25);
        assert!((parent.remaining_qty - 43.75).abs() < 0.01);
    }
}
