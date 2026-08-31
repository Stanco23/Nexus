//! Strategy trait definitions.
//!
//! Defines the core `Strategy` trait for all trading strategies.
//! Lifecycle hooks: `on_init` → `on_start` → [tick/bar loop] → `on_stop` → `on_finish`
//! `on_reset` clears strategy state for reuse in sweep/mc runs.

use std::sync::Arc;
use std::time::Duration;

use crate::context::StrategyCtx;
use crate::signals::SignalBus;
use crate::types::{BacktestMode, Bar, InstrumentId, ParameterSchema, Signal, Tick};

/// Core strategy trait for all trading strategies.
///
/// Strategies are `Send + Sync` so they can be shared across rayon threads
/// during parameter sweeps. The `Clone` implementation via `clone_box()` allows
/// each sweep iteration to get a fresh strategy instance.
///
/// # Lifecycle
/// ```text
/// on_init()     → called once before backtest run starts (parameter validation/setup)
/// on_start()    → called once per instrument when first data arrives
/// on_trade()    → called on each tick (tick-mode backtest)
/// on_bar()      → called on each bar (bar-mode backtest)
/// on_signal()   → called when a subscribed named signal fires
/// on_stop()     → called once per instrument when data ends
/// on_finish()   → called once after backtest run ends (cleanup/reporting)
/// on_reset()    → called to reset strategy state for reuse (sweeps, walk-forward)
/// ```
pub trait Strategy: Send + Sync {
    fn name(&self) -> &str;

    fn mode(&self) -> BacktestMode;

    fn subscribed_instruments(&self) -> Vec<InstrumentId>;

    fn parameters(&self) -> Vec<ParameterSchema>;

    fn clone_box(&self) -> Box<dyn Strategy>;

    /// Returns the bar timeframe for this strategy.
    ///
    /// - `None` = tick mode (`on_trade` called per tick)
    /// - `Some(Duration)` = bar mode (`on_bar` called when bar period completes)
    ///
    /// The backtest engine creates a `BarAggregator` with the configured period
    /// and routes ticks through it, calling `on_bar` when each bar closes.
    /// For bar-mode strategies, `on_trade` is NOT called.
    fn timeframe(&self) -> Option<Duration> { None }

    /// Subscribe to a named signal bus.
    /// Strategies call this to register interest in specific signal events.
    fn subscribe_signal(&mut self, _signal_bus: Arc<SignalBus>) {}

    /// Returns the position size (number of contracts/units) for this strategy.
    /// Default: 1.0 (one unit).
    fn position_size(&self) -> f64 { 1.0 }

    /// Called once before the backtest run starts.
    /// Use for parameter validation and one-time setup.
    fn on_init(&mut self) {}

    /// Called once when the first market data (tick or bar) arrives for this strategy.
    /// Use for per-instrument initialization (e.g., setting entry price, warming up indicators).
    /// Default implementation: no-op.
    fn on_start(&mut self) {}

    /// Called once when market data ends for this strategy (end of file or data gap).
    /// Use for finalizing indicator values, closing positions, logging.
    /// Default implementation: no-op.
    fn on_stop(&mut self) {}

    /// Called once after the backtest run ends.
    /// Use for cleanup, final reporting, writing results.
    fn on_finish(&mut self) {}

    /// Reset strategy state so it can be reused in a new run.
    /// Resets all indicators, positions, signals, and internal state to initial values.
    /// Called automatically before each sweep iteration and walk-forward window.
    /// Default implementation: no-op (strategies without runtime state don't need it).
    fn on_reset(&mut self) {}

    /// Called on each tick (tick-mode backtest).
    fn on_trade(
        &mut self,
        instrument_id: InstrumentId,
        tick: &Tick,
        ctx: &mut dyn StrategyCtx,
    ) -> Option<Signal>;

    /// Called on each bar (bar-mode backtest).
    fn on_bar(
        &mut self,
        instrument_id: InstrumentId,
        bar: &Bar,
        ctx: &mut dyn StrategyCtx,
    ) -> Option<Signal>;

    /// Called when a subscribed named signal fires.
    ///
    /// Default implementation: no-op. Strategies that subscribe to signals
    /// (e.g., via `StrategyCtx::subscribe_signal`) receive them here.
    fn on_signal(&mut self, _name: &str, _value: f64, _timestamp_ns: u64) {}
}

impl Clone for Box<dyn Strategy> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// Blanket impl so `Box<dyn Strategy>` is usable as the concrete `S` in
/// `BacktestEngine::run`. This bridges the gap when `strategy_factory: impl Fn() -> Box<dyn Strategy>`
/// is used as the argument — the compiler needs `Box<dyn Strategy>: Strategy`.
impl Strategy for Box<dyn Strategy> {
    fn name(&self) -> &str { (**self).name() }
    fn mode(&self) -> BacktestMode { (**self).mode() }
    fn subscribed_instruments(&self) -> Vec<InstrumentId> { (**self).subscribed_instruments() }
    fn parameters(&self) -> Vec<ParameterSchema> { (**self).parameters() }
    fn clone_box(&self) -> Box<dyn Strategy> { (**self).clone_box() }
    fn timeframe(&self) -> Option<Duration> { (**self).timeframe() }
    fn subscribe_signal(&mut self, sb: Arc<SignalBus>) { (**self).subscribe_signal(sb) }
    fn position_size(&self) -> f64 { (**self).position_size() }
    fn on_init(&mut self) { (**self).on_init() }
    fn on_start(&mut self) { (**self).on_start() }
    fn on_stop(&mut self) { (**self).on_stop() }
    fn on_finish(&mut self) { (**self).on_finish() }
    fn on_reset(&mut self) { (**self).on_reset() }
    fn on_trade(&mut self, i: InstrumentId, t: &Tick, c: &mut dyn StrategyCtx) -> Option<Signal> {
        (**self).on_trade(i, t, c)
    }
    fn on_bar(&mut self, i: InstrumentId, b: &Bar, c: &mut dyn StrategyCtx) -> Option<Signal> {
        (**self).on_bar(i, b, c)
    }
    fn on_signal(&mut self, n: &str, v: f64, ts: u64) { (**self).on_signal(n, v, ts) }
}
