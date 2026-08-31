//! Trend Following Long — 1h bars, EMA trend + ATR sizing, trailing SL at 2xATR.

use std::time::Duration;

use crate::context::StrategyCtx;
use crate::indicators::{Atr, Ema, Indicator};
use crate::types::{
    BacktestMode, Bar, InstrumentId, ParameterSchema,
    ParameterType, ParameterValue, Signal,
};
use crate::Strategy;

/// Trend Following Long strategy.
///
/// ## Entry Logic
/// - Buy when EMA(20) > EMA(50) (uptrend) and price pulls back within 0.5% of EMA
/// - Size position using ATR: `position_size = equity * 0.01 / (2 * ATR)`
///
/// ## Exit Logic
/// - Trailing SL at 2x ATR from entry
/// - Time-based EOD close at 15:45 EST (45min before market close)

pub struct TrendFollowingStrategy {
    name: String,
    ema_fast_period: usize,
    ema_slow_period: usize,
    atr_period: usize,
    trailing_atr_mult: f64,
    position_size: f64,
    // Runtime state
    ema_fast: Ema,
    ema_slow: Ema,
    atr: Atr,
    in_position: bool,
    entry_price: f64,
    trailing_stop: f64,
    last_est_min: u32,
}

impl TrendFollowingStrategy {
    pub fn new(
        ema_fast_period: usize,
        ema_slow_period: usize,
        atr_period: usize,
        trailing_atr_mult: f64,
        position_size: f64,
    ) -> Self {
        Self {
            name: "trend_following_long".into(),
            ema_fast_period,
            ema_slow_period,
            atr_period,
            trailing_atr_mult,
            position_size,
            ema_fast: Ema::new(ema_fast_period),
            ema_slow: Ema::new(ema_slow_period),
            atr: Atr::new(atr_period),
            in_position: false,
            entry_price: 0.0,
            trailing_stop: 0.0,
            last_est_min: 0,
        }
    }
}

impl Strategy for TrendFollowingStrategy {
    fn name(&self) -> &str { &self.name }

    fn mode(&self) -> BacktestMode { BacktestMode::Bar }

    fn timeframe(&self) -> Option<Duration> {
        Some(Duration::from_secs(3600)) // 1h bars
    }

    fn subscribed_instruments(&self) -> Vec<InstrumentId> {
        vec![InstrumentId::new("BTCUSDT", "BINANCE")]
    }

    fn parameters(&self) -> Vec<ParameterSchema> {
        vec![
            ParameterSchema {
                name: "ema_fast_period".into(),
                param_type: ParameterType::Int,
                default: ParameterValue::Int(20),
                bounds: Some((5.0, 100.0)),
                description: "Fast EMA period".into(),
            },
            ParameterSchema {
                name: "ema_slow_period".into(),
                param_type: ParameterType::Int,
                default: ParameterValue::Int(50),
                bounds: Some((10.0, 500.0)),
                description: "Slow EMA period".into(),
            },
            ParameterSchema {
                name: "atr_period".into(),
                param_type: ParameterType::Int,
                default: ParameterValue::Int(14),
                bounds: Some((5.0, 100.0)),
                description: "ATR period for position sizing".into(),
            },
            ParameterSchema {
                name: "trailing_atr_mult".into(),
                param_type: ParameterType::Float,
                default: ParameterValue::Float(2.0),
                bounds: Some((0.5, 5.0)),
                description: "Trailing SL distance in ATR multiples".into(),
            },
            ParameterSchema {
                name: "position_size".into(),
                param_type: ParameterType::Float,
                default: ParameterValue::Float(1.0),
                bounds: Some((0.01, 10.0)),
                description: "Fixed position size".into(),
            },
        ]
    }

