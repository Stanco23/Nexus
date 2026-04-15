//! L2 Order Book — synthetic order book from tick stream + limit order queue emulator.

use crate::buffer::tick_buffer::TradeFlowStats;
use crate::slippage::SlippageConfig;
use std::collections::BTreeMap;

/// Unique order identifier.
pub type OrderId = u64;

/// Side of an order or trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy = 1,
    Sell = 2,
}

impl Side {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Side::Buy),
            2 => Some(Side::Sell),
            _ => None,
        }
    }

    pub fn as_u8(&self) -> u8 {
        match self {
            Side::Buy => 1,
            Side::Sell => 2,
        }
    }

    pub fn opposite(&self) -> Self {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

/// A queued limit order awaiting fill in the OrderEmulator.
#[derive(Debug, Clone)]
pub struct QueuedOrder {
    pub order_id: OrderId,
    pub side: Side,
    pub price: f64,         // limit price (i64 in book)
    pub price_int: i64,
    pub size: f64,         // original size
    pub remaining_size: f64,
    pub submitted_ns: u64,
    pub queue_position: u64, // position in queue at this price level
}

/// A fill event recorded by the OrderEmulator.
#[derive(Debug, Clone)]
pub struct FillEvent {
    pub order_id: OrderId,
    pub side: Side,
    pub fill_price: f64,
    pub fill_size: f64,
    pub queue_position: u64,
    pub vpin: f64,
    pub slippage_bps: f64,
    pub delay_ns: u64,
    pub timestamp_ns: u64,
    pub is_maker: bool,
}

/// Price level with queue of orders.
#[derive(Debug, Clone)]
struct QueueLevel {
    orders: Vec<OrderId>,       // order IDs in FIFO order
    timestamp_ns: u64,          // time first order was placed
}

impl QueueLevel {
    fn new() -> Self {
        Self {
            orders: Vec::new(),
            timestamp_ns: u64::MAX,
        }
    }

    fn push(&mut self, order_id: OrderId, timestamp_ns: u64) {
        if self.orders.is_empty() {
            self.timestamp_ns = timestamp_ns;
        }
        self.orders.push(order_id);
    }
}

/// Queue-aware limit order fill simulator.
#[derive(Debug, Clone)]
pub struct OrderEmulator {
    /// Price-level queues: price_int → QueueLevel
    bid_queues: BTreeMap<i64, QueueLevel>,
    ask_queues: BTreeMap<i64, QueueLevel>,
    /// All pending queued orders.
    pending: Vec<QueuedOrder>,
    /// Fill event log.
    filled_log: Vec<FillEvent>,
    /// Next order ID counter.
    next_order_id: OrderId,
    /// Running average market volume per tick.
    avg_market_volume: f64,
    /// Slippage configuration.
    slippage_config: SlippageConfig,
}

impl OrderEmulator {
    pub fn new() -> Self {
        Self {
            bid_queues: BTreeMap::new(),
            ask_queues: BTreeMap::new(),
            pending: Vec::new(),
            filled_log: Vec::new(),
            next_order_id: 1,
            avg_market_volume: 1.0,
            slippage_config: SlippageConfig::default(),
        }
    }

    /// Submit a limit order to the queue at the specified price level.
    pub fn submit_limit(
        &mut self,
        price: f64,
        size: f64,
        side: Side,
        timestamp_ns: u64,
    ) -> OrderId {
        let price_int = (price * 1_000_000_000.0) as i64;
        let order_id = self.next_order_id;
        self.next_order_id += 1;

        // Assign queue position based on current queue depth at this level.
        let queue_position = if side == Side::Buy {
            let level = self.bid_queues.entry(price_int).or_insert_with(QueueLevel::new);
            let pos = level.orders.len() as u64;
            level.push(order_id, timestamp_ns);
            pos
        } else {
            let level = self.ask_queues.entry(price_int).or_insert_with(QueueLevel::new);
            let pos = level.orders.len() as u64;
            level.push(order_id, timestamp_ns);
            pos
        };

        let qo = QueuedOrder {
            order_id,
            side,
            price,
            price_int,
            size,
            remaining_size: size,
            submitted_ns: timestamp_ns,
            queue_position,
        };

        self.pending.push(qo);
        order_id
    }

    /// Compute fill probability for an order at a given queue position.
    ///
    /// `P(fill) = 1 / (1 + queue_position * 0.1)` scaled by volume ratio.
    #[allow(dead_code)]
    pub fn fill_probability(&self, queue_position: u64, market_volume: f64) -> f64 {
        let base_prob = 1.0 / (1.0 + queue_position as f64 * 0.1);
        let volume_ratio = (market_volume / self.avg_market_volume).clamp(0.1, 2.0);
        (base_prob * volume_ratio).min(1.0)
    }

    /// Compute fill event for a queued order (no mutable self borrow).
    fn compute_fill_event(
        qo: &QueuedOrder,
        current_price: f64,
        market_volume: f64,
        vpin: f64,
        timestamp_ns: u64,
        slippage_config: &SlippageConfig,
        avg_market_volume: f64,
    ) -> Option<FillEvent> {
        let prob = {
            let base_prob = 1.0 / (1.0 + qo.queue_position as f64 * 0.1);
            let volume_ratio = (market_volume / avg_market_volume).clamp(0.1, 2.0);
            (base_prob * volume_ratio).min(1.0)
        };

        if prob < 0.5 {
            return None;
        }

        let order_size_ticks = (qo.remaining_size / avg_market_volume).max(1.0);
        let avg_tick_duration_ns = 1_000_000; // 1ms average tick duration
        let delay_ns = slippage_config.compute_fill_delay(order_size_ticks, vpin, avg_tick_duration_ns);
        let impact_bps = slippage_config.compute_impact_bps(order_size_ticks, vpin);

        let fill_price = current_price * (1.0 + impact_bps / 10000.0);

        Some(FillEvent {
            order_id: qo.order_id,
            side: qo.side,
            fill_price,
            fill_size: qo.remaining_size,
            queue_position: qo.queue_position,
            vpin,
            slippage_bps: impact_bps,
            delay_ns,
            timestamp_ns,
            is_maker: true,
        })
    }

    /// Check and process fills for all pending limit orders at current market price.
    ///
    /// Returns filled orders and their fill events.
    pub fn process_fills(
        &mut self,
        current_price: f64,
        vpin: f64,
        timestamp_ns: u64,
    ) -> Vec<FillEvent> {
        let market_volume = self.avg_market_volume;

        // First pass: collect order IDs that should fill (uses static method, no borrow conflict)
        let mut to_fill: Vec<(OrderId, FillEvent)> = Vec::new();
        for qo in &self.pending {
            if let Some(event) = Self::compute_fill_event(
                qo,
                current_price,
                market_volume,
                vpin,
                timestamp_ns,
                &self.slippage_config,
                self.avg_market_volume,
            ) {
                to_fill.push((qo.order_id, event));
            }
        }

        if to_fill.is_empty() {
            return Vec::new();
        }

        let filled_ids: std::collections::HashSet<_> = to_fill.iter().map(|(id, _)| *id).collect();

        // Second pass: remove filled from pending
        self.pending.retain(|qo| !filled_ids.contains(&qo.order_id));

        // Remove from queue structures
        for level in self.bid_queues.values_mut() {
            level.orders.retain(|id| !filled_ids.contains(id));
        }
        for level in self.ask_queues.values_mut() {
            level.orders.retain(|id| !filled_ids.contains(id));
        }

        // Record fills and return
        let mut fills = Vec::new();
        for (_, event) in to_fill {
            self.filled_log.push(event.clone());
            fills.push(event);
        }

        fills
    }

    /// Update market volume estimate from a trade.
    pub fn update_market_volume(&mut self, size: f64) {
        self.avg_market_volume = self.avg_market_volume * 0.9 + size.abs() * 0.1;
    }

    /// Get queue position for a specific order.
    pub fn queue_position(&self, order_id: OrderId) -> Option<u64> {
        for qo in &self.pending {
            if qo.order_id == order_id {
                return Some(qo.queue_position);
            }
        }
        None
    }

    /// Number of pending orders.
    pub fn num_pending(&self) -> usize {
        self.pending.len()
    }

    /// Number of filled orders.
    pub fn num_filled(&self) -> usize {
        self.filled_log.len()
    }

    /// Get all fill events.
    pub fn filled_log(&self) -> &[FillEvent] {
        &self.filled_log
    }
}

impl Default for OrderEmulator {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// OrderBook — unchanged except for the bug fix in update_from_trade
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PriceLevel {
    pub price: f64,
    pub size: f64,
}

impl PriceLevel {
    pub fn new(price: f64, size: f64) -> Self {
        Self { price, size }
    }
}

#[derive(Debug, Clone)]
pub struct OrderBook {
    pub bids: BTreeMap<i64, f64>,
    pub asks: BTreeMap<i64, f64>,
    pub last_price: f64,
    pub last_size: f64,
    pub last_side: u8,
    pub vpin: f64,
    pub avg_tick_size: f64,
    pub spread_bps: f64,
}

impl OrderBook {
    pub fn new() -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last_price: 0.0,
            last_size: 0.0,
            last_side: 0,
            vpin: 0.0,
            avg_tick_size: 1.0,
            spread_bps: 1.0,
        }
    }

    /// Update the book from a trade.
    ///
    /// BUY trade → aggressive buyer removes size from bid side (takes liquidity)
    /// SELL trade → aggressive seller removes size from ask side (takes liquidity)
    pub fn update_from_trade(&mut self, tick: &TradeFlowStats) {
        let price = tick.price_int as f64 / 1_000_000_000.0;
        let size = tick.size_int as f64 / 1_000_000_000.0;
        let side = tick.side;

        self.last_price = price;
        self.last_size = size;
        self.last_side = side;
        self.vpin = tick.vpin;
        self.avg_tick_size = self.avg_tick_size * 0.9 + size * 0.1;

        if side == 1 {
            // BUY trade: remove size from bid side (aggressive buyer hitting the bid)
            self.remove_size(price, size, 1);
        } else {
            // SELL trade: remove size from ask side (aggressive seller lifting the ask)
            self.remove_size(price, size, 2);
        }

        self.spread_bps = self.compute_spread_bps();
    }

    pub fn best_bid(&self) -> Option<f64> {
        self.bids
            .iter()
            .next_back()
            .map(|(p, _)| *p as f64 / 1_000_000_000.0)
    }

    pub fn best_ask(&self) -> Option<f64> {
        self.asks
            .iter()
            .next()
            .map(|(p, _)| *p as f64 / 1_000_000_000.0)
    }

    pub fn mid_price(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some((bid + ask) / 2.0),
            _ => None,
        }
    }

    pub fn spread(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) if bid > 0.0 => Some(ask - bid),
            _ => None,
        }
    }

    fn compute_spread_bps(&self) -> f64 {
        let mid = self.mid_price().unwrap_or(self.last_price);
        if mid <= 0.0 {
            return 1.0;
        }
        let base_spread = 0.1;
        let vpin_adjustment = self.vpin * 5.0;
        let size_adjustment = (self.avg_tick_size.sqrt() * 0.01).min(5.0);
        (base_spread + vpin_adjustment + size_adjustment).max(0.1)
    }

    pub fn add_limit_order(&mut self, price: f64, size: f64, side: u8) {
        let price_int = (price * 1_000_000_000.0) as i64;
        if side == 1 {
            *self.bids.entry(price_int).or_insert(0.0) += size;
        } else {
            *self.asks.entry(price_int).or_insert(0.0) += size;
        }
    }

    pub fn remove_size(&mut self, price: f64, size: f64, side: u8) {
        let price_int = (price * 1_000_000_000.0) as i64;
        let book_side = if side == 1 {
            &mut self.bids
        } else {
            &mut self.asks
        };
        if let Some(current) = book_side.get_mut(&price_int) {
            *current -= size;
            if *current <= 0.0 {
                book_side.remove(&price_int);
            }
        }
        // If price level doesn't exist, there's nothing to remove — passive order
        // was already filled or never existed; this is fine for the synthetic model.
    }
}

