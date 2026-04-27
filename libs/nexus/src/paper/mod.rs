//! Paper trading module.

pub mod broker;
pub use broker::{PaperBroker, PaperTrade};

use crate::messages::{CancelOrder, OrderFilled, SubmitOrder};

/// Shared trait for simulated (paper) order execution.
/// Both paper trading and backtest use this to submit/cancel orders and receive fills.
pub trait PaperExecution: Send + Sync {
    /// Submit an order and get immediate simulated fills (sync).
    fn submit_order(&mut self, submit: &SubmitOrder) -> Vec<OrderFilled>;

    /// Cancel an order.
    fn cancel_order(&mut self, cancel: &CancelOrder) -> bool;

    /// Apply a trade tick to update internal state (order book, VPIN, etc).
    fn apply_trade(&mut self, trade: &crate::buffer::TradeFlowStats);

    /// Seed the internal order book with real L2 data for paper trading.
    /// When seeded, paper fills use real book depth instead of synthetic VPIN model.
    fn seed_order_book(&mut self, bids: &[(f64, f64)], asks: &[(f64, f64)])
    where
        f64: Copy;

    /// Get current simulated latency in nanoseconds.
    fn latency_ns(&self) -> u64;

    /// Set simulated execution latency (adds delay to fill simulation).
    fn set_latency_ns(&mut self, latency_ns: u64);
}