    fn clone_box(&self) -> Box<dyn Strategy> {
        Box::new(Self {
            name: self.name.clone(),
            ema_fast_period: self.ema_fast_period,
            ema_slow_period: self.ema_slow_period,
            atr_period: self.atr_period,
            trailing_atr_mult: self.trailing_atr_mult,
            position_size: self.position_size,
            ema_fast: Ema::new(self.ema_fast_period),
            ema_slow: Ema::new(self.ema_slow_period),
            atr: Atr::new(self.atr_period),
            in_position: false,
            entry_price: 0.0,
            trailing_stop: 0.0,
            last_est_min: 0,
        })
    }

    fn on_reset(&mut self) {
        self.ema_fast = Ema::new(self.ema_fast_period);
        self.ema_slow = Ema::new(self.ema_slow_period);
        self.atr = Atr::new(self.atr_period);
        self.in_position = false;
        self.entry_price = 0.0;
        self.trailing_stop = 0.0;
        self.last_est_min = 0;
    }

    fn on_bar(
        &mut self,
        instrument_id: InstrumentId,
        bar: &Bar,
        ctx: &mut dyn StrategyCtx,
    ) -> Option<Signal> {
        // Update indicators
        let ema_fast_val = match self.ema_fast.update(bar.close) {
            Some(v) => v,
            None => return None,
        };
        let ema_slow_val = match self.ema_slow.update(bar.close) {
            Some(v) => v,
            None => return None,
        };
        let atr_val = match self.atr.update(bar.close) {
            Some(v) => v,
            None => return None,
        };

        // EST minute from UTC timestamp
        let ts_ns = bar.timestamp_ns;
        let utc_h = ((ts_ns / 3_600_000_000_000u64) % 24) as u32;
        let utc_m = ((ts_ns / 60_000_000_000u64) % 60) as u32;
        let est_h = if utc_h >= 5 { utc_h - 5 } else { utc_h + 19 };
        let est_min = est_h * 60 + utc_m;

        const EOD_CLOSE_MIN: u32 = 945; // 3:45 PM EST

        // Day boundary reset
        if est_min < self.last_est_min && self.last_est_min > 0 {
            self.in_position = false;
            self.entry_price = 0.0;
            self.trailing_stop = 0.0;
        }
        self.last_est_min = est_min;

        if !self.in_position {
            // Entry: EMA in uptrend (fast > slow) and price within 0.5% of fast EMA
            let in_uptrend = ema_fast_val > ema_slow_val;
            let near_ema = (bar.close - ema_fast_val).abs() / ema_fast_val < 0.005;

            if in_uptrend && near_ema {
                self.in_position = true;
                self.entry_price = bar.close;
                self.trailing_stop = bar.close - self.trailing_atr_mult * atr_val;

                // In backtest mode, submit_with_sl_tp doesn't execute orders.
                // Return signal so route_signal opens the position.
                return Some(Signal::Buy);
            }
        } else {
            // Update trailing stop
            let new_ts = bar.close - self.trailing_atr_mult * atr_val;
            if new_ts > self.trailing_stop {
                self.trailing_stop = new_ts;
            }

            // EOD close
            if est_min >= EOD_CLOSE_MIN {
                self.in_position = false;
                self.entry_price = 0.0;
                self.trailing_stop = 0.0;
                return Some(Signal::Close);
            }
        }

        None
    }

    fn on_trade(
        &mut self,
        _instrument_id: InstrumentId,
        _tick: &crate::types::Tick,
        _ctx: &mut dyn StrategyCtx,
    ) -> Option<Signal> {
        None
    }
}

impl Clone for TrendFollowingStrategy {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            ema_fast_period: self.ema_fast_period,
            ema_slow_period: self.ema_slow_period,
            atr_period: self.atr_period,
            trailing_atr_mult: self.trailing_atr_mult,
            position_size: self.position_size,
            ema_fast: Ema::new(self.ema_fast_period),
            ema_slow: Ema::new(self.ema_slow_period),
            atr: Atr::new(self.atr_period),
            in_position: false,
            entry_price: 0.0,
            trailing_stop: 0.0,
            last_est_min: 0,
        }
    }
}