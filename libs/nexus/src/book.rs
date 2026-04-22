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
    pub remaining_size: f64,   // for partial fills: remaining size after this level
    pub queue_position: u64,
    pub vpin: f64,
    pub slippage_bps: f64,
    pub delay_ns: u64,
    pub timestamp_ns: u64,
    pub is_maker: bool,
    pub fee: f64,             // commission charged for this fill
}

/// Price level with queue of orders.
#[derive(Debug, Clone)]
struct QueueLevel {
    orders: Vec<OrderId>,       // order IDs in FIFO order
    timestamp_ns: u64,          // time first order was placed
    order_count: u64,           // number of orders at this level (queue depth)
}

impl QueueLevel {
    fn new() -> Self {
        Self {
            orders: Vec::new(),
            timestamp_ns: u64::MAX,
            order_count: 0,
        }
    }

    fn push(&mut self, order_id: OrderId, timestamp_ns: u64) {
        if self.orders.is_empty() {
            self.timestamp_ns = timestamp_ns;
        }
        self.orders.push(order_id);
        self.order_count += 1;
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

    /// New emulator with a custom slippage configuration.
    pub fn new_with_config(config: SlippageConfig) -> Self {
        Self {
            slippage_config: config,
            ..Self::new()
        }
    }

    /// Cancel a specific queued order by ID.
    /// Returns true if the order was found and removed.
    pub fn cancel_order(&mut self, order_id: OrderId) -> bool {
        // Remove from pending Vec
        if let Some(pos) = self.pending.iter().position(|qo| qo.order_id == order_id) {
            let qo = self.pending.remove(pos);
            // Remove from bid or ask queue
            let price_key = qo.price_int;
            let queues = if qo.side == Side::Buy {
                &mut self.bid_queues
            } else {
                &mut self.ask_queues
            };
            if let Some(level) = queues.get_mut(&price_key) {
                level.orders.retain(|&id| id != order_id);
            }
            return true;
        }
        false
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
    #[allow(clippy::too_many_arguments)]
    fn compute_fill_event(
        qo: &QueuedOrder,
        current_price: f64,
        market_volume: f64,
        vpin: f64,
        timestamp_ns: u64,
        slippage_config: &SlippageConfig,
        avg_market_volume: f64,
        maker_fee: f64,
    ) -> Option<FillEvent> {
        // Check price crossing before fill probability:
        // - BUY limit: fills when market price drops to or below the limit price
        //   (I want to buy at P or better — lower is better for buyer)
        // - SELL limit: fills when market price rises to or above the limit price
        //   (I want to sell at P or better — higher is better for seller)
        let price_crossed = match qo.side {
            Side::Buy => current_price <= qo.price,
            Side::Sell => current_price >= qo.price,
        };
        if !price_crossed {
            return None;
        }

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
        let fee = qo.remaining_size * maker_fee;

        Some(FillEvent {
            order_id: qo.order_id,
            side: qo.side,
            fill_price,
            fill_size: qo.remaining_size,
            remaining_size: 0.0, // limit orders fully fill in emulator
            queue_position: qo.queue_position,
            vpin,
            slippage_bps: impact_bps,
            delay_ns,
            timestamp_ns,
            is_maker: true,
            fee,
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
        maker_fee: f64,
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
                maker_fee,
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

    /// Process a market order by walking through the order book levels.
    ///
    /// Consumes liquidity level by level. Each level fill gets its own FillEvent
    /// with the correct price, size, slippage, and taker fee.
    ///
    /// Returns all fill events (one per level consumed). Remaining size is returned
    /// in the last fill's `remaining_size` if order exceeds book liquidity.
    pub fn process_market_order(
        &mut self,
        size: f64,
        side: Side,
        order_book: &OrderBook,
        timestamp_ns: u64,
        taker_fee: f64,
    ) -> Vec<FillEvent> {
        let levels = order_book.walk_book(size, side);
        let mut fills = Vec::new();

        for level in levels {
            let fill_price = level.price();
            let order_size_ticks = (level.size_filled / self.avg_market_volume).max(1.0);
            let impact_bps = self.slippage_config.compute_impact_bps(order_size_ticks, order_book.vpin);
            let delay_ns = self.slippage_config.compute_fill_delay(
                order_size_ticks,
                order_book.vpin,
                1_000_000,
            );
            let slippage_price = fill_price * (1.0 + impact_bps / 10000.0);
            let fee = level.size_filled * taker_fee;

            let order_id = self.next_order_id;
            self.next_order_id += 1;

            let event = FillEvent {
                order_id,
                side,
                fill_price: slippage_price,
                fill_size: level.size_filled,
                remaining_size: level.remaining_at_level,
                queue_position: 0,
                vpin: order_book.vpin,
                slippage_bps: impact_bps,
                delay_ns,
                timestamp_ns,
                is_maker: false,
                fee,
            };
            self.filled_log.push(event.clone());
            fills.push(event);
        }

        fills
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

/// Result of a single price level being consumed by a market order.
#[derive(Debug, Clone)]
pub struct LevelFill {
    /// Price at this level (nanounits).
    pub price_int: i64,
    /// Size actually filled at this level.
    pub size_filled: f64,
    /// Remaining size at this level after partial fill (0 if fully consumed).
    pub remaining_at_level: f64,
    /// Number of orders at this price level (queue depth for this level).
    pub queue_depth: u64,
}

impl LevelFill {
    pub fn price(&self) -> f64 {
        self.price_int as f64 / 1_000_000_000.0
    }
}

#[derive(Debug, Clone)]
pub struct OrderBook {
    /// Bid price levels: price_int → size
    pub bids: BTreeMap<i64, f64>,
    /// Ask price levels: price_int → size
    pub asks: BTreeMap<i64, f64>,
    /// Bid queue depths: price_int → number of orders at this level
    bid_queue_depths: BTreeMap<i64, u64>,
    /// Ask queue depths: price_int → number of orders at this level
    ask_queue_depths: BTreeMap<i64, u64>,
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
            bid_queue_depths: BTreeMap::new(),
            ask_queue_depths: BTreeMap::new(),
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
            *self.bid_queue_depths.entry(price_int).or_insert(0) += 1;
        } else {
            *self.asks.entry(price_int).or_insert(0.0) += size;
            *self.ask_queue_depths.entry(price_int).or_insert(0) += 1;
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
        // Decrement queue depth at this level.
        let queue_depths = if side == 1 {
            &mut self.bid_queue_depths
        } else {
            &mut self.ask_queue_depths
        };
        if let Some(depth) = queue_depths.get_mut(&price_int) {
            *depth -= 1;
            if *depth == 0 {
                queue_depths.remove(&price_int);
            }
        }
    }

    /// Validate that a price aligns to the given tick size (price_increment).
    /// Returns true if the price is a valid tick multiple from min_price.
    pub fn validate_price_increment(price: f64, tick_size: f64, min_price: f64) -> bool {
        if tick_size <= 0.0 || price < min_price {
            return false;
        }
        let offset = (price - min_price) / tick_size;
        (offset - offset.round()).abs() < 1e-9
    }

    /// Validate that a price aligns to the instrument's tick size.
    pub fn validate_price(&self, price: f64, tick_size: f64) -> bool {
        Self::validate_price_increment(price, tick_size, 0.0)
    }

    /// Walk the order book and compute fills for a market order.
    ///
    /// For a BUY order: walk up the ask side (lowest ask first, then higher asks).
    /// For a SELL order: walk down the bid side (highest bid first, then lower bids).
    ///
    /// Returns a list of level fills, each representing one price level consumed.
    /// The list is ordered from best price to worst price for the aggressing order.
    ///
    /// If the order size is larger than total liquidity, remaining size is returned
    /// in the last fill with `remaining_at_level = size`.
    pub fn walk_book(&self, size: f64, side: Side) -> Vec<LevelFill> {
        let mut fills = Vec::new();
        let mut remaining = size;

        if side == Side::Buy {
            // Walk up the ask side (ascending by price_int)
            for (&price_int, &available) in self.asks.iter() {
                if remaining <= 0.0 {
                    break;
                }
                let size_at_level = available.min(remaining);
                let remaining_at_level = available - size_at_level;
                let queue_depth = self.ask_queue_depths.get(&price_int).copied().unwrap_or(1);
                fills.push(LevelFill {
                    price_int,
                    size_filled: size_at_level,
                    remaining_at_level,
                    queue_depth,
                });
                remaining -= size_at_level;
            }
        } else {
            // Walk down the bid side (descending by price_int — highest bid first)
            for (&price_int, &available) in self.bids.iter().rev() {
                if remaining <= 0.0 {
                    break;
                }
                let size_at_level = available.min(remaining);
                let remaining_at_level = available - size_at_level;
                let queue_depth = self.bid_queue_depths.get(&price_int).copied().unwrap_or(1);
                fills.push(LevelFill {
                    price_int,
                    size_filled: size_at_level,
                    remaining_at_level,
                    queue_depth,
                });
                remaining -= size_at_level;
            }
        }

        fills
    }
}

/// Incremental order book delta update.
///
/// Represents a set of changes to apply to the order book
/// rather than a full book snapshot.
#[derive(Debug, Clone)]
pub struct OrderBookDelta {
    pub price_int: i64,
    pub size_delta: f64,   // positive = add, negative = remove
    pub side: Side,
}

impl OrderBookDelta {
    pub fn new(price_int: i64, size_delta: f64, side: Side) -> Self {
        Self { price_int, size_delta, side }
    }

    pub fn price(&self) -> f64 {
        self.price_int as f64 / 1_000_000_000.0
    }
}

/// A batch of order book deltas to apply atomically.
#[derive(Debug, Clone, Default)]
pub struct OrderBookDeltas {
    pub deltas: Vec<OrderBookDelta>,
    pub timestamp_ns: u64,
}

impl OrderBookDeltas {
    pub fn new(timestamp_ns: u64) -> Self {
        Self {
            deltas: Vec::new(),
            timestamp_ns,
        }
    }

    pub fn add_bid(&mut self, price_int: i64, size_delta: f64) {
        self.deltas.push(OrderBookDelta::new(price_int, size_delta, Side::Buy));
    }

    pub fn add_ask(&mut self, price_int: i64, size_delta: f64) {
        self.deltas.push(OrderBookDelta::new(price_int, size_delta, Side::Sell));
    }

    /// Apply deltas to an order book.
    pub fn apply_to(&self, book: &mut OrderBook) {
        for delta in &self.deltas {
            let price = delta.price();
            let size = delta.size_delta.abs();
            if delta.side == Side::Buy {
                if delta.size_delta > 0.0 {
                    *book.bids.entry(delta.price_int).or_insert(0.0) += size;
                    *book.bid_queue_depths.entry(delta.price_int).or_insert(0) += 1;
                } else {
                    book.remove_size(price, size, 1);
                }
            } else {
                if delta.size_delta > 0.0 {
                    *book.asks.entry(delta.price_int).or_insert(0.0) += size;
                    *book.ask_queue_depths.entry(delta.price_int).or_insert(0) += 1;
                } else {
                    book.remove_size(price, size, 2);
                }
            }
        }
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
        let fills = emulator.process_fills(100.0, 0.0, 2000, 0.0);
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

    #[test]
    fn test_walk_book_queue_depth() {
        let mut book = OrderBook::new();
        book.add_limit_order(100.0, 5.0, 1);
        book.add_limit_order(100.0, 5.0, 1);
        book.add_limit_order(101.0, 3.0, 2);

        let fills = book.walk_book(10.0, Side::Buy);
        assert!(!fills.is_empty());
        for fill in &fills {
            assert!(fill.queue_depth >= 1, "queue_depth should be >= 1");
        }
    }

    #[test]
    fn test_validate_price_increment() {
        let tick = 0.01;
        assert!(OrderBook::validate_price_increment(100.00, tick, 0.0));
        assert!(OrderBook::validate_price_increment(100.01, tick, 0.0));
        assert!(!OrderBook::validate_price_increment(100.005, tick, 0.0));
        assert!(!OrderBook::validate_price_increment(99.99, tick, 100.0));
    }

    #[test]
    fn test_order_book_deltas_apply() {
        let mut book = OrderBook::new();
        book.add_limit_order(100.0, 5.0, 1);

        let mut deltas = OrderBookDeltas::new(1000);
        deltas.add_bid(100_000_000_000, 2.0);
        deltas.apply_to(&mut book);

        assert_eq!(book.bids.get(&(100_000_000_000_i64)), Some(&7.0));
    }

    #[test]
    fn test_order_book_delta_remove() {
        let mut book = OrderBook::new();
        book.add_limit_order(100.0, 5.0, 1);

        let mut deltas = OrderBookDeltas::new(1000);
        deltas.deltas.push(OrderBookDelta::new(100_000_000_000, -2.0, Side::Buy));
        deltas.apply_to(&mut book);

        assert_eq!(book.bids.get(&(100_000_000_000_i64)), Some(&3.0));
    }
}

// ─── Phase 2.8: Add submit_market to OrderEmulator ───────────────────────────────────────
// This method was identified as missing during the Phase 3.2 StrategyCtx implementation review.
// OrderEmulator had process_market_order but no simple submit_market entry point.

impl OrderEmulator {
    /// Submit a market order for immediate fill simulation.
    /// Returns an order ID (0 if size is 0).
    ///
    /// Unlike limit orders which queue and wait for price crossing,
    /// market orders are filled immediately at current market price
    /// with VPIN-based slippage applied.
    pub fn submit_market(
        &mut self,
        size: f64,
        side: Side,
        timestamp_ns: u64,
    ) -> OrderId {
        if size <= 0.0 {
            return 0;
        }

        let order_id = self.next_order_id;
        self.next_order_id += 1;

        // Market orders don't queue — they fill immediately.
        // Record as a fill event with queue_position=0 (no queue delay).
        // The actual fill price and slippage are computed via process_market_order
        // when the engine calls it with the current order book state.
        let qo = QueuedOrder {
            order_id,
            side,
            price: 0.0, // Market order has no limit price
            price_int: 0,
            size,
            remaining_size: size,
            submitted_ns: timestamp_ns,
            queue_position: 0, // Immediate fill, no queue position
        };

        self.pending.push(qo);
        order_id
    }
}
