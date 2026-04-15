//! Backtest engine — tick-by-tick simulation.

use crate::book::{OrderBook, OrderEmulator, OrderId, Side};
use crate::buffer::tick_buffer::{TickBuffer, TradeFlowStats};
use crate::engine::orders::OrderManager;
use crate::engine::risk::RiskEngine;
use crate::instrument::InstrumentId;
use crate::slippage::SlippageConfig;

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
    pub fee: f64,
    pub is_maker: bool,
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
    /// Fee rate for limit orders (resting in book, maker).
    pub maker_fee: f64,
    /// Fee rate for market orders (aggressing book, taker).
    pub taker_fee: f64,
}

impl Default for CommissionConfig {
    fn default() -> Self {
        Self { rate: 0.0, maker_fee: 0.0, taker_fee: 0.0 }
    }
}

impl CommissionConfig {
    pub fn new(rate: f64) -> Self {
        Self { rate, maker_fee: 0.0, taker_fee: 0.0 }
    }

    pub fn with_maker_fee(mut self, fee: f64) -> Self {
        self.maker_fee = fee;
        self
    }

    pub fn with_taker_fee(mut self, fee: f64) -> Self {
        self.taker_fee = fee;
        self
    }

    pub fn compute(&self, _price: f64, size: f64) -> f64 {
        size * self.rate
    }

    pub fn maker_commission(&self, _price: f64, size: f64) -> f64 {
        size * self.maker_fee
    }

    pub fn taker_commission(&self, _price: f64, size: f64) -> f64 {
        size * self.taker_fee
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
    slippage_config: SlippageConfig,
    order_book: OrderBook,
    order_emulator: OrderEmulator,
    order_manager: OrderManager,
    risk_engine: Option<RiskEngine>,
    data: Option<Vec<(u64, f64, f64, f64)>>,
    result: Option<BacktestResult>,
}

impl BacktestEngine {
    pub fn new(instrument_id: InstrumentId, initial_equity: f64) -> Self {
        Self {
            _instrument_id: instrument_id,
            initial_equity,
            commission: CommissionConfig::default(),
            slippage_config: SlippageConfig::new(),
            order_book: OrderBook::new(),
            order_emulator: OrderEmulator::new(),
            order_manager: OrderManager::new(),
            risk_engine: None,
            data: None,
            result: None,
        }
    }

    pub fn set_commission(&mut self, rate: f64) {
        self.commission = CommissionConfig::new(rate);
    }

    pub fn set_tick_buffer(&mut self, buffer: &TickBuffer) {
        let ticks: Vec<(u64, f64, f64, f64)> = buffer
            .iter()
            .map(|tick| {
                (
                    tick.timestamp_ns,
                    tick.price_int as f64 / 1_000_000_000.0,
                    tick.size_int as f64 / 1_000_000_000.0,
                    tick.vpin,
                )
            })
            .collect();
        self.data = Some(ticks);
    }

    pub fn set_risk_engine(&mut self, config: crate::engine::risk::RiskConfig) {
        self.risk_engine = Some(RiskEngine::new(config, self.initial_equity));
    }

