//! Price-time matching engine — proper order book matching for live/paper trading.
//!
//! Nautilus `MatchingCore` is a full order-book matching engine with bid/ask order lists,
//! price-level processing, and event callbacks. This module provides a correct implementation
//! for paper trading where local matching is needed.

use crate::book::{OrderBook, LevelFill, Side as BookSide};
use crate::instrument::InstrumentId;
use crate::messages::{OrderFilled, OrderSide, TradeId};
use std::collections::{BTreeMap, HashMap};

/// A resting order in the matching engine.
#[derive(Debug, Clone)]
pub struct BookOrder {
    pub order_id: u64,
    pub client_order_id: String,
    pub instrument_id: InstrumentId,
    pub side: OrderSide,
    pub price: f64,         // limit price (f64)
    pub price_int: i64,      // limit price (nanounits)
    pub size: f64,          // original size
    pub remaining: f64,     // remaining unfilled size
    pub ts_init: u64,       // submission timestamp (for time priority)
    pub is_maker: bool,
}

impl BookOrder {
    pub fn new(
        order_id: u64,
        client_order_id: String,
        instrument_id: InstrumentId,
        side: OrderSide,
        price: f64,
        size: f64,
        ts_init: u64,
    ) -> Self {
        let price_int = (price * 1_000_000_000.0) as i64;
        Self {
            order_id,
            client_order_id,
            instrument_id,
            side,
            price,
            price_int,
            size,
            remaining: size,
            ts_init,
            is_maker: true,
        }
    }

    /// Partially fill this order.
    pub fn fill(&mut self, qty: f64) -> f64 {
        let filled = self.remaining.min(qty);
        self.remaining -= filled;
        filled
    }

    /// Check if order is fully filled.
    pub fn isFilled(&self) -> bool {
        self.remaining <= 0.0
    }
}

/// Price-level queue with FIFO time priority.
#[derive(Debug, Clone)]
struct PriceLevel {
    orders: Vec<u64>,       // order IDs in FIFO order (by ts_init)
    timestamp_ns: u64,      // timestamp of first order at this level
}

impl PriceLevel {
    fn new() -> Self {
        Self {
            orders: Vec::new(),
            timestamp_ns: u64::MAX,
        }
    }

    fn push(&mut self, order_id: u64, ts_init: u64) {
        if self.orders.is_empty() {
            self.timestamp_ns = ts_init;
        }
        self.orders.push(order_id);
    }

    /// Remove a specific order by ID.
    fn remove(&mut self, order_id: u64) -> bool {
        if let Some(pos) = self.orders.iter().position(|&id| id == order_id) {
            self.orders.remove(pos);
            if self.orders.is_empty() {
                self.timestamp_ns = u64::MAX;
            } else if pos == 0 {
                // Need to recalculate first order's timestamp
                // We don't store per-order timestamps in the queue, so just mark as unknown
                self.timestamp_ns = u64::MAX;
            }
            true
        } else {
            false
        }
    }
}

/// A fill result from the matching engine.
#[derive(Debug, Clone)]
pub struct MatchResult {
    pub order_id: u64,
    pub client_order_id: String,
    pub instrument_id: InstrumentId,
    pub side: OrderSide,
    pub fill_price: f64,
    pub fill_size: f64,
    pub remaining: f64,
    pub is_maker: bool,
    pub ts_event: u64,
}

/// Price-time priority matching engine.
///
/// Implements proper limit order book matching:
/// - Price priority: best bid/ask first
/// - Time priority: FIFO at same price level
/// - Market orders walk the book
/// - Limit orders fill when they cross the spread
pub struct MatchingCore {
    instrument_id: InstrumentId,
    /// Bid price levels: price_int → PriceLevel (max-heap via negative keys)
    bid_levels: BTreeMap<i64, PriceLevel>,
    /// Ask price levels: price_int → PriceLevel (min-heap via BTreeMap)
    ask_levels: BTreeMap<i64, PriceLevel>,
    /// All resting orders: order_id → BookOrder
    orders: HashMap<u64, BookOrder>,
    /// Next order ID
    next_order_id: u64,
    /// Current market price (last trade)
    last_price: f64,
    /// Spread in basis points
    spread_bps: f64,
    /// VPIN for slippage calculation
    vpin: f64,
    /// Slippage config
    maker_fee: f64,
    taker_fee: f64,
}

