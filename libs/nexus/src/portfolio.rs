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

use crate::buffer::buffer_set::MergeCursor;
use crate::engine::{CommissionConfig, Signal};
use crate::instrument::InstrumentId;
use std::collections::HashMap;

/// Portfolio-level configuration.
#[derive(Debug, Clone)]
pub struct PortfolioConfig {
    pub initial_equity_per_instrument: f64,
    pub commission: CommissionConfig,
    /// SL threshold in percent (e.g. 2.0 = 2%)
    pub stop_loss_pct: f64,
    /// TP threshold in percent (e.g. 5.0 = 5%)
    pub take_profit_pct: f64,
    /// Trading days per year for Sharpe annualization (252 = standard, 365 = calendar)
    pub trading_days_per_year: f64,
}

impl PortfolioConfig {
    pub fn new(initial_equity_per_instrument: f64, commission: CommissionConfig) -> Self {
        Self {
            initial_equity_per_instrument,
            commission,
            stop_loss_pct: 2.0,
            take_profit_pct: 5.0,
            trading_days_per_year: 252.0,
        }
    }

    pub fn with_stop_loss(mut self, pct: f64) -> Self {
        self.stop_loss_pct = pct;
        self
    }

    pub fn with_take_profit(mut self, pct: f64) -> Self {
        self.take_profit_pct = pct;
        self
    }

    pub fn with_trading_days(mut self, days: f64) -> Self {
        self.trading_days_per_year = days;
        self
    }
}

impl Default for PortfolioConfig {
    fn default() -> Self {
        Self::new(10_000.0, CommissionConfig::new(0.001))
    }
}

#[derive(Debug, Clone)]
pub struct InstrumentState {
    pub position: f64,
    pub entry_price: f64,
    pub equity: f64,
    pub unrealized_pnl: f64,
    pub realized_pnl: f64,
    pub commissions: f64,
    pub num_trades: usize,
    pub peak_equity: f64,
    pub max_drawdown: f64,
}