    pub fn run<S: Strategy>(&mut self, strategy: &mut S) {
        let mut ctx = EngineContext::new(self.initial_equity);
        let mut trades = Vec::new();
        let mut last_signal = Signal::Close;

        let data = self.data.take().expect("No data set");
        for (timestamp, price, size, vpin) in data {
            // Build TradeFlowStats for OrderBook update
            let tick = TradeFlowStats {
                timestamp_ns: timestamp,
                price_int: (price * 1_000_000_000.0) as i64,
                size_int: (size * 1_000_000_000.0) as i64,
                side: 0,
                cum_buy_volume: 0,
                cum_sell_volume: 0,
                vpin,
                bucket_index: 0,
            };
            self.order_book.update_from_trade(&tick);

            // Update order emulator market volume
            self.order_emulator.update_market_volume(size);

            // Process pending limit order fills — update positions based on queue fills
            let fills = self.order_emulator.process_fills(price, vpin, timestamp, self.commission.maker_fee);
            for fill in &fills {
                let fill_signal = if fill.side == Side::Buy { Signal::Buy } else { Signal::Sell };

                // If fill is Buy but we have a short position, close it first
                if fill_signal == Signal::Buy && ctx.position < 0.0 {
                    self.close_position(fill.timestamp_ns, fill.fill_price, &mut ctx, &mut trades);
                }
                // If fill is Sell but we have a long position, close it first
                if fill_signal == Signal::Sell && ctx.position > 0.0 {
                    self.close_position(fill.timestamp_ns, fill.fill_price, &mut ctx, &mut trades);
                }

                // Open or add to position
                let is_add = ctx.position != 0.0
                    && ((fill_signal == Signal::Buy && ctx.position > 0.0)
                        || (fill_signal == Signal::Sell && ctx.position < 0.0));

                if is_add {
                    // Add to existing position (average in)
                    let old_pos = ctx.position.abs();
                    let new_pos = fill.fill_size;
                    ctx.entry_price = (ctx.entry_price * old_pos + fill.fill_price * new_pos) / (old_pos + new_pos);
                    ctx.position = if fill_signal == Signal::Buy { old_pos + new_pos } else { -(old_pos + new_pos) };
                    let comm = self.commission.compute(fill.fill_price, fill.fill_size);
                    ctx.equity -= comm;
                    trades.push(Trade {
                        timestamp_ns: fill.timestamp_ns,
                        side: fill_signal,
                        price: fill.fill_price,
                        size: fill.fill_size,
                        commission: comm,
                        pnl: 0.0,
                        fee: fill.fee,
                        is_maker: true,
                    });
                    ctx.num_trades += 1;
                } else {
                    // Open new position
                    self.open_position(fill.timestamp_ns, fill.fill_price, fill.fill_size, fill_signal, &mut ctx, &mut trades);
                }
            }

            // Check SL/TP (takes priority over pending fills)
            let sl_tp_signal = self.order_manager.check_sl_tp(price, &ctx);
            let _pending_signal = self.order_manager.check_pending_orders(price, &ctx);
            let _ = _pending_signal; // suppress unused warning until wired

            // Allow strategy to submit limit orders before signal processing
            strategy.on_tick_orders(timestamp, price, &mut ctx);

            // Determine final signal for this tick (SL/TP overrides pending)
            let mut signal = strategy.on_tick(timestamp, price, size, &mut ctx);
            if sl_tp_signal == Some(Signal::Close) {
                signal = Signal::Close;
            }

            // Risk check — block Buy/Sell signals if risk limits would be breached
            if signal != Signal::Close {
                if let Some(ref risk) = self.risk_engine {
                    if let Some(_reason) = risk.check_signal(size, price, ctx.position, ctx.equity, ctx.max_drawdown) {
                        // Risk limit breached — skip this signal
                        last_signal = signal;
                        ctx.update_equity(price);
                        continue;
                    }
                }
            }

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

    fn open_position(&mut self,
        timestamp: u64,
        price: f64,
        size: f64,
        side: Signal,
        ctx: &mut EngineContext,
        trades: &mut Vec<Trade>,
    ) {
        let order_side = if side == Signal::Buy { Side::Buy } else { Side::Sell };
        let fills = self.order_emulator.process_market_order(
            size,
            order_side,
            &self.order_book,
            timestamp,
            self.commission.taker_fee,
        );

        if fills.is_empty() {
            // No liquidity — could not fill
            return;
        }

        // Average fill price across all levels
        let total_cost: f64 = fills.iter().map(|f| f.fill_price * f.fill_size).sum();
        let total_size: f64 = fills.iter().map(|f| f.fill_size).sum();
        let avg_fill_price = if total_size > 0.0 { total_cost / total_size } else { price };

        // Commission (base rate) + taker fee
        let base_commission = self.commission.compute(avg_fill_price, total_size);
        let total_fee: f64 = fills.iter().map(|f| f.fee).sum();

        ctx.equity -= base_commission + total_fee;
        ctx.position = if side == Signal::Buy { total_size } else { -total_size };
        ctx.entry_price = avg_fill_price;

        for fill in &fills {
            trades.push(Trade {
                timestamp_ns: fill.timestamp_ns,
                side,
                price: fill.fill_price,
                size: fill.fill_size,
                commission: base_commission * (fill.fill_size / total_size),
                pnl: 0.0,
                fee: fill.fee,
                is_maker: false,
            });
            ctx.num_trades += 1;
        }
    }

    fn close_position(
        &self,
        timestamp: u64,
        price: f64,
        ctx: &mut EngineContext,
        trades: &mut Vec<Trade>,
    ) {
        let order_size_ticks = (ctx.position.abs() / 0.001).max(1.0);
        let impact_bps = self.slippage_config.compute_impact_bps(order_size_ticks, 0.0);
        let fill_price = price * (1.0 - impact_bps / 10_000.0);

        let comm = self.commission.compute(fill_price, ctx.position.abs());
        ctx.equity -= comm;

        let pnl = if ctx.position > 0.0 {
            (fill_price - ctx.entry_price) * ctx.position.abs()
        } else {
            (ctx.entry_price - fill_price) * ctx.position.abs()
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
            price: fill_price,
            size: ctx.position.abs(),
            commission: comm,
            pnl,
            fee: 0.0,
            is_maker: false,
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

    /// Optional: submit limit orders before the main signal is processed.
    /// Default implementation does nothing. Override to submit limit orders
    /// via `ctx.submit_limit_order(...)`.
    #[inline(always)]
    fn on_tick_orders(&mut self, _timestamp_ns: u64, _price: f64, _ctx: &mut EngineContext) {}

    /// Submit a limit order to the emulator queue.
    /// Can be called from `on_tick_orders`.
    fn submit_limit_order(&self, price: f64, size: f64, side: Side, timestamp_ns: u64, emulator: &mut OrderEmulator) -> OrderId {
        emulator.submit_limit(price, size, side, timestamp_ns)
    }
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
