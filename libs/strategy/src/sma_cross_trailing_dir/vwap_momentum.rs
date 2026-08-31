//! VWAP Momentum Strategy.
//!
//! Tick-mode strategy that enters when price deviates from VWAP beyond a threshold,
//! filtered by RSI momentum. Exits via trailing stop managed by the execution layer.

use crate::context::StrategyCtx;
use crate::indicators::{Indicator, Rsi};
use crate::types::{BacktestMode, Bar, InstrumentId, ParameterSchema, ParameterType, ParameterValue, Signal, Tick};
use crate::Strategy;

/// VWAP Momentum Strategy.
///
/// ## Entry Logic
/// - **Buy**: VWAP deviation > `deviation_threshold`% AND RSI < 70 (not overbought)
/// - **Sell**: VWAP deviation < `-deviation_threshold`% AND RSI > 30 (not oversold)
///
/// ## Tick Mode
/// `timeframe()` returns `None`, so `on_trade` is called on every tick.
/// `on_bar` is a no-op stub.
pub struct VwapMomentumStrategy {
    pub name: String,
    pub instruments: Vec<InstrumentId>,
    pub deviation_threshold: f64,
    pub rsi_period: usize,
    pub position_size: f64,
    pub trailing_delta_pct: f64,
    // ─── Runtime state ────────────────────────────────────────────────────────
    cum_volume: f64,
    cum_price_volume: f64,
    last_signal: Option<Signal>,
    rsi: Rsi,
}

impl VwapMomentumStrategy {
    pub fn new(
        instruments: Vec<InstrumentId>,
        deviation_threshold: f64,
        rsi_period: usize,
        position_size: f64,
        trailing_delta_pct: f64,
    ) -> Self {
        Self {
            name: "vwap_momentum".into(),
            instruments,
            deviation_threshold,
            rsi_period,
            position_size,
            trailing_delta_pct,
            cum_volume: 0.0,
            cum_price_volume: 0.0,
            last_signal: None,
            rsi: Rsi::new(rsi_period),
        }
    }

    /// Convenience constructor using a single instrument (default BTCUSDT/BINANCE).
    #[inline]
    pub fn with_instrument(instrument: InstrumentId) -> Self {
        Self::new(vec![instrument], 0.5, 14, 1.0, 0.2)
    }
}

impl Strategy for VwapMomentumStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn mode(&self) -> BacktestMode {
        BacktestMode::Tick
    }

    fn timeframe(&self) -> Option<std::time::Duration> {
        // None = tick mode: on_trade called per tick
        None
    }

    fn subscribed_instruments(&self) -> Vec<InstrumentId> {
        self.instruments.clone()
    }

    fn parameters(&self) -> Vec<ParameterSchema> {
        vec![
            ParameterSchema {
                name: "deviation_threshold".into(),
                param_type: ParameterType::Float,
                default: ParameterValue::Float(0.5),
                bounds: Some((0.01, 10.0)),
                description: "VWAP deviation (%) that triggers entry".into(),
            },
            ParameterSchema {
                name: "rsi_period".into(),
                param_type: ParameterType::Int,
                default: ParameterValue::Int(14),
                bounds: Some((2.0, 100.0)),
                description: "RSI averaging period".into(),
            },
            ParameterSchema {
                name: "position_size".into(),
                param_type: ParameterType::Float,
                default: ParameterValue::Float(1.0),
                bounds: Some((0.01, 1000.0)),
                description: "Number of contracts/units per trade".into(),
            },
            ParameterSchema {
                name: "trailing_delta_pct".into(),
                param_type: ParameterType::Float,
                default: ParameterValue::Float(0.2),
                bounds: Some((0.01, 10.0)),
                description: "Trailing stop delta as % of entry price".into(),
            },
        ]
    }

    fn position_size(&self) -> f64 {
        self.position_size
    }

    fn clone_box(&self) -> Box<dyn Strategy> {
        Box::new(Self {
            name: self.name.clone(),
            instruments: self.instruments.clone(),
            deviation_threshold: self.deviation_threshold,
            rsi_period: self.rsi_period,
            position_size: self.position_size,
            trailing_delta_pct: self.trailing_delta_pct,
            cum_volume: self.cum_volume,
            cum_price_volume: self.cum_price_volume,
            last_signal: self.last_signal.clone(),
            rsi: self.rsi.clone(),
        })
    }

    fn on_reset(&mut self) {
        self.cum_volume = 0.0;
        self.cum_price_volume = 0.0;
        self.last_signal = None;
        self.rsi.reset();
    }

    /// Called on every tick (tick-mode backtest).
    fn on_trade(
        &mut self,
        instrument_id: InstrumentId,
        tick: &Tick,
        _ctx: &mut dyn StrategyCtx,
    ) -> Option<Signal> {
        if !self.instruments.contains(&instrument_id) {
            return None;
        }

        let price = tick.price;
        let size = tick.size;

        // ── VWAP update ────────────────────────────────────────────────────────
        self.cum_price_volume += price * size;
        self.cum_volume += size;

        if self.cum_volume == 0.0 {
            return None;
        }
        let vwap = self.cum_price_volume / self.cum_volume;
        let deviation_pct = (price - vwap) / vwap * 100.0;

        // ── RSI update ──────────────────────────────────────────────────────────
        let rsi_value = self.rsi.update(price);
        let Some(rsi) = rsi_value else {
            return None;
        };

        // ── Signal logic ─────────────────────────────────────────────────────────
        // Buy:  deviation >  threshold AND RSI < 70 (not overbought)
        // Sell: deviation < -threshold AND RSI > 30 (not oversold)
        if deviation_pct > self.deviation_threshold && rsi < 70.0 {
            self.last_signal = Some(Signal::Buy);
        } else if deviation_pct < -self.deviation_threshold && rsi > 30.0 {
            self.last_signal = Some(Signal::Sell);
        } else {
            self.last_signal = Some(Signal::Close);
        }

        self.last_signal
    }

    /// No-op in tick mode — `on_trade` is used instead.
    fn on_bar(
        &mut self,
        _instrument_id: InstrumentId,
        _bar: &Bar,
        _ctx: &mut dyn StrategyCtx,
    ) -> Option<Signal> {
        None
    }
}

impl Clone for VwapMomentumStrategy {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            instruments: self.instruments.clone(),
            deviation_threshold: self.deviation_threshold,
            rsi_period: self.rsi_period,
            position_size: self.position_size,
            trailing_delta_pct: self.trailing_delta_pct,
            cum_volume: self.cum_volume,
            cum_price_volume: self.cum_price_volume,
            last_signal: self.last_signal.clone(),
            rsi: self.rsi.clone(),
        }
    }
}
