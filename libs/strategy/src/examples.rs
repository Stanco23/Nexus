//! Example strategy implementations.

use crate::context::StrategyCtx;
use crate::indicators::{Indicator, Rsi};
use crate::types::{BacktestMode, Bar, InstrumentId, ParameterSchema, ParameterType, ParameterValue, Signal, Tick};
use crate::Strategy;

/// EMA Crossover Strategy.
///
/// Buy when fast_ema crosses above slow_ema, sell on reverse crossover.
pub struct EmaCrossStrategy {
    pub fast_period: usize,
    pub slow_period: usize,
    pub instrument_id: InstrumentId,
    // Runtime state
    fast_ema: f64,
    slow_ema: f64,
    initialized: bool,
}

impl EmaCrossStrategy {
    pub fn new(instrument_id: InstrumentId, fast_period: usize, slow_period: usize) -> Self {
        Self {
            fast_period,
            slow_period,
            instrument_id,
            fast_ema: 0.0,
            slow_ema: 0.0,
            initialized: false,
        }
    }
}

impl Strategy for EmaCrossStrategy {
    fn name(&self) -> &str {
        "EmaCrossStrategy"
    }

    fn mode(&self) -> BacktestMode {
        BacktestMode::Tick
    }

    fn subscribed_instruments(&self) -> Vec<InstrumentId> {
        vec![self.instrument_id.clone()]
    }

    fn parameters(&self) -> Vec<ParameterSchema> {
        vec![
            ParameterSchema {
                name: "fast_period".into(),
                param_type: ParameterType::Int,
                default: ParameterValue::Int(12),
                bounds: Some((1.0, 200.0)),
                description: "Fast EMA period".into(),
            },
            ParameterSchema {
                name: "slow_period".into(),
                param_type: ParameterType::Int,
                default: ParameterValue::Int(26),
                bounds: Some((1.0, 200.0)),
                description: "Slow EMA period".into(),
            },
        ]
    }

    fn clone_box(&self) -> Box<dyn Strategy> {
        Box::new(Self {
            fast_period: self.fast_period,
            slow_period: self.slow_period,
            instrument_id: self.instrument_id.clone(),
            fast_ema: self.fast_ema,
            slow_ema: self.slow_ema,
            initialized: self.initialized,
        })
    }

    fn on_trade(
        &mut self,
        instrument_id: InstrumentId,
        tick: &Tick,
        _ctx: &mut dyn StrategyCtx,
    ) -> Option<Signal> {
        if instrument_id != self.instrument_id {
            return None;
        }

        if !self.initialized {
            self.fast_ema = tick.price;
            self.slow_ema = tick.price;
            self.initialized = true;
            return None;
        }

        let prev_fast = self.fast_ema;
        let prev_slow = self.slow_ema;

        let alpha = 2.0 / (self.fast_period as f64 + 1.0);
        self.fast_ema = tick.price * alpha + self.fast_ema * (1.0 - alpha);
        let slow_alpha = 2.0 / (self.slow_period as f64 + 1.0);
        self.slow_ema = tick.price * slow_alpha + self.slow_ema * (1.0 - slow_alpha);

        // Buy signal: fast EMA crosses above slow EMA
        if self.fast_ema > self.slow_ema && prev_fast <= prev_slow {
            return Some(Signal::Buy);
        }

        // Sell signal: fast EMA crosses below slow EMA
        if self.fast_ema < self.slow_ema && prev_fast >= prev_slow {
            return Some(Signal::Sell);
        }

        None
    }

    fn on_bar(
        &mut self,
        instrument_id: InstrumentId,
        bar: &Bar,
        _ctx: &mut dyn StrategyCtx,
    ) -> Option<Signal> {
        if instrument_id != self.instrument_id {
            return None;
        }
        if !self.initialized {
            self.fast_ema = bar.close;
            self.slow_ema = bar.close;
            self.initialized = true;
            return None;
        }
        let alpha = 2.0 / (self.fast_period as f64 + 1.0);
        self.fast_ema = bar.close * alpha + self.fast_ema * (1.0 - alpha);
        let slow_alpha = 2.0 / (self.slow_period as f64 + 1.0);
        self.slow_ema = bar.close * slow_alpha + self.slow_ema * (1.0 - slow_alpha);
        None
    }
}

/// RSI Overbought/Oversold Strategy.
///
/// Buy when RSI drops below oversold threshold, sell when RSI rises above overbought threshold.
pub struct RsiStrategy {
    pub period: usize,
    pub overbought: f64,
    pub oversold: f64,
    pub instrument_id: InstrumentId,
    // Runtime state
    rsi: Rsi,
    last_signal: Option<Signal>,
}

impl RsiStrategy {
    pub fn new(
        instrument_id: InstrumentId,
        period: usize,
        overbought: f64,
        oversold: f64,
    ) -> Self {
        Self {
            period,
            overbought,
            oversold,
            instrument_id,
            rsi: Rsi::new(period),
            last_signal: None,
        }
    }
}

impl Strategy for RsiStrategy {
    fn name(&self) -> &str {
        "RsiStrategy"
    }

    fn mode(&self) -> BacktestMode {
        BacktestMode::Tick
    }

    fn subscribed_instruments(&self) -> Vec<InstrumentId> {
        vec![self.instrument_id.clone()]
    }

    fn parameters(&self) -> Vec<ParameterSchema> {
        vec![
            ParameterSchema {
                name: "period".into(),
                param_type: ParameterType::Int,
                default: ParameterValue::Int(14),
                bounds: Some((2.0, 100.0)),
                description: "RSI averaging period".into(),
            },
            ParameterSchema {
                name: "overbought".into(),
                param_type: ParameterType::Float,
                default: ParameterValue::Float(70.0),
                bounds: Some((50.0, 95.0)),
                description: "Overbought threshold".into(),
            },
            ParameterSchema {
                name: "oversold".into(),
                param_type: ParameterType::Float,
                default: ParameterValue::Float(30.0),
                bounds: Some((5.0, 50.0)),
                description: "Oversold threshold".into(),
            },
        ]
    }

    fn clone_box(&self) -> Box<dyn Strategy> {
        Box::new(Self {
            period: self.period,
            overbought: self.overbought,
            oversold: self.oversold,
            instrument_id: self.instrument_id.clone(),
            rsi: self.rsi.clone(),
            last_signal: self.last_signal,
        })
    }

    fn on_trade(
        &mut self,
        instrument_id: InstrumentId,
        tick: &Tick,
        _ctx: &mut dyn StrategyCtx,
    ) -> Option<Signal> {
        if instrument_id != self.instrument_id {
            return None;
        }

        let rsi_value = self.rsi.update(tick.price);
        self.last_signal = None;

        if let Some(rsi) = rsi_value {
            if rsi < self.oversold {
                self.last_signal = Some(Signal::Buy);
            } else if rsi > self.overbought {
                self.last_signal = Some(Signal::Sell);
            }
        }

        self.last_signal
    }

    fn on_bar(
        &mut self,
        instrument_id: InstrumentId,
        bar: &Bar,
        _ctx: &mut dyn StrategyCtx,
    ) -> Option<Signal> {
        if instrument_id != self.instrument_id {
            return None;
        }
        self.rsi.update(bar.close);
        None
    }
}
