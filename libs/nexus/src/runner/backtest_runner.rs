//! BacktestRunner — executes historical replay via BacktestEngine.

use super::Runner;
use crate::backtest::BacktestEngine;
use crate::data_manager::types::DataManagerConfig;
use nexus_strategy::Strategy;

/// Executes a historical backtest run.
#[derive(Debug, Default)]
pub struct BacktestRunner;

impl BacktestRunner {
    /// Construct a new BacktestRunner.
    pub fn new() -> Self {
        Self
    }
}

impl Runner for BacktestRunner {
    fn run(
        &self,
        config: &DataManagerConfig,
        strategy_factory: Box<dyn Fn() -> Box<dyn Strategy>>,
    ) -> Result<crate::backtest::BacktestResult, String> {
        let mut engine = BacktestEngine::new()
            .with_instrument(&config.symbol, config.exchange.as_str())
            .map_err(|e| e.to_string())?;

        engine = engine
            .with_date_range(config.start_date, config.end_date)
            .map_err(|e| e.to_string())?;

        engine = engine
            .with_data_dir(config.data_root.clone())
            .map_err(|e| e.to_string())?;

        engine = engine.with_exchange_venue(config.exchange, config.venue);

        engine.run_boxed(strategy_factory).map_err(|e| e.to_string())
    }
}
