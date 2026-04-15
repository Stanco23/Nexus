//! Example strategy implementations.

use crate::context::StrategyCtx;
use crate::types::{BacktestMode, Bar, InstrumentId, ParameterSchema, Signal, Tick};
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
    last_price: f64,
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
            last_price: 0.0,
            initialized: false,
        }
    }

    fn update_ema(&mut self, price: f64) {
        let alpha = 2.0 / (self.fast_period as f64 + 1.0);
        if !self.initialized {
            self.fast_ema = price;
            self.slow_ema = price;
            self.initialized = true;
        } else {
            self.fast_ema = price * alpha + self.fast_ema * (1.0 - alpha);
            let slow_alpha = 2.0 / (self.slow_period as f64 + 1.0);
            self.slow_ema = price * slow_alpha + self.slow_ema * (1.0 - slow_alpha);
        }
        self.last_price = price;
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
        vec![]
    }

    fn clone_box(&self) -> Box<dyn Strategy> {
        Box::new(Self {
            fast_period: self.fast_period,
            slow_period: self.slow_period,
            instrument_id: self.instrument_id.clone(),
            fast_ema: self.fast_ema,
            slow_ema: self.slow_ema,
            last_price: self.last_price,
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

        self.update_ema(tick.price);

        if !self.initialized || self.last_price == 0.0 {
            return None;
        }

        // Buy signal: fast EMA crosses above slow EMA
        if self.fast_ema > self.slow_ema && self.last_price < self.slow_ema {
            return Some(Signal::Buy);
        }

        // Sell signal: fast EMA crosses below slow EMA
        if self.fast_ema < self.slow_ema && self.last_price > self.slow_ema {
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
        self.update_ema(bar.close);
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
    gains: Vec<f64>,
    losses: Vec<f64>,
    last_price: f64,
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
            gains: Vec::new(),
            losses: Vec::new(),
            last_price: 0.0,
        }
    }

    fn compute_rsi(&self) -> f64 {
        if self.gains.is_empty() {
            return 50.0;
        }
        let avg_gain: f64 = self.gains.iter().sum::<f64>() / self.period as f64;
        let avg_loss: f64 = self.losses.iter().sum::<f64>() / self.period as f64;
        if avg_loss == 0.0 {
            return 100.0;
        }
        let rs = avg_gain / avg_loss;
        100.0 - (100.0 / (1.0 + rs))
    }

    fn update_rsi(&mut self, price: f64) {
        if self.last_price == 0.0 {
            self.last_price = price;
            return;
        }
        let change = price - self.last_price;
        if change > 0.0 {
            self.gains.push(change);
            self.losses.push(0.0);
        } else {
            self.gains.push(0.0);
            self.losses.push(change.abs());
        }
        if self.gains.len() > self.period {
            self.gains.remove(0);
            self.losses.remove(0);
        }
        self.last_price = price;
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
        vec![]
    }

    fn clone_box(&self) -> Box<dyn Strategy> {
        Box::new(Self {
            period: self.period,
            overbought: self.overbought,
            oversold: self.oversold,
            instrument_id: self.instrument_id.clone(),
            gains: self.gains.clone(),
            losses: self.losses.clone(),
            last_price: self.last_price,
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

        self.update_rsi(tick.price);
        let rsi = self.compute_rsi();

        if rsi < self.oversold {
            Some(Signal::Buy)
        } else if rsi > self.overbought {
            Some(Signal::Sell)
        } else {
            None
        }
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
        self.update_rsi(bar.close);
        None
    }
}
