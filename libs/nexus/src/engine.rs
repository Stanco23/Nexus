//! Backtest engine — tick-by-tick and bar-mode simulation.
//!
//! # Architecture
//! - `BacktestEngine` — main entry point, runs backtest loop
//! - `EngineContext` — mutable state during backtest (position, equity, draws)
//! - `Trade` — recorded fill with PnL
//! - `Signal` — Buy/Sell/Close from strategy
//! - `BacktestResult` — final results (equity curve, trades, stats)
//!
//! # Commission Math
//! Applied on BOTH entry AND exit for both long and short:
//! - Commission: `price * size * rate` on each fill
//! - Long PnL: `(exit - entry) * size - total_commission`
//! - Short PnL: `(entry - exit) * size - total_commission`

use crate::buffer::tick_buffer::TickBuffer;
use crate::instrument::InstrumentId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Buy,
    Sell,
    Close,
}

#[derive(Debug, Clone)]
pub struct Trade {
    pub timestamp_ns: u64,
    pub side: Signal,
    pub price: f64,
    pub size: f64,
    pub commission: f64,
    pub pnl: f64,
}

#[derive(Debug, Clone)]
pub struct EngineContext {
    pub position: f64,
    pub equity: f64,
    pub peak_equity: f64,
    pub max_drawdown: f64,
    pub entry_price: f64,
    pub num_trades: usize,
    pub equity_curve: Vec<f64>,
}

impl EngineContext {
    pub fn new(initial_equity: f64) -> Self {
        Self {
            position: 0.0,
            equity: initial_equity,
            peak_equity: initial_equity,
            max_drawdown: 0.0,
            entry_price: 0.0,
            num_trades: 0,
            equity_curve: Vec::new(),
        }
    }

    pub fn unrealized_pnl(&self, current_price: f64) -> f64 {
        if self.position == 0.0 || self.entry_price == 0.0 {
            return 0.0;
        }
        if self.position > 0.0 {
            (current_price - self.entry_price) * self.position.abs()
        } else {
            (self.entry_price - current_price) * self.position.abs()
        }
    }