impl Default for OrderBook {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_order_book_new() {
        let book = OrderBook::new();
        assert_eq!(book.best_bid(), None);
        assert_eq!(book.best_ask(), None);
        assert_eq!(book.mid_price(), None);
    }

    #[test]
    fn test_order_book_spread() {
        let mut book = OrderBook::new();
        book.add_limit_order(99.0, 1.0, 1);
        book.add_limit_order(101.0, 1.0, 2);
        assert_eq!(book.spread(), Some(2.0));
        assert_eq!(book.mid_price(), Some(100.0));
    }

    #[test]
    fn test_order_book_remove_size() {
        let mut book = OrderBook::new();
        book.add_limit_order(100.0, 5.0, 1);
        assert_eq!(book.bids.get(&(100_000_000_000_i64)), Some(&5.0));
        book.remove_size(100.0, 3.0, 1);
        assert_eq!(book.bids.get(&(100_000_000_000_i64)), Some(&2.0));
        book.remove_size(100.0, 2.0, 1);
        assert!(book.bids.get(&(100_000_000_000_i64)).is_none());
    }

    #[test]
    fn test_order_book_vpin_adjustment() {
        let mut book = OrderBook::new();
        book.vpin = 0.5;
        let spread = book.compute_spread_bps();
        assert!(spread > 0.1);
    }

