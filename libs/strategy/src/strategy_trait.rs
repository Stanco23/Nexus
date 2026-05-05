//! Strategy trait definitions.
//!
//! Defines the core `Strategy` trait for all trading strategies.
//! Lifecycle hooks: `on_init` → `on_start` → [tick/bar loop] → `on_stop` → `on_finish`
//! `on_reset` clears strategy state for reuse in sweep/mc runs.

use crate::context::StrategyCtx;
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