    pub fn update_equity(&mut self, price: f64) {
        if self.position != 0.0 {
            self.equity += self.unrealized_pnl(price);
        }
        if self.equity > self.peak_equity {
            self.peak_equity = self.equity;
        }
        let dd = (self.peak_equity - self.equity) / self.peak_equity * 100.0;
        if dd > self.max_drawdown {
            self.max_drawdown = dd;
        }
        self.equity_curve.push(self.equity);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CommissionConfig {
    pub rate: f64,
}

impl Default for CommissionConfig {
    fn default() -> Self {
        Self { rate: 0.0 }
    }
}

impl CommissionConfig {
    pub fn new(rate: f64) -> Self {
        Self { rate }
    }

    pub fn compute(&self, _price: f64, size: f64) -> f64 {
        size * self.rate
    }
}

#[derive(Debug, Clone)]
pub struct BacktestResult {
    pub final_equity: f64,
    pub peak_equity: f64,
    pub max_drawdown_pct: f64,
    pub num_trades: usize,
    pub equity_curve: Vec<f64>,
    pub trades: Vec<Trade>,
}

pub struct BacktestEngine {
    _instrument_id: InstrumentId,
    initial_equity: f64,
    commission: CommissionConfig,
    data: Option<Vec<(u64, f64, f64)>>,
    result: Option<BacktestResult>,
}

impl BacktestEngine {
    pub fn new(instrument_id: InstrumentId, initial_equity: f64) -> Self {
        Self {
            _instrument_id: instrument_id,
            initial_equity,
            commission: CommissionConfig::default(),
            data: None,
            result: None,
        }
    }

    pub fn set_commission(&mut self, rate: f64) {
        self.commission = CommissionConfig::new(rate);
    }

    pub fn set_tick_buffer(&mut self, buffer: &TickBuffer) {
        let ticks: Vec<(u64, f64, f64)> = buffer
            .iter()
            .map(|tick| {
                (
                    tick.timestamp_ns,
                    tick.price_int as f64 / 1_000_000_000.0,
                    tick.size_int as f64 / 1_000_000_000.0,
                )
            })
            .collect();
        self.data = Some(ticks);
    }

    pub fn run<S: Strategy>(&mut self, strategy: &mut S) {
        let mut ctx = EngineContext::new(self.initial_equity);
        let mut trades = Vec::new();
        let mut last_signal = Signal::Close;

        let data = self.data.take().expect("No data set");
        for (timestamp, price, size) in data {
            let signal = strategy.on_tick(timestamp, price, size, &mut ctx);

            if signal != last_signal {
                match signal {
                    Signal::Buy => {
                        if ctx.position <= 0.0 {
                            if ctx.position < 0.0 {
                                self.close_position(timestamp, price, &mut ctx, &mut trades);
                            }
                            self.open_position(
                                timestamp,
                                price,
                                size,
                                Signal::Buy,
                                &mut ctx,
                                &mut trades,
                            );
                        }
                    }
                    Signal::Sell => {
                        if ctx.position >= 0.0 {
                            if ctx.position > 0.0 {
                                self.close_position(timestamp, price, &mut ctx, &mut trades);
                            }
                            self.open_position(
                                timestamp,
                                price,
                                size,
                                Signal::Sell,
                                &mut ctx,
                                &mut trades,
                            );
                        }
                    }
                    Signal::Close => {
                        if ctx.position != 0.0 {
                            self.close_position(timestamp, price, &mut ctx, &mut trades);
                        }
                    }
                }
                last_signal = signal;
            }

            ctx.update_equity(price);
        }

        self.result = Some(BacktestResult {
            final_equity: ctx.equity,
            peak_equity: ctx.peak_equity,
            max_drawdown_pct: ctx.max_drawdown,
            num_trades: ctx.num_trades,
            equity_curve: ctx.equity_curve,
            trades,
        });
    }

    fn open_position(
        &self,
        timestamp: u64,
        price: f64,
        size: f64,
        side: Signal,
        ctx: &mut EngineContext,
        trades: &mut Vec<Trade>,
    ) {
        let comm = self.commission.compute(price, size);
        ctx.equity -= comm;
        ctx.position = if side == Signal::Buy { size } else { -size };
        ctx.entry_price = price;
        trades.push(Trade {
            timestamp_ns: timestamp,
            side,
            price,
            size,
            commission: comm,
            pnl: 0.0,
        });
        ctx.num_trades += 1;
    }

    fn close_position(
        &self,
        timestamp: u64,
        price: f64,
        ctx: &mut EngineContext,
        trades: &mut Vec<Trade>,
    ) {
        let comm = self.commission.compute(price, ctx.position.abs());
        ctx.equity -= comm;

        let pnl = if ctx.position > 0.0 {
            (price - ctx.entry_price) * ctx.position.abs()
        } else {
            (ctx.entry_price - price) * ctx.position.abs()
        };
        ctx.equity += pnl;

        let side = if ctx.position > 0.0 {
            Signal::Sell
        } else {
            Signal::Buy
        };
        trades.push(Trade {
            timestamp_ns: timestamp,
            side,
            price,
            size: ctx.position.abs(),
            commission: comm,
            pnl,
        });
        ctx.num_trades += 1;
        ctx.position = 0.0;
        ctx.entry_price = 0.0;
    }

    pub fn result(&self) -> Option<&BacktestResult> {
        self.result.as_ref()
    }
}

pub trait Strategy {
    fn on_tick(
        &mut self,
        timestamp_ns: u64,
        price: f64,
        size: f64,
        ctx: &mut EngineContext,
    ) -> Signal;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_commission_compute() {
        let comm = CommissionConfig::new(0.001);
        let cost = comm.compute(100.0, 1.0);
        assert!((cost - 0.001).abs() < 0.0001);
    }

    #[test]
    fn test_commission_full_round_trip() {
        let comm = CommissionConfig::new(0.001);
        let entry = 100.0;
        let size = 1.0;
        let exit = 110.0;
        let entry_comm = comm.compute(entry, size);
        let exit_comm = comm.compute(exit, size);
        let pnl = (exit - entry) * size - entry_comm - exit_comm;
        assert!((pnl - 9.998).abs() < 0.001, "pnl={}", pnl);
    }

    #[test]
    fn test_short_commission_pnl() {
        let comm = CommissionConfig::new(0.001);
        let entry = 100.0;
        let size = 1.0;
        let exit = 90.0;
        let entry_comm = comm.compute(entry, size);
        let exit_comm = comm.compute(exit, size);
        let pnl = (entry - exit) * size - entry_comm - exit_comm;
        assert!((pnl - 9.998).abs() < 0.001, "pnl={}", pnl);
    }

    #[test]
    fn test_engine_context_new() {
        let ctx = EngineContext::new(10000.0);
        assert_eq!(ctx.position, 0.0);
        assert_eq!(ctx.equity, 10000.0);
        assert_eq!(ctx.peak_equity, 10000.0);
        assert_eq!(ctx.max_drawdown, 0.0);
    }

    #[test]
    fn test_engine_context_unrealized_pnl_long() {
        let mut ctx = EngineContext::new(10000.0);
        ctx.position = 1.0;
        ctx.entry_price = 100.0;
        let pnl = ctx.unrealized_pnl(110.0);
        assert!((pnl - 10.0).abs() < 0.001);
    }

    #[test]
    fn test_engine_context_unrealized_pnl_short() {
        let mut ctx = EngineContext::new(10000.0);
        ctx.position = -1.0;
        ctx.entry_price = 100.0;
        let pnl = ctx.unrealized_pnl(90.0);
        assert!((pnl - 10.0).abs() < 0.001);
    }
}