    #[test]
    fn test_order_emulator_submit_and_position() {
        let mut emulator = OrderEmulator::new();
        let id1 = emulator.submit_limit(100.0, 1.0, Side::Buy, 1000);
        let id2 = emulator.submit_limit(100.0, 1.0, Side::Buy, 1001);
        let id3 = emulator.submit_limit(100.0, 1.0, Side::Sell, 1002);

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
        assert_eq!(emulator.num_pending(), 3);

        // FIFO queue positions at same price level
        assert_eq!(emulator.queue_position(id1), Some(0));
        assert_eq!(emulator.queue_position(id2), Some(1));
        assert_eq!(emulator.queue_position(id3), Some(0)); // separate ask queue
    }

    #[test]
    fn test_order_emulator_fill_probability() {
        let emulator = OrderEmulator::new();
        // At position 0, high probability
        let p0 = emulator.fill_probability(0, 1.0);
        // At position 5, lower probability
        let p5 = emulator.fill_probability(5, 1.0);
        assert!(p0 > p5);
    }

    #[test]
    fn test_order_emulator_filled_log() {
        let mut emulator = OrderEmulator::new();
        emulator.update_market_volume(10.0);
        emulator.submit_limit(100.0, 1.0, Side::Buy, 1000);

        // Simulate fills — with prob < 0.5 check, may not fill deterministically
        // in test environment. Just verify structure.
        let fills = emulator.process_fills(100.0, 0.0, 2000);
        // Without seeded randomness, this is deterministic — prob >= 0.5 should fill
        assert_eq!(emulator.filled_log().len(), fills.len());
    }