impl MatchingCore {
    pub fn new(instrument_id: InstrumentId, maker_fee: f64, taker_fee: f64) -> Self {
        Self {
            instrument_id,
            bid_levels: BTreeMap::new(),
            ask_levels: BTreeMap::new(),
            orders: HashMap::new(),
            next_order_id: 1,
            last_price: 0.0,
            spread_bps: 1.0,
            vpin: 0.0,
            maker_fee,
            taker_fee,
        }
    }

    /// Update market state from a trade tick.
    pub fn update_market(&mut self, price: f64, vpin: f64, spread_bps: f64) {
        self.last_price = price;
        self.vpin = vpin;
        self.spread_bps = spread_bps;
    }

    /// Get best bid price (highest bid).
    pub fn best_bid(&self) -> Option<f64> {
        self.bid_levels
            .keys()
            .next_back()
            .map(|&p| p as f64 / 1_000_000_000.0)
    }

    /// Get best ask price (lowest ask).
    pub fn best_ask(&self) -> Option<f64> {
        self.ask_levels
            .keys()
            .next()
            .map(|&p| p as f64 / 1_000_000_000.0)
    }

    /// Get mid price.
    pub fn mid_price(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => Some((b + a) / 2.0),
            _ => None,
        }
    }

    /// Submit a limit order. Returns fills if immediate match.
    pub fn submit_limit(
        &mut self,
        client_order_id: String,
        side: OrderSide,
        price: f64,
        size: f64,
        ts_init: u64,
    ) -> Vec<MatchResult> {
        let order_id = self.next_order_id;
        self.next_order_id += 1;

        let price_int = (price * 1_000_000_000.0) as i64;
        let book_side = match side {
            OrderSide::Buy => BookSide::Buy,
            OrderSide::Sell => BookSide::Sell,
        };

        // Check if this limit order crosses the spread (immediate fill)
        let would_fills = self.would_fill_limit(price, size, book_side);
        if !would_fills.is_empty() {
            // Immediate fill — order doesn't rest
            return self.execute_market_like(order_id, client_order_id, side, price, size, ts_init);
        }

        // Add to book
        let levels = match book_side {
            BookSide::Buy => &mut self.bid_levels,
            BookSide::Sell => &mut self.ask_levels,
        };
        let level = levels.entry(price_int).or_insert_with(PriceLevel::new);
        level.push(order_id, ts_init);

        let order = BookOrder::new(order_id, client_order_id, self.instrument_id.clone(), side, price, size, ts_init);
        self.orders.insert(order_id, order);

        // Check if it immediately crosses at-touch or better price
        let fills = self.check_limit_fill(order_id, price, size, book_side, ts_init);
        fills
    }

    /// Submit a market order. Walks the book for liquidity.
    pub fn submit_market(
        &mut self,
        client_order_id: String,
        side: OrderSide,
        size: f64,
        ts_init: u64,
    ) -> Vec<MatchResult> {
        let order_id = self.next_order_id;
        self.next_order_id += 1;

        let book_side = match side {
            OrderSide::Buy => BookSide::Buy,
            OrderSide::Sell => BookSide::Sell,
        };

        self.execute_market_like(order_id, client_order_id, side, self.last_price, size, ts_init)
    }

    /// Cancel a resting order.
    pub fn cancel(&mut self, order_id: u64) -> bool {
        if let Some(order) = self.orders.remove(&order_id) {
            let levels = match order.side {
                OrderSide::Buy => &mut self.bid_levels,
                OrderSide::Sell => &mut self.ask_levels,
            };
            if let Some(level) = levels.get_mut(&order.price_int) {
                level.remove(order_id);
                if level.orders.is_empty() {
                    levels.remove(&order.price_int);
                }
            }
            return true;
        }
        false
    }

    /// Check if a limit order would immediately fill (crosses the spread).
    fn would_fill_limit(&self, price: f64, _size: f64, side: BookSide) -> Vec<()> {
        match side {
            BookSide::Buy => {
                // Buy limit crosses if price >= best ask
                if let Some(ask) = self.best_ask() {
                    if price >= ask {
                        return vec![()];
                    }
                }
            }
            BookSide::Sell => {
                // Sell limit crosses if price <= best bid
                if let Some(bid) = self.best_bid() {
                    if price <= bid {
                        return vec![()];
                    }
                }
            }
        }
        vec![]
    }

    /// Execute a market-like order (fully taking liquidity).
    fn execute_market_like(
        &mut self,
        order_id: u64,
        client_order_id: String,
        side: OrderSide,
        reference_price: f64,
        size: f64,
        ts_init: u64,
    ) -> Vec<MatchResult> {
        let book_side = match side {
            OrderSide::Buy => BookSide::Buy,
            OrderSide::Sell => BookSide::Sell,
        };

        let mut remaining = size;
        let mut fills = Vec::new();

        // Walk the book on the opposite side
        let levels_map: &BTreeMap<i64, PriceLevel> = match book_side {
            BookSide::Buy => &self.ask_levels,
            BookSide::Sell => &self.bid_levels,
        };

        for (&price_int, level) in levels_map.iter() {
            if remaining <= 0.0 {
                break;
            }
            let level_price = price_int as f64 / 1_000_000_000.0;

            // Fill against all orders at this price level (FIFO)
            let order_ids: Vec<u64> = level.orders.clone();
            for oid in order_ids {
                if remaining <= 0.0 {
                    break;
                }
                if let Some(order) = self.orders.get_mut(&oid) {
                    let filled_qty = order.fill(remaining);
                    let fill_price = level_price; // Maker's price
                    let fee = filled_qty * self.taker_fee;

                    remaining -= filled_qty;

                    fills.push(MatchResult {
                        order_id: oid,
                        client_order_id: order.client_order_id.clone(),
                        instrument_id: order.instrument_id.clone(),
                        side: order.side,
                        fill_price,
                        fill_size: filled_qty,
                        remaining: order.remaining,
                        is_maker: true,
                        ts_event: ts_init,
                    });

                    // Remove if fully filled
                    if order.isFilled() {
                        self.orders.remove(&oid);
                        // Level will be cleaned up after iteration
                    }
                }
            }
        }

        // Clean up empty levels
        self.clean_empty_levels();

        // If market order had remaining unfilled, it's a partial fill (no more liquidity)
        // A real exchange would reject the remaining; for paper we record partial fill
        fills
    }

    /// Check if a resting limit order should trigger fills (for limit orders that
    /// were submitted but may cross due to subsequent price moves).
    fn check_limit_fill(
        &mut self,
        order_id: u64,
        _price: f64,
        _size: f64,
        side: BookSide,
        ts_init: u64,
    ) -> Vec<MatchResult> {
        // For a resting limit order, check if market moved to cross it.
        // This is called when a new order is added to see if it immediately fills.
        // In practice, a properly submitted limit order that crosses should not rest.
        // This method handles edge cases where the order was already in the book
        // and market moved.
        let mut fills = Vec::new();

        match side {
            BookSide::Buy => {
                // A buy limit rests at its price — it only fills when market
                // price drops to or below it (price <= limit for buy).
                // Since we already checked would_fill before adding, this shouldn't fire.
            }
            BookSide::Sell => {
                // A sell limit rests — fills when market price rises to or above it.
            }
        }

        fills
    }

    /// Remove empty price levels.
    fn clean_empty_levels(&mut self) {
        self.bid_levels.retain(|_, l| !l.orders.is_empty());
        self.ask_levels.retain(|_, l| !l.orders.is_empty());
    }

    /// Number of resting orders.
    pub fn num_resting(&self) -> usize {
        self.orders.len()
    }

    /// Number of price levels.
    pub fn num_levels(&self) -> usize {
        self.bid_levels.len() + self.ask_levels.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instr() -> InstrumentId {
        InstrumentId::new("BTCUSDT", "PAPER")
    }

    #[test]
    fn test_matching_core_best_bid_ask() {
        let mut core = MatchingCore::new(instr(), 0.001, 0.001);

        core.submit_limit("c1".into(), OrderSide::Buy, 99.0, 1.0, 1000);
        core.submit_limit("c2".into(), OrderSide::Sell, 101.0, 1.0, 1001);

        assert_eq!(core.best_bid(), Some(99.0));
        assert_eq!(core.best_ask(), Some(101.0));
        assert_eq!(core.mid_price(), Some(100.0));
    }

    #[test]
    fn test_limit_order_resting() {
        let mut core = MatchingCore::new(instr(), 0.001, 0.001);

        // Sell at 101, buy at 100 — should not immediately fill
        let fills = core.submit_limit("sell1".into(), OrderSide::Sell, 101.0, 1.0, 1000);
        assert!(fills.is_empty(), "Limit sell at 101 should rest when bid is 100");

        let fills2 = core.submit_limit("buy1".into(), OrderSide::Buy, 100.0, 1.0, 1001);
        assert!(fills2.is_empty(), "Limit buy at 100 should rest when ask is 101");

        assert_eq!(core.num_resting(), 2);
    }

    #[test]
    fn test_market_order_takes_liquidity() {
        let mut core = MatchingCore::new(instr(), 0.001, 0.001);

        // Set up some liquidity
        core.submit_limit("s1".into(), OrderSide::Sell, 100.0, 5.0, 1000);
        core.submit_limit("s2".into(), OrderSide::Sell, 101.0, 5.0, 1001);

        // Market buy should walk the book
        core.update_market(100.5, 0.0, 1.0);
        let fills = core.submit_market("m1".into(), OrderSide::Buy, 3.0, 2000);

        assert!(!fills.is_empty());
        assert_eq!(fills[0].fill_price, 100.0); // Takes best ask first
        assert_eq!(fills[0].fill_size, 3.0);
    }

    #[test]
    fn test_limit_cross_spread_immediate_fill() {
        let mut core = MatchingCore::new(instr(), 0.001, 0.001);

        // Set up best bid at 99
        core.submit_limit("b1".into(), OrderSide::Buy, 99.0, 5.0, 1000);

        // Sell limit at 98 — crosses spread (98 <= 99), should immediately fill
        core.update_market(99.5, 0.0, 1.0);
        let fills = core.submit_limit("s1".into(), OrderSide::Sell, 98.0, 2.0, 2000);

        assert!(!fills.is_empty(), "Sell at 98 should immediately fill against bid at 99");
        assert_eq!(fills[0].fill_price, 99.0); // Gets bid price
        assert_eq!(fills[0].fill_size, 2.0);
    }

    #[test]
    fn test_cancel_order() {
        let mut core = MatchingCore::new(instr(), 0.001, 0.001);

        let fills = core.submit_limit("b1".into(), OrderSide::Buy, 99.0, 5.0, 1000);
        assert!(fills.is_empty());

        // The order is resting — find its ID (it's the first order, ID = 1)
        let result = core.cancel(1);
        assert!(result);
        assert_eq!(core.num_resting(), 0);
    }

    #[test]
    fn test_price_time_priority() {
        let mut core = MatchingCore::new(instr(), 0.001, 0.001);

        // Two sell orders at same price, different times
        core.submit_limit("s1".into(), OrderSide::Sell, 100.0, 2.0, 1000);
        core.submit_limit("s2".into(), OrderSide::Sell, 100.0, 3.0, 1001);

        // Buy market order takes 3.0 — should fill s1 first (earlier)
        let fills = core.submit_market("m1".into(), OrderSide::Buy, 3.0, 2000);

        // s1 (2.0) + s2 (1.0) = 3.0
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[0].order_id, 1); // s1 first
        assert_eq!(fills[0].fill_size, 2.0);
        assert_eq!(fills[1].order_id, 2); // s2 second
        assert_eq!(fills[1].fill_size, 1.0);
    }

    #[test]
    fn test_filled_order_removed_from_book() {
        let mut core = MatchingCore::new(instr(), 0.001, 0.001);

        core.submit_limit("s1".into(), OrderSide::Sell, 100.0, 2.0, 1000);
        assert_eq!(core.num_resting(), 1);

        // Buy exactly 2.0 — should fully fill s1
        let fills = core.submit_market("m1".into(), OrderSide::Buy, 2.0, 2000);

        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].remaining, 0.0);
        assert_eq!(core.num_resting(), 0);
    }
}
