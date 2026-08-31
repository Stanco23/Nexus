//! RSI Overbought/Oversold Strategy.
//!
//! Buy when RSI drops below oversold threshold, sell when RSI rises above overbought threshold.

use crate::indicators::Indicator;
use crate::context::StrategyCtx;
use crate::indicators::Rsi;
use crate::types::{BacktestMode, Bar, InstrumentId, ParameterSchema, ParameterType, ParameterValue, Signal, Tick};
use crate::Strategy;

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
