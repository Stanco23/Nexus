//! L2 Order Book — synthetic limit order book reconstruction from tick stream.
//!
//! # Overview
//! When L2 market data is unavailable (e.g., Binance SPOT), we reconstruct
//! a synthetic order book from the tick stream. Each trade updates the book.
//!
//! # Book Update Logic
//! - Buy trade + size → remove size from bids (aggressive buyer takes liquidity)
//! - Sell trade + size → remove size from asks (aggressive seller takes liquidity)
//! - New orders are assumed to arrive at the mid-price ± spread/2
//!
//! # Spread Model
//! spread = f(vpin, avg_tick_size, volatility)
//! Higher VPIN → wider spread (informed trading risk)

use crate::buffer::tick_buffer::TradeFlowStats;
use std::collections::BTreeMap;

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

    pub fn update_from_trade(&mut self, tick: &TradeFlowStats) {
        let price = tick.price_int as f64 / 1_000_000_000.0;
        let size = tick.size_int as f64 / 1_000_000_000.0;
        let side = tick.side;

        self.last_price = price;
        self.last_size = size;
        self.last_side = side;
        self.vpin = tick.vpin;
        self.avg_tick_size = self.avg_tick_size * 0.9 + size * 0.1;

        let price_int = tick.price_int;
        if side == 1 {
            self.bids.insert(price_int, size);
        } else {
            self.asks.insert(price_int, size);
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
    }
}

impl Default for OrderBook {
    fn default() -> Self {
        Self::new()
    }
}

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
}
