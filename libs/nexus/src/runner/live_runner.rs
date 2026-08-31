//! LiveRunner — placeholder for real-time / paper trading (Phase 6).

use super::Runner;
use crate::backtest::BacktestResult;
use crate::data_manager::types::DataManagerConfig;
use nexus_strategy::Strategy;

/// Executes live or paper trading runs.
///
/// Currently a stub — Phase 6 will wire in the Trader + DataEngine + RiskEngine
/// pipeline for real-time execution.
#[derive(Debug, Default)]
pub struct LiveRunner;

impl LiveRunner {
    /// Construct a new LiveRunner.
    pub fn new() -> Self {
        Self
    }
}

impl Runner for LiveRunner {
    fn run(
        &self,
        _config: &DataManagerConfig,
        _strategy_factory: Box<dyn Fn() -> Box<dyn Strategy>>,
    ) -> Result<BacktestResult, String> {
        Err("LiveRunner not yet implemented — Phase 6".to_string())
    }
}
