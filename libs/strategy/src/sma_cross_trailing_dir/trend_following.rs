//! Trend Following Strategy.
//!
//! Bar-mode strategy using EMA(50) for trend direction and ATR for position sizing.
//! - Buy: price > EMA(50) AND price > EMA(200) (both EMAs aligned bullish)
//! - Stop loss: 2xATR trailing
//! - Take profit: 3xATR
//! - Exit: price < EMA(50) or trailing SL hit

use std::time::Duration;

use crate::context::StrategyCtx;
use crate::indicators::{Atr, Ema, Indicator};
use crate::types::{
    BacktestMode, Bar, InstrumentId, OrderSide, OrderType, ParameterSchema,
    ParameterType, ParameterValue, Signal, Tick,
};
use crate::Strategy;

/// Trend Following Strategy — EMA(50/200) trend with ATR-based position sizing.
pub struct TrendFollowingStrategy {
    /// Strategy name identifier.
    name: String,
    /// Instrument to trade.
    instrument_id: InstrumentId,
    /// EMA fast period.
    ema_fast_period: usize,
    /// EMA slow period.
    ema_slow_period: usize,
    /// ATR period.
    atr_period: usize,
    /// Risk per trade as percentage of equity (e.g. 2.0 = 2%).
    risk_pct: f64,

    // ── Runtime state ────────────────────────────────────────────────────────

    /// Fast EMA (50-period).
    ema_fast: Ema,
    /// Slow EMA (200-period).
    ema_slow: Ema,
    /// ATR indicator.
    atr: Atr,
    /// Whether indicators are warmed up enough to generate signals.
    indicators_ready: bool,
    /// Last signal emitted by the strategy.
    last_signal: Option<Signal>,
    /// Whether a long position is currently open.
    in_position: bool,
    /// Entry price of the current position.
    entry_price: f64,
    /// Stop-loss price (trailing).
    stop_loss: f64,
    /// Take-profit price.
    take_profit: f64,
    /// Highest price reached since entry (for trailing SL).
    highest_since_entry: f64,
    /// Equity used for position sizing. Updated each bar.
    equity: f64,
}

impl TrendFollowingStrategy {
    /// Create a new TrendFollowingStrategy for the given instrument.
    ///
    /// Default parameters: ema_fast=50, ema_slow=200, atr_period=14, risk_pct=2.0
    pub fn new(instrument_id: InstrumentId) -> Self {
        Self::with_params(instrument_id, 50, 200, 14, 2.0)
    }

    /// Builder-style constructor with custom parameters.
    pub fn with_params(
        instrument_id: InstrumentId,
        ema_fast_period: usize,
        ema_slow_period: usize,
        atr_period: usize,
        risk_pct: f64,
    ) -> Self {
        Self {
            name: "trend_following_long".to_string(),
            instrument_id,
            ema_fast_period,
            ema_slow_period,
            atr_period,
            risk_pct,
            ema_fast: Ema::new(ema_fast_period),
            ema_slow: Ema::new(ema_slow_period),
            atr: Atr::new(atr_period),
            indicators_ready: false,
            last_signal: None,
            in_position: false,
            entry_price: 0.0,
            stop_loss: 0.0,
            take_profit: 0.0,
            highest_since_entry: 0.0,
            equity: 0.0,
        }
    }

    /// ATR-based position size: (equity * risk_pct / 100) / atr
    ///
    /// This ensures 1 ATR move = risk_pct% of equity.
    fn compute_position_size(&self, atr_value: f64) -> f64 {
        if atr_value <= 0.0 || self.equity <= 0.0 {
            return 1.0;
        }
        (self.equity * self.risk_pct / 100.0) / atr_value
    }
}

