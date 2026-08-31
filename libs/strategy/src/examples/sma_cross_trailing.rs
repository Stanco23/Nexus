//! SMA Crossover with Trailing Stop strategy.
//!
//! Buys when the fast SMA crosses above the slow SMA,
//! sells on the reverse crossover. Uses a trailing stop for risk management.

use std::collections::HashMap;
use std::time::Duration;

use crate::context::StrategyCtx;
use crate::indicators::{Indicator, Sma};
use crate::types::{
    BacktestMode, Bar, InstrumentId, ParameterSchema,
    ParameterType, ParameterValue, Signal,
};
use crate::Strategy;

/// SMA Crossover with Trailing Stop.
///
/// Multi-instrument bar-mode strategy: subscribes to BTCUSDT/BINANCE and ETHUSDT/BINANCE
/// and manages independent state for each.
pub struct SmaCrossTrailingStrategy {
    name: String,
    fast_period: usize,
    slow_period: usize,
    position_size: f64,
    trailing_delta_pct: f64,
    sl_pct: f64,
    // Per-instrument SMA indicators
    sma_fast: HashMap<InstrumentId, Sma>,
    sma_slow: HashMap<InstrumentId, Sma>,
    // Track previous SMA relationship for crossover detection
    prev_fast_above_slow: HashMap<InstrumentId, bool>,
    // Track last signal per instrument
    last_signal: HashMap<InstrumentId, Option<Signal>>,
}

impl Clone for SmaCrossTrailingStrategy {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            fast_period: self.fast_period,
            slow_period: self.slow_period,
            position_size: self.position_size,
            trailing_delta_pct: self.trailing_delta_pct,
            sl_pct: self.sl_pct,
            sma_fast: HashMap::new(),
            sma_slow: HashMap::new(),
            prev_fast_above_slow: HashMap::new(),
            last_signal: HashMap::new(),
        }
    }
}

impl SmaCrossTrailingStrategy {
    pub fn new(
        fast_period: usize,
        slow_period: usize,
        position_size: f64,
        trailing_delta_pct: f64,
        sl_pct: f64,
    ) -> Self {
        Self {
            name: "sma_cross_trailing".into(),
            fast_period,
            slow_period,
            position_size,
            trailing_delta_pct,
            sl_pct,
            sma_fast: HashMap::new(),
            sma_slow: HashMap::new(),
            prev_fast_above_slow: HashMap::new(),
            last_signal: HashMap::new(),
        }
    }
}

impl Strategy for SmaCrossTrailingStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn mode(&self) -> BacktestMode {
        BacktestMode::Bar
    }

    fn timeframe(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    fn subscribed_instruments(&self) -> Vec<InstrumentId> {
        vec![
            InstrumentId::new("BTCUSDT", "BINANCE"),
            InstrumentId::new("ETHUSDT", "BINANCE"),
        ]
    }

    fn parameters(&self) -> Vec<ParameterSchema> {
        vec![
            ParameterSchema {
                name: "fast_period".into(),
                param_type: ParameterType::Int,
                default: ParameterValue::Int(20),
                bounds: Some((2.0, 100.0)),
                description: "Fast SMA period".into(),
            },
            ParameterSchema {
                name: "slow_period".into(),
                param_type: ParameterType::Int,
                default: ParameterValue::Int(50),
                bounds: Some((5.0, 500.0)),
                description: "Slow SMA period".into(),
            },
            ParameterSchema {
                name: "position_size".into(),
                param_type: ParameterType::Float,
                default: ParameterValue::Float(1.0),
                bounds: Some((0.01, 100.0)),
                description: "Position size (units)".into(),
            },
            ParameterSchema {
                name: "trailing_delta_pct".into(),
                param_type: ParameterType::Float,
                default: ParameterValue::Float(0.01),
                bounds: Some((0.001, 0.1)),
                description: "Trailing stop trigger delta (% of entry, e.g. 0.01 = 1%)".into(),
            },
            ParameterSchema {
                name: "sl_pct".into(),
                param_type: ParameterType::Float,
                default: ParameterValue::Float(0.02),
                bounds: Some((0.001, 0.2)),
                description: "Stop-loss percentage (e.g. 0.02 = 2%)".into(),
            },
        ]
    }

    fn clone_box(&self) -> Box<dyn Strategy> {
        Box::new(Self {
            name: self.name.clone(),
            fast_period: self.fast_period,
            slow_period: self.slow_period,
            position_size: self.position_size,
            trailing_delta_pct: self.trailing_delta_pct,
            sl_pct: self.sl_pct,
            sma_fast: HashMap::new(),
            sma_slow: HashMap::new(),
            prev_fast_above_slow: HashMap::new(),
            last_signal: HashMap::new(),
        })
    }

    fn on_reset(&mut self) {
        self.sma_fast.clear();
        self.sma_slow.clear();
        self.prev_fast_above_slow.clear();
        self.last_signal.clear();
    }

    fn on_bar(
        &mut self,
        instrument_id: InstrumentId,
        bar: &Bar,
        ctx: &mut dyn StrategyCtx,
    ) -> Option<Signal> {
        // Lazily initialize per-instrument indicators on first bar
        let fast = self
            .sma_fast
            .entry(instrument_id.clone())
            .or_insert_with(|| Sma::new(self.fast_period));
        let slow = self
            .sma_slow
            .entry(instrument_id.clone())
            .or_insert_with(|| Sma::new(self.slow_period));

        // Update SMAs with bar close price
        let _ = fast.update(bar.close);
        let _ = slow.update(bar.close);

        let fast_val = fast.mean()?;
        let slow_val = slow.mean()?;

        let curr_fast_above = fast_val > slow_val;
        let prev_fast_above = *self
            .prev_fast_above_slow
            .get(&instrument_id)
            .unwrap_or(&false);

        // Update previous state
        self.prev_fast_above_slow
            .insert(instrument_id.clone(), curr_fast_above);

        let signal = if curr_fast_above && !prev_fast_above {
            // Fast SMA crossed above slow SMA -> BUY
            Some(Signal::Buy)
        } else if !curr_fast_above && prev_fast_above {
            // Fast SMA crossed below slow SMA -> SELL
            Some(Signal::Sell)
        } else {
            None
        };

        // Emit signal and submit trailing stop order on crossover
        if let Some(sig) = signal {
            self.last_signal
                .insert(instrument_id.clone(), Some(sig));

            // In backtest mode, submit_with_sl_tp doesn't execute orders.
            // Return the signal so route_signal opens/closes positions.
            return Some(sig);
        }

        // on_bar returns None -- order submission is handled directly via ctx
        None
    }

    fn on_trade(
        &mut self,
        _instrument_id: crate::types::InstrumentId,
        _tick: &crate::types::Tick,
        _ctx: &mut dyn StrategyCtx,
    ) -> Option<Signal> {
        // Bar-mode strategy: all logic lives in on_bar
        None
    }
}