impl InstrumentState {
    pub fn new(initial_equity: f64) -> Self {
        Self {
            position: 0.0,
            entry_price: 0.0,
            equity: initial_equity,
            unrealized_pnl: 0.0,
            realized_pnl: 0.0,
            commissions: 0.0,
            num_trades: 0,
            peak_equity: initial_equity,
            max_drawdown: 0.0,
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

    /// Update peak equity and max drawdown after a price tick.
    pub fn update_peak(&mut self, current_price: f64) {
        self.update_unrealized_pnl(current_price);
        let total = self.total_equity();
        if total > self.peak_equity {
            self.peak_equity = total;
        }
        if self.peak_equity > 0.0 {
            let dd = (self.peak_equity - total) / self.peak_equity * 100.0;
            if dd > self.max_drawdown {
                self.max_drawdown = dd;
            }
        }
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
            self.register_instrument(id.clone());
        }
    }

    pub fn state(&self, instrument_id: &InstrumentId) -> Option<&InstrumentState> {
        self.states.get(instrument_id)
    }

    pub fn state_mut(&mut self, instrument_id: &InstrumentId) -> Option<&mut InstrumentState> {
        self.states.get_mut(instrument_id)
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
            .map(|(id, s)| (id.clone(), s.position, s.entry_price))
            .collect()
    }

    /// Open or add to a position for the given instrument.
    pub fn open_position(
        &mut self,
        instrument_id: &InstrumentId,
        price: f64,
        size: f64,
        side: Signal,
        commission_config: &CommissionConfig,
    ) {
        let state = self.states.entry(instrument_id.clone()).or_insert_with(|| {
            InstrumentState::new(self.initial_equity_per_instrument)
        });

        let comm = commission_config.compute(price, size.abs());
        state.commissions += comm;
        state.equity -= comm;

        if state.position == 0.0 {
            // Open new position
            state.position = if side == Signal::Buy { size } else { -size };
            state.entry_price = price;
        } else {
            // Add to existing position (average in)
            let is_same_side = (state.position > 0.0 && side == Signal::Buy)
                || (state.position < 0.0 && side == Signal::Sell);
            if is_same_side {
                let old_pos = state.position.abs();
                let new_fill_pos = size.abs();
                state.entry_price =
                    (state.entry_price * old_pos + price * new_fill_pos) / (old_pos + new_fill_pos);
                state.position = if state.position > 0.0 {
                    old_pos + new_fill_pos
                } else {
                    -(old_pos + new_fill_pos)
                };
            } else {
                // Reversing — close then open
                let pnl = if state.position > 0.0 {
                    (price - state.entry_price) * state.position.abs()
                } else {
                    (state.entry_price - price) * state.position.abs()
                };
                state.realized_pnl += pnl;
                state.equity += pnl;
                state.position = if side == Signal::Buy { size } else { -size };
                state.entry_price = price;
            }
        }
        state.num_trades += 1;
    }

    /// Close a position for the given instrument and return the realized PnL.
    pub fn close_position(
        &mut self,
        instrument_id: &InstrumentId,
        price: f64,
        commission_config: &CommissionConfig,
    ) -> f64 {
        let state = match self.states.get_mut(instrument_id) {
            Some(s) => s,
            None => return 0.0,
        };
        if state.position == 0.0 {
            return 0.0;
        }

        let comm = commission_config.compute(price, state.position.abs());
        state.commissions += comm;

        let pnl = if state.position > 0.0 {
            (price - state.entry_price) * state.position.abs()
        } else {
            (state.entry_price - price) * state.position.abs()
        };

        state.realized_pnl += pnl;
        state.equity += pnl;
        state.equity -= comm;
        state.position = 0.0;
        state.entry_price = 0.0;
        state.num_trades += 1;

        pnl
    }

    /// Total realized PnL across all instruments.
    pub fn total_realized_pnl(&self) -> f64 {
        self.states.values().map(|s| s.realized_pnl).sum()
    }

    /// Total commissions paid across all instruments.
    pub fn total_commissions(&self) -> f64 {
        self.states.values().map(|s| s.commissions).sum()
    }

    /// Portfolio-level peak equity and max drawdown.
    pub fn portfolio_peak_equity(&self) -> f64 {
        self.states.values().map(|s| s.peak_equity).sum()
    }

    /// Portfolio-level max drawdown (maximum of all instruments' max drawdowns).
    pub fn portfolio_max_drawdown(&self) -> f64 {
        self.states
            .values()
            .map(|s| s.max_drawdown)
            .fold(0.0, f64::max)
    }

    /// Total number of trades across all instruments.
    pub fn total_trades(&self) -> usize {
        self.states.values().map(|s| s.num_trades).sum()
    }

    /// Update peak tracking for all instruments after a tick.
    pub fn update_peaks(&mut self, prices: &HashMap<InstrumentId, f64>) {
        for (id, state) in self.states.iter_mut() {
            if let Some(&price) = prices.get(id) {
                state.update_peak(price);
            }
        }
    }

    /// Run a portfolio backtest over a merged cursor.
    ///
    /// Calls `strategy.on_trade()` per tick. Strategy returns `Signal::Buy/Sell/Close`.
    /// Position opens/closes are managed via `open_position`/`close_position`.
    /// Commission is charged on each fill. Peak equity and max drawdown are tracked.
    pub fn run_portfolio<S: PortfolioStrategy + Clone>(
        &mut self,
        cursor: &mut MergeCursor<'_>,
        config: &PortfolioConfig,
        strategy_factory: impl Fn() -> S,
    ) {
        let mut strategy = strategy_factory();
        let mut prices: HashMap<InstrumentId, f64> = HashMap::new();
        let mut last_signal: HashMap<InstrumentId, Signal> = HashMap::new();

        // Initialize last_signal for all registered instruments
        for id in self.states.keys() {
            last_signal.insert(id.clone(), Signal::Close);
        }

        while let Some(event) = cursor.advance() {
            let instrument_id = event.instrument_id.clone();
            let price = event.tick.price_int as f64 / 1_000_000_000.0;
            let size = event.tick.size_int as f64 / 1_000_000_000.0;

            // Ensure instrument is registered
            if !self.states.contains_key(&instrument_id) {
                self.register_instrument(instrument_id.clone());
                last_signal.insert(instrument_id.clone(), Signal::Close);
            }

            prices.insert(instrument_id.clone(), price);

            // Get current position for this instrument
            let current_position = self.state(&instrument_id).map(|s| s.position).unwrap_or(0.0);

            // Strategy signal
            let signal = strategy.on_trade(
                instrument_id.clone(),
                event.tick.timestamp_ns,
                price,
                size,
                self,
            );

            // SL/TP circuit breaker — close on significant adverse move
            let sl_tp_signal = self.check_sl_tp(&instrument_id, price, config);
            let final_signal = if sl_tp_signal == Some(Signal::Close) {
                Signal::Close
            } else {
                signal
            };

            let last_sig = last_signal.get(&instrument_id).copied().unwrap_or(Signal::Close);

            if final_signal != last_sig {
                match final_signal {
                    Signal::Buy => {
                        if current_position <= 0.0 {
                            if current_position < 0.0 {
                                self.close_position(&instrument_id, price, &config.commission);
                            }
                            self.open_position(&instrument_id, price, size, Signal::Buy, &config.commission);
                        }
                    }
                    Signal::Sell => {
                        if current_position >= 0.0 {
                            if current_position > 0.0 {
                                self.close_position(&instrument_id, price, &config.commission);
                            }
                            self.open_position(&instrument_id, price, size, Signal::Sell, &config.commission);
                        }
                    }
                    Signal::Close => {
                        if current_position != 0.0 {
                            self.close_position(&instrument_id, price, &config.commission);
                        }
                    }
                }
                last_signal.insert(instrument_id, final_signal);
            }

            // Update peaks after each tick
            self.update_peaks(&prices);
        }
    }

    /// Simple SL/TP check for a given instrument.
    /// Returns `Some(Signal::Close)` if stop-loss or take-profit is triggered.
    fn check_sl_tp(&self, instrument_id: &InstrumentId, current_price: f64, config: &PortfolioConfig) -> Option<Signal> {
        let state = self.state(instrument_id)?;
        if state.position == 0.0 || state.entry_price == 0.0 {
            return None;
        }

        let pnl_pct = if state.position > 0.0 {
            (current_price - state.entry_price) / state.entry_price * 100.0
        } else {
            (state.entry_price - current_price) / state.entry_price * 100.0
        };

        // Stop-loss
        if pnl_pct <= -config.stop_loss_pct {
            return Some(Signal::Close);
        }
        // Take-profit
        if pnl_pct >= config.take_profit_pct {
            return Some(Signal::Close);
        }

        None
    }

    /// Final equity after unrealized PnL is settled at the given prices.
    pub fn final_equity(&self, prices: &HashMap<InstrumentId, f64>) -> f64 {
        let mut total = 0.0;
        for (id, state) in &self.states {
            let price = prices.get(id).copied().unwrap_or(state.entry_price);
            let unrealized = if state.position > 0.0 {
                (price - state.entry_price) * state.position.abs()
            } else if state.position < 0.0 {
                (state.entry_price - price) * state.position.abs()
            } else {
                0.0
            };
            total += state.equity + unrealized - state.commissions;
        }
        total
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
    use crate::engine::CommissionConfig;

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
        portfolio.register_instrument(btc_id.clone());
        assert_eq!(portfolio.num_instruments(), 1);
        let state = portfolio.state(&btc_id).unwrap();
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
        portfolio.register_instruments(&[btc_id.clone(), eth_id.clone()]);

        {
            let btc = portfolio.state_mut(&btc_id).unwrap();
            btc.position = 1.0;
            btc.entry_price = 50000.0;
            btc.update_unrealized_pnl(51000.0);
        }

        {
            let eth = portfolio.state_mut(&eth_id).unwrap();
            eth.position = -1.0;
            eth.entry_price = 3000.0;
            eth.update_unrealized_pnl(2900.0);
        }

        let btc_total = 10000.0 + 1000.0;
        let eth_total = 10000.0 + 100.0;
        assert!((portfolio.portfolio_equity() - (btc_total + eth_total)).abs() < 0.001);
    }

    #[test]
    fn test_open_position_long() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let comm = CommissionConfig::new(0.001);
        portfolio.register_instrument(btc_id.clone());
        portfolio.open_position(&btc_id, 100.0, 1.0, Signal::Buy, &comm);
        let state = portfolio.state(&btc_id).unwrap();
        assert_eq!(state.position, 1.0);
        assert_eq!(state.entry_price, 100.0);
        assert_eq!(state.equity, 10000.0 - 0.001); // commission charged
        assert_eq!(state.commissions, 0.001);
    }