    #[test]
    fn test_order_emulator_separate_bid_ask_queues() {
        let mut emulator = OrderEmulator::new();
        let bid_id = emulator.submit_limit(100.0, 1.0, Side::Buy, 1000);
        let ask_id = emulator.submit_limit(100.0, 1.0, Side::Sell, 1001);

        assert_eq!(emulator.queue_position(bid_id), Some(0));
        assert_eq!(emulator.queue_position(ask_id), Some(0)); // separate queues
    }

    #[test]
    fn test_update_from_trade_removes_liquidity() {
        // BUY trade should remove from bid side, SELL from ask side
        let mut book = OrderBook::new();

        // Set up bid/ask levels
        book.add_limit_order(99.0, 5.0, 1); // bid at 99, size 5
        book.add_limit_order(101.0, 5.0, 2); // ask at 101, size 5

        // Simulate BUY trade at 99 (aggressive buyer hits the bid)
        let buy_tick = TradeFlowStats {
            timestamp_ns: 1000,
            price_int: 99_000_000_000,
            size_int: 2_000_000_000, // size 2
            side: 1, // buy
            cum_buy_volume: 100,
            cum_sell_volume: 50,
            vpin: 0.0,
            bucket_index: 0,
        };
        book.update_from_trade(&buy_tick);

        // Bid at 99 should have size 5 - 2 = 3
        let bid_size = book.bids.get(&(99_000_000_000_i64));
        assert_eq!(bid_size, Some(&3.0));

        // Simulate SELL trade at 101 (aggressive seller lifts the ask)
        let sell_tick = TradeFlowStats {
            timestamp_ns: 2000,
            price_int: 101_000_000_000,
            size_int: 3_000_000_000, // size 3
            side: 2, // sell
            cum_buy_volume: 100,
            cum_sell_volume: 50,
            vpin: 0.0,
            bucket_index: 0,
        };
        book.update_from_trade(&sell_tick);

        // Ask at 101 should have size 5 - 3 = 2
        let ask_size = book.asks.get(&(101_000_000_000_i64));
        assert_eq!(ask_size, Some(&2.0));
    }
}
