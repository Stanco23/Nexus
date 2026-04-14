//! Portfolio — multi-instrument backtest orchestration.
//!
//! # Architecture
//! - `Portfolio`: manages `HashMap<InstrumentId, InstrumentState>`
//! - `InstrumentState`: position, pending_orders, equity, unrealized_pnl
//! - `BacktestEngine::run_portfolio()`: time-ordered tick delivery across instruments
//! - Strategy: `on_trade(instrument_id, tick, ctx)` called per instrument tick
//!
//! # Portfolio Equity
//! Portfolio equity = sum of all instrument equities. Per-instrument equity tracked separately.

use crate::engine::Signal;
use crate::instrument::InstrumentId;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct InstrumentState {
    pub position: f64,
    pub entry_price: f64,
    pub equity: f64,
    pub unrealized_pnl: f64,
    pub num_trades: usize,
}

impl InstrumentState {
    pub fn new(initial_equity: f64) -> Self {
        Self {
            position: 0.0,
            entry_price: 0.0,
            equity: initial_equity,
            unrealized_pnl: 0.0,
            num_trades: 0,
        }
    }

    pub fn update_unrealized_pnl(&mut self, current_price: f64) {
        if self.position == 0.0 || self.entry_price == 0.0 {
            self.unrealized_pnl = 0.0;
            return;
        }
        if self.position > 0.0 {
            self.unrealized_pnl = (current_price - self.entry_price) * self.position.abs();
        } else {
            self.unrealized_pnl = (self.entry_price - current_price) * self.position.abs();
        }
    }

    pub fn total_equity(&self) -> f64 {
        self.equity + self.unrealized_pnl
    }
}

#[derive(Debug)]
pub struct Portfolio {
    initial_equity_per_instrument: f64,
    states: HashMap<InstrumentId, InstrumentState>,
}

impl Portfolio {
    pub fn new(initial_equity_per_instrument: f64) -> Self {
        Self {
            initial_equity_per_instrument,
            states: HashMap::new(),
        }
    }

    pub fn register_instrument(&mut self, instrument_id: InstrumentId) {
        self.states
            .entry(instrument_id)
            .or_insert_with(|| InstrumentState::new(self.initial_equity_per_instrument));
    }

    pub fn register_instruments(&mut self, instrument_ids: &[InstrumentId]) {
        for id in instrument_ids {
            self.register_instrument(*id);
        }
    }

    pub fn state(&self, instrument_id: InstrumentId) -> Option<&InstrumentState> {
        self.states.get(&instrument_id)
    }

    pub fn state_mut(&mut self, instrument_id: InstrumentId) -> Option<&mut InstrumentState> {
        self.states.get_mut(&instrument_id)
    }

    pub fn portfolio_equity(&self) -> f64 {
        self.states.values().map(|s| s.total_equity()).sum()
    }

    pub fn total_unrealized_pnl(&self) -> f64 {
        self.states.values().map(|s| s.unrealized_pnl).sum()
    }

    pub fn num_instruments(&self) -> usize {
        self.states.len()
    }

    pub fn open_positions(&self) -> Vec<(InstrumentId, f64, f64)> {
        self.states
            .iter()
            .filter(|(_, s)| s.position != 0.0)
            .map(|(id, s)| (*id, s.position, s.entry_price))
            .collect()
    }
}

pub trait PortfolioStrategy {
    fn on_trade(
        &mut self,
        instrument_id: InstrumentId,
        timestamp_ns: u64,
        price: f64,
        size: f64,
        portfolio: &mut Portfolio,
    ) -> Signal;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_portfolio_new() {
        let portfolio = Portfolio::new(10000.0);
        assert_eq!(portfolio.num_instruments(), 0);
        assert_eq!(portfolio.portfolio_equity(), 0.0);
    }

    #[test]
    fn test_portfolio_register_instrument() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        portfolio.register_instrument(btc_id);
        assert_eq!(portfolio.num_instruments(), 1);
        let state = portfolio.state(btc_id).unwrap();
        assert_eq!(state.equity, 10000.0);
        assert_eq!(state.position, 0.0);
    }

    #[test]
    fn test_portfolio_multiple_instruments() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let eth_id = InstrumentId::new("ETHUSDT", "BINANCE");
        portfolio.register_instruments(&[btc_id, eth_id]);
        assert_eq!(portfolio.num_instruments(), 2);
        assert_eq!(portfolio.portfolio_equity(), 20000.0);
    }

    #[test]
    fn test_instrument_state_unrealized_pnl_long() {
        let mut state = InstrumentState::new(10000.0);
        state.position = 1.0;
        state.entry_price = 100.0;
        state.update_unrealized_pnl(110.0);
        assert!((state.unrealized_pnl - 10.0).abs() < 0.001);
        assert!((state.total_equity() - 10010.0).abs() < 0.001);
    }

    #[test]
    fn test_instrument_state_unrealized_pnl_short() {
        let mut state = InstrumentState::new(10000.0);
        state.position = -1.0;
        state.entry_price = 100.0;
        state.update_unrealized_pnl(90.0);
        assert!((state.unrealized_pnl - 10.0).abs() < 0.001);
        assert!((state.total_equity() - 10010.0).abs() < 0.001);
    }

    #[test]
    fn test_portfolio_total_equity() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let eth_id = InstrumentId::new("ETHUSDT", "BINANCE");
        portfolio.register_instruments(&[btc_id, eth_id]);

        {
            let btc = portfolio.state_mut(btc_id).unwrap();
            btc.position = 1.0;
            btc.entry_price = 50000.0;
            btc.update_unrealized_pnl(51000.0);
        }

        {
            let eth = portfolio.state_mut(eth_id).unwrap();
            eth.position = -1.0;
            eth.entry_price = 3000.0;
            eth.update_unrealized_pnl(2900.0);
        }

        let btc_total = 10000.0 + 1000.0;
        let eth_total = 10000.0 + 100.0;
        assert!((portfolio.portfolio_equity() - (btc_total + eth_total)).abs() < 0.001);
    }
}