    #[test]
    fn test_open_position_short() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let comm = CommissionConfig::new(0.001);
        portfolio.register_instrument(btc_id.clone());
        portfolio.open_position(&btc_id, 100.0, 1.0, Signal::Sell, &comm);
        let state = portfolio.state(&btc_id).unwrap();
        assert_eq!(state.position, -1.0);
        assert_eq!(state.entry_price, 100.0);
    }

    #[test]
    fn test_open_position_add_to_long() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let comm = CommissionConfig::new(0.001);
        portfolio.register_instrument(btc_id.clone());
        // Open long 1 @ 100
        portfolio.open_position(&btc_id, 100.0, 1.0, Signal::Buy, &comm);
        // Add 1 more @ 110 (average entry = (100*1 + 110*1) / 2 = 105)
        portfolio.open_position(&btc_id, 110.0, 1.0, Signal::Buy, &comm);
        let state = portfolio.state(&btc_id).unwrap();
        assert_eq!(state.position, 2.0);
        assert!((state.entry_price - 105.0).abs() < 0.001);
        assert_eq!(state.commissions, 0.002);
    }

    #[test]
    fn test_open_position_reverse_short_to_long() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let comm = CommissionConfig::new(0.001);
        portfolio.register_instrument(btc_id.clone());
        // Open short 1 @ 100
        portfolio.open_position(&btc_id, 100.0, 1.0, Signal::Sell, &comm);
        // Buy to close short @ 95 → pnl = 100 - 95 = 5
        let pnl = portfolio.close_position(&btc_id, 95.0, &comm);
        assert!((pnl - 5.0).abs() < 0.001);
        let state = portfolio.state(&btc_id).unwrap();
        assert_eq!(state.position, 0.0);
        // Now open long 1 @ 95
        portfolio.open_position(&btc_id, 95.0, 1.0, Signal::Buy, &comm);
        let state = portfolio.state(&btc_id).unwrap();
        assert_eq!(state.position, 1.0);
        assert_eq!(state.entry_price, 95.0);
    }

    #[test]
    fn test_close_position_long() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let comm = CommissionConfig::new(0.001);
        portfolio.register_instrument(btc_id.clone());
        // Open long 1 @ 100
        portfolio.open_position(&btc_id, 100.0, 1.0, Signal::Buy, &comm);
        // Close @ 110 → pnl = (110 - 100) * 1 = 10
        let pnl = portfolio.close_position(&btc_id, 110.0, &comm);
        assert!((pnl - 10.0).abs() < 0.001);
        let state = portfolio.state(&btc_id).unwrap();
        assert_eq!(state.position, 0.0);
        // equity = 10000 - 0.001(entry) - 0.001(exit) + 10
        assert!((state.equity - (10000.0 - 0.002 + 10.0)).abs() < 0.001);
        assert_eq!(state.realized_pnl, 10.0);
    }

    #[test]
    fn test_close_position_short() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let comm = CommissionConfig::new(0.001);
        portfolio.register_instrument(btc_id.clone());
        // Open short 1 @ 100
        portfolio.open_position(&btc_id, 100.0, 1.0, Signal::Sell, &comm);
        // Close @ 90 → pnl = (100 - 90) * 1 = 10
        let pnl = portfolio.close_position(&btc_id, 90.0, &comm);
        assert!((pnl - 10.0).abs() < 0.001);
        let state = portfolio.state(&btc_id).unwrap();
        assert_eq!(state.position, 0.0);
        assert_eq!(state.realized_pnl, 10.0);
    }

    #[test]
    fn test_update_peak_tracking() {
        let mut state = InstrumentState::new(10000.0);
        state.position = 1.0;
        state.entry_price = 100.0;
        // equity = 10000, unrealized = 0
        state.update_peak(100.0);
        assert_eq!(state.peak_equity, 10000.0);
        assert_eq!(state.max_drawdown, 0.0);
        // price rises to 110 → unrealized = 10, equity = 10010
        state.update_peak(110.0);
        assert_eq!(state.peak_equity, 10010.0);
        assert_eq!(state.max_drawdown, 0.0);
        // price drops to 95 → unrealized = -5, equity = 9995
        state.update_peak(95.0);
        assert_eq!(state.peak_equity, 10010.0);
        // drawdown = (10010 - 9995) / 10010 ≈ 0.15%
        assert!(state.max_drawdown > 0.0);
        assert!(state.max_drawdown < 0.5);
    }

    #[test]
    fn test_instrument_state_new_has_all_fields() {
        let state = InstrumentState::new(10000.0);
        assert_eq!(state.position, 0.0);
        assert_eq!(state.entry_price, 0.0);
        assert_eq!(state.equity, 10000.0);
        assert_eq!(state.realized_pnl, 0.0);
        assert_eq!(state.commissions, 0.0);
        assert_eq!(state.num_trades, 0);
        assert_eq!(state.peak_equity, 10000.0);
        assert_eq!(state.max_drawdown, 0.0);
    }
}