impl Strategy for TrendFollowingStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn mode(&self) -> BacktestMode {
        BacktestMode::Bar
    }

    fn timeframe(&self) -> Option<Duration> {
        Some(Duration::from_secs(3600)) // 1-hour bars
    }

    fn subscribed_instruments(&self) -> Vec<InstrumentId> {
        vec![self.instrument_id.clone()]
    }

    fn parameters(&self) -> Vec<ParameterSchema> {
        vec![
            ParameterSchema {
                name: "ema_fast_period".into(),
                param_type: ParameterType::Int,
                default: ParameterValue::Int(50),
                bounds: Some((5.0, 500.0)),
                description: "Fast EMA period (trend filter)".into(),
            },
            ParameterSchema {
                name: "ema_slow_period".into(),
                param_type: ParameterType::Int,
                default: ParameterValue::Int(200),
                bounds: Some((10.0, 1000.0)),
                description: "Slow EMA period (trend confirmation)".into(),
            },
            ParameterSchema {
                name: "atr_period".into(),
                param_type: ParameterType::Int,
                default: ParameterValue::Int(14),
                bounds: Some((2.0, 100.0)),
                description: "ATR period for position sizing".into(),
            },
            ParameterSchema {
                name: "risk_pct".into(),
                param_type: ParameterType::Float,
                default: ParameterValue::Float(2.0),
                bounds: Some((0.1, 10.0)),
                description: "Risk per trade as percentage of equity (e.g. 2.0 = 2%)".into(),
            },
        ]
    }

    fn clone_box(&self) -> Box<dyn Strategy> {
        Box::new(Self {
            name: self.name.clone(),
            instrument_id: self.instrument_id.clone(),
            ema_fast_period: self.ema_fast_period,
            ema_slow_period: self.ema_slow_period,
            atr_period: self.atr_period,
            risk_pct: self.risk_pct,
            ema_fast: Ema::new(self.ema_fast_period),
            ema_slow: Ema::new(self.ema_slow_period),
            atr: Atr::new(self.atr_period),
            indicators_ready: false,
            last_signal: None,
            in_position: false,
            entry_price: 0.0,
            stop_loss: 0.0,
            take_profit: 0.0,
            highest_since_entry: 0.0,
            equity: self.equity,
        })
    }

    fn on_init(&mut self) {
        assert!(self.ema_fast_period >= 2, "ema_fast_period must be >= 2");
        assert!(
            self.ema_slow_period > self.ema_fast_period,
            "ema_slow_period must be > ema_fast_period"
        );
        assert!(self.atr_period >= 2, "atr_period must be >= 2");
        assert!(self.risk_pct > 0.0, "risk_pct must be positive");
    }

    fn on_start(&mut self) {
        self.last_signal = None;
        self.in_position = false;
    }

    fn on_reset(&mut self) {
        self.ema_fast.reset();
        self.ema_slow.reset();
        self.atr.reset();
        self.indicators_ready = false;
        self.last_signal = None;
        self.in_position = false;
        self.entry_price = 0.0;
        self.stop_loss = 0.0;
        self.take_profit = 0.0;
        self.highest_since_entry = 0.0;
    }

    /// Bar-mode strategy — not called when timeframe() is set.
    fn on_trade(
        &mut self,
        _instrument_id: InstrumentId,
        _tick: &Tick,
        _ctx: &mut dyn StrategyCtx,
    ) -> Option<Signal> {
        None
    }

    /// Called on each 1-hour bar. Implements EMA trend + ATR sizing + trailing SL/TP.
    fn on_bar(
        &mut self,
        instrument_id: InstrumentId,
        bar: &Bar,
        ctx: &mut dyn StrategyCtx,
    ) -> Option<Signal> {
        if instrument_id != self.instrument_id {
            return None;
        }

        // Update equity from context each bar
        self.equity = ctx.account_equity();

        let close = bar.close;
        let high = bar.high;

        // Update EMA indicators
        let ema_fast_val = self.ema_fast.update(close);
        let ema_slow_val = self.ema_slow.update(close);

        // ATR with full true range (high/low/prev_close)
        let atr_val = crate::indicators::atr_update(&mut self.atr, high, bar.low, close);

        // Indicators ready when both EMAs and ATR are warmed up
        let ema_ready = ema_fast_val.is_some() && ema_slow_val.is_some();
        let atr_ready = atr_val.is_some();

        if ema_ready && atr_ready {
            self.indicators_ready = true;
        }

        if !self.indicators_ready {
            self.last_signal = None;
            return None;
        }

        let ema_fast_val = ema_fast_val.unwrap();
        let ema_slow_val = ema_slow_val.unwrap();
        let atr_val = atr_val.unwrap();

        // ── Trailing stop management ───────────────────────────────────────

        if self.in_position {
            // Track highest price and raise stop (trailing SL)
            if high > self.highest_since_entry {
                self.highest_since_entry = high;
                let new_sl = self.highest_since_entry - 2.0 * atr_val;
                if new_sl > self.stop_loss {
                    self.stop_loss = new_sl;
                }
            }

            // Exit conditions:
            // 1. Price drops below EMA(50) — trend reversal
            // 2. Price hits stop-loss
            // 3. Price hits take-profit
            if close < ema_fast_val {
                self.in_position = false;
                self.last_signal = Some(Signal::Sell);
                ctx.emit_signal(Signal::Sell);
                return self.last_signal.take();
            }
            if close <= self.stop_loss {
                self.in_position = false;
                self.last_signal = Some(Signal::Sell);
                ctx.emit_signal(Signal::Sell);
                return self.last_signal.take();
            }
            if close >= self.take_profit {
                self.in_position = false;
                self.last_signal = Some(Signal::Sell);
                ctx.emit_signal(Signal::Sell);
                return self.last_signal.take();
            }

            self.last_signal = None;
            return None;
        }

        // ── Entry logic ───────────────────────────────────────────────────

        // Buy: price > EMA(50) AND price > EMA(200) (both EMAs aligned bullish)
        if close > ema_fast_val && close > ema_slow_val {
            let size = self.compute_position_size(atr_val);
            let sl = Some(close - 2.0 * atr_val);
            let tp = Some(close + 3.0 * atr_val);

            // Submit market order with SL/TP
            ctx.submit_with_sl_tp(
                self.instrument_id.clone(),
                OrderSide::Buy,
                OrderType::Market,
                close,
                size,
                sl,
                tp,
            );

            self.in_position = true;
            self.entry_price = close;
            self.highest_since_entry = high;
            self.stop_loss = close - 2.0 * atr_val;
            self.take_profit = close + 3.0 * atr_val;
            self.last_signal = Some(Signal::Buy);
            ctx.emit_signal(Signal::Buy);
            return self.last_signal.take();
        }

        self.last_signal = None;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::InstrumentId;

    fn make_bar(close: f64, range: f64) -> Bar {
        Bar {
            instrument_id: InstrumentId::from("BTCUSDT.BINANCE"),
            ts_ns: 0,
            open: close,
            high: close + range,
            low: close - range,
            close,
            volume: 1.0,
        }
    }

    struct DummyCtx {
        equity_val: f64,
    }
    impl DummyCtx {
        fn new(equity: f64) -> Self {
            Self { equity_val: equity }
        }
    }
    impl StrategyCtx for DummyCtx {
        fn current_price(&self, _instrument_id: InstrumentId) -> f64 {
            0.0
        }
        fn position(&self, _instrument_id: InstrumentId) -> Option<PositionSide> {
            None
        }
        fn account_equity(&self) -> f64 {
            self.equity_val
        }
        fn unrealized_pnl(&self, _instrument_id: InstrumentId) -> f64 {
            0.0
        }
        fn pending_orders(&self, _instrument_id: InstrumentId) -> Vec<crate::types::Order> {
            vec![]
        }
        fn subscribe_instruments(&mut self, _instruments: Vec<InstrumentId>) {}
        fn subscribe_signal(&mut self, _name: &str, _callback: crate::signals::SignalCallback) {}
        fn submit_limit(
            &mut self,
            _instrument_id: InstrumentId,
            _side: OrderSide,
            _price: f64,
            _size: f64,
        ) -> u64 {
            0
        }
        fn submit_market(
            &mut self,
            _instrument_id: InstrumentId,
            _side: OrderSide,
            _size: f64,
        ) -> u64 {
            0
        }
        fn submit_with_sl_tp(
            &mut self,
            _instrument_id: InstrumentId,
            _side: OrderSide,
            _order_type: OrderType,
            _price: f64,
            _size: f64,
            _sl: Option<f64>,
            _tp: Option<f64>,
        ) -> u64 {
            0
        }
        fn submit_trailing(
            &mut self,
            _instrument_id: InstrumentId,
            _side: OrderSide,
            _price: f64,
            _size: f64,
            _trailing_delta_pct: f64,
        ) -> u64 {
            0
        }
        fn emit_signal(&mut self, _signal: Signal) {}
    }

    #[test]
    fn test_on_reset_clears_state() {
        let id = InstrumentId::from("BTCUSDT.BINANCE");
        let mut strat = TrendFollowingStrategy::new(id.clone());
        let mut ctx = DummyCtx::new(100_000.0);

        // Warm up indicators (need 200 bars for EMA(200))
        for i in 0..250 {
            let close = 50_000.0 + i as f64 * 10.0;
            strat.on_bar(id.clone(), &make_bar(close, 50.0), &mut ctx);
        }

        strat.on_reset();

        assert!(!strat.indicators_ready);
        assert!(!strat.in_position);
        assert!(strat.last_signal.is_none());
    }

    #[test]
    fn test_clone_box_name_and_params() {
        let id = InstrumentId::from("BTCUSDT.BINANCE");
        let strat = TrendFollowingStrategy::new(id);
        let clone = strat.clone_box();

        assert_eq!(clone.name(), "trend_following_long");
        assert_eq!(clone.parameters().len(), 4);
    }

    #[test]
    fn test_equity_updated_each_bar() {
        let id = InstrumentId::from("BTCUSDT.BINANCE");
        let mut strat = TrendFollowingStrategy::new(id.clone());
        let mut ctx = DummyCtx::new(100_000.0);

        // Before any bar, equity starts at 0
        assert_eq!(strat.equity, 0.0);

        // After first bar, equity is populated from context
        strat.on_bar(id.clone(), &make_bar(50_000.0, 50.0), &mut ctx);
        assert_eq!(strat.equity, 100_000.0);
    }
}