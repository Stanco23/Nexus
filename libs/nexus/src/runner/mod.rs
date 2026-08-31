//! Runner — dispatches to BacktestRunner or LiveRunner based on config mode.

pub mod backtest_runner;
pub mod live_runner;

use crate::backtest::BacktestResult;
use crate::data_manager::types::DataManagerConfig;
use nexus_strategy::Strategy;

/// Supported run modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BacktestMode {
    /// Historical replay via BacktestRunner.
    #[default]
    Backtest,
    /// Real-time live execution via LiveRunner.
    Live,
    /// Simulated execution with real-time data via LiveRunner.
    Paper,
}

impl std::fmt::Display for BacktestMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BacktestMode::Backtest => write!(f, "backtest"),
            BacktestMode::Live => write!(f, "live"),
            BacktestMode::Paper => write!(f, "paper"),
        }
    }
}

impl std::str::FromStr for BacktestMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "backtest" => Ok(BacktestMode::Backtest),
            "live" => Ok(BacktestMode::Live),
            "paper" => Ok(BacktestMode::Paper),
            other => Err(format!("unknown mode: {}", other)),
        }
    }
}

/// Runner trait — implemented by BacktestRunner and LiveRunner.
pub trait Runner {
    /// Execute the run and return a BacktestResult or error string.
    fn run(
        &self,
        config: &DataManagerConfig,
        strategy_factory: Box<dyn Fn() -> Box<dyn Strategy>>,
    ) -> Result<BacktestResult, String>;
}
