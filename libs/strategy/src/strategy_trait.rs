//! Strategy trait definitions.

use crate::types::{BacktestMode, Bar, InstrumentId, ParameterSchema, Signal, Tick};
use crate::context::StrategyCtx;

/// Core strategy trait for all trading strategies.
///
/// Strategies are `Send + Sync` so they can be shared across rayon threads
/// during parameter sweeps. The `Clone` implementation via `clone_box()` allows
/// each sweep iteration to get a fresh strategy instance.
pub trait Strategy: Send + Sync {
    fn name(&self) -> &str;

    fn mode(&self) -> BacktestMode;

    fn subscribed_instruments(&self) -> Vec<InstrumentId>;

    fn parameters(&self) -> Vec<ParameterSchema>;

    fn clone_box(&self) -> Box<dyn Strategy>;

    /// Called once before the backtest run starts.
    fn on_init(&mut self) {}

    /// Called once after the backtest run ends.
    fn on_finish(&mut self) {}

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
}

impl Clone for Box<dyn Strategy> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::examples::{EmaCrossStrategy, RsiStrategy};
    use crate::types::InstrumentId;

    #[test]
    fn test_strategy_clone_box() {
        let id = InstrumentId::new("BTCUSDT", "BINANCE");
        let s = EmaCrossStrategy::new(id, 5, 15);
        let s2 = s.clone_box();
        assert_eq!(s.name(), s2.name());
    }

    #[test]
    fn test_ema_cross_strategy_name() {
        let id = InstrumentId::new("BTCUSDT", "BINANCE");
        let s = EmaCrossStrategy::new(id, 5, 15);
        assert_eq!(s.name(), "EmaCrossStrategy");
    }

    #[test]
    fn test_rsi_strategy_name() {
        let id = InstrumentId::new("ETHUSDT", "BINANCE");
        let s = RsiStrategy::new(id, 14, 70.0, 30.0);
        assert_eq!(s.name(), "RsiStrategy");
    }

    #[test]
    fn test_backtest_mode() {
        let id = InstrumentId::new("BTCUSDT", "BINANCE");
        let ema = EmaCrossStrategy::new(id.clone(), 5, 15);
        let rsi = RsiStrategy::new(id, 14, 70.0, 30.0);
        assert_eq!(ema.mode(), BacktestMode::Tick);
        assert_eq!(rsi.mode(), BacktestMode::Tick);
    }
}
