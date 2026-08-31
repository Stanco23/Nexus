//! Parameter sweep runner — parallel grid/random search across parameter space.
//!
//! # Architecture
//! - `SweepRunner` drives Grid or Random mode sweeps
//! - `Arc<TickBufferSet>` shared zero-copy across rayon workers
//! - Each worker clones strategy via `clone_box()` (Send + Sync)
//!
//! # Exit Criteria
//! - Grid mode: enumerate all param combinations from `param_ranges`
//! - Random mode: sample N configs with seed
//! - `max_combos` limit validated (error if exceeded)

use crate::buffer::buffer_set::TickBufferSet;
use crate::portfolio::{Portfolio, PortfolioConfig};
use crate::engine::CommissionConfig;
use nexus_strategy::{Strategy, StrategyCtx};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

// =============================================================================
// Config & Types
// =============================================================================

/// One parameter's discrete grid values for enumeration.
#[derive(Debug, Clone)]
pub struct ParamRange {
    pub name: String,
    /// Discrete values to sweep over (e.g., [1.0, 2.0, 3.0]).
    pub values: Vec<f64>,
}

impl ParamRange {
    pub fn new(name: impl Into<String>, values: Vec<f64>) -> Self {
        Self { name: name.into(), values }
    }

    /// Linear spacing from start to end inclusive.
    pub fn linspace(name: impl Into<String>, start: f64, end: f64, count: usize) -> Self {
        let step = if count <= 1 { 0.0 } else { (end - start) / (count - 1) as f64 };
        let values = (0..count).map(|i| start + step * i as f64).collect();
        Self { name: name.into(), values }
    }
}

/// Sweep execution mode.
#[derive(Debug, Clone)]
pub enum SweepMode {
    /// Enumerate every combination from param_ranges.
    Grid,
    /// Random sample of N configs with a given seed.
    Random { seed: u64, n: usize },
}

/// Which metric to rank/optimize by.
#[derive(Debug, Clone, Copy)]
pub enum Metric {
    /// Maximize Sharpe ratio.
    Sharpe,
    /// Maximize total PnL.
    PnL,
    /// Minimize max drawdown (smallest = best).
    MaxDD,
}

/// Configuration for a parameter sweep.
#[derive(Debug, Clone)]
pub struct SweepConfig {
    pub mode: SweepMode,
    /// Parameter ranges — each `ParamRange` carries its own discrete values.
    pub param_ranges: Vec<ParamRange>,
    pub metric: Metric,
    /// Hard cap on combos. Exceeding this is a fatal error.
    pub max_combos: Option<usize>,
}

impl SweepConfig {
    /// Validate max_combos against total grid size.
    /// Returns `Ok(total)` if within limit, `Err(msg)` if exceeded.
    pub fn validate(&self) -> Result<usize, String> {
        let total = self.total_combos();
        if let Some(max) = self.max_combos {
            if total > max {
                return Err(format!(
                    "SweepConfig: {} combos exceeds max_combos={}. \
                     Increase max_combos or reduce param ranges.",
                    total, max
                ));
            }
        }
        Ok(total)
    }

    /// Total combos (grid size), without random truncation.
    pub fn total_combos(&self) -> usize {
        match &self.mode {
            SweepMode::Grid => {
                if self.param_ranges.is_empty() {
                    return 0;
                }
                self.param_ranges.iter().map(|p| p.values.len()).product()
            }
            SweepMode::Random { n, .. } => *n,
        }
    }

    fn grid_keys(&self) -> Vec<&str> {
        self.param_ranges.iter().map(|p| p.name.as_str()).collect()
    }
}

/// Per-run metrics for one param combination.
#[derive(Debug)]
pub struct RunMetrics {
    pub params: HashMap<String, f64>,
    pub sharpe: f64,
    pub pnl: f64,
    pub max_dd: f64,
    pub num_trades: u32,
    pub duration_ms: u64,
}

/// Final sweep result — best params + all individual runs.
#[derive(Debug)]
pub struct SweepResult {
    pub best_params: HashMap<String, f64>,
    pub best_sharpe: f64,
    pub all_runs: Vec<RunMetrics>,
}

// =============================================================================
// SweepRunner
// =============================================================================

/// Parameter sweep runner.
///
/// Shares `Arc<TickBufferSet>` across rayon workers (zero-copy).
/// Each worker gets a fresh strategy via `strategy_factory`.
pub struct SweepRunner {
    buffer_set: Arc<TickBufferSet>,
    portfolio_config: PortfolioConfig,
}

impl SweepRunner {
    /// Create a new SweepRunner with a shared tick-buffer set and initial equity.
    pub fn new(buffer_set: Arc<TickBufferSet>, initial_equity: f64) -> Self {
        let commission = CommissionConfig::new(0.001); // 0.1% taker commission
        let config = PortfolioConfig::new(initial_equity, commission);
        Self { buffer_set, portfolio_config: config }
    }

    /// Run the sweep with the given configuration.
    ///
    /// `strategy_factory` is called once per combo — must return a `Send + Sync`
    /// strategy (all `Strategy` implementors are automatically `Send + Sync`).
    ///
    /// # Panics
    /// Panics if `config.validate()` fails (max_combos exceeded).
    pub fn run<S>(
        &self,
        config: &SweepConfig,
        strategy_factory: impl Fn(HashMap<String, f64>) -> S + Send + Sync + 'static,
    ) -> SweepResult
    where
        S: Strategy + Clone,
    {
        config.validate().expect(
            "SweepConfig validation failed: max_combos exceeded. \
             Increase max_combos or reduce param ranges.",
        );

        let combos = match &config.mode {
            SweepMode::Grid => Self::enumerate_grid(config),
            SweepMode::Random { seed, n } => Self::sample_random(config, *seed, *n),
        };

        let initial_equity = self.portfolio_config.initial_equity_per_instrument;
        let commission = self.portfolio_config.commission.clone();

        let results: Vec<RunMetrics> = combos
            .par_iter()
            .map(|params| {
                let start = std::time::Instant::now();

                // Strategy is Clone + Send + Sync — rayon can clone into each worker
                let mut strategy: S = strategy_factory(params.clone());

                let mut portfolio = Portfolio::new(initial_equity);
                for id in self.buffer_set.instrument_ids() {
                    portfolio.register_instrument(id.clone());
                }

                run_sweep_tick_loop(
                    Arc::clone(&self.buffer_set),
                    &mut portfolio,
                    &mut strategy,
                    &commission,
                    initial_equity,
                );

                let num_instruments = portfolio.num_instruments() as f64;
                let initial_total = initial_equity * num_instruments;
                let final_equity = portfolio.portfolio_equity();
                let pnl = final_equity - initial_total;
                let max_dd = portfolio.portfolio_max_drawdown();
                let num_trades = portfolio.total_trades();
                let returns = portfolio.returns();
                let sharpe = compute_sharpe(&returns);
                let duration_ms = start.elapsed().as_millis() as u64;

                RunMetrics { params: params.clone(), sharpe, pnl, max_dd, num_trades, duration_ms }
            })
            .collect();

        let best = results
            .iter()
            .max_by(|a, b| Self::cmp_metric(a, b, config.metric))
            .expect("no combos ran");

        SweepResult {
            best_params: best.params.clone(),
            best_sharpe: best.sharpe,
            all_runs: results,
        }
    }

    /// Enumerate all combos as a Cartesian product of param value vectors.
    fn enumerate_grid(config: &SweepConfig) -> Vec<HashMap<String, f64>> {
        let keys: Vec<&str> = config.grid_keys();
        let values: Vec<&[f64]> = config.param_ranges.iter().map(|p| p.values.as_slice()).collect();

        if keys.is_empty() {
            return vec![];
        }

        let n = keys.len();
        let mut indices = vec![0usize; n];
        let mut combos = Vec::new();

        loop {
            let mut combo = HashMap::new();
            for (i, key) in keys.iter().enumerate() {
                combo.insert(key.to_string(), values[i][indices[i]]);
            }
            combos.push(combo);

            let mut pos = 0;
            while pos < n {
                indices[pos] += 1;
                if indices[pos] < values[pos].len() {
                    break;
                }
                indices[pos] = 0;
                pos += 1;
            }
            if pos == n {
                break;
            }
        }

        combos
    }

    /// Sample N combos randomly with a given seed.
    fn sample_random(config: &SweepConfig, seed: u64, n: usize) -> Vec<HashMap<String, f64>> {
        use rand::seq::IteratorRandom;
        use rand::rngs::SmallRng;
        use rand::SeedableRng;

        if config.param_ranges.is_empty() {
            return vec![];
        }

        let full_grid = Self::enumerate_grid(config);

        if full_grid.len() <= n {
            return full_grid;
        }

        let mut rng = SmallRng::seed_from_u64(seed);
        full_grid
            .iter()
            .choose_multiple(&mut rng, n)
            .into_iter()
            .cloned()
            .collect()
    }

    fn cmp_metric(a: &RunMetrics, b: &RunMetrics, metric: Metric) -> std::cmp::Ordering {
        let score = |r: &RunMetrics| match metric {
            Metric::Sharpe => r.sharpe,
            Metric::PnL => r.pnl,
            Metric::MaxDD => -r.max_dd,
        };
        score(a).partial_cmp(&score(b)).unwrap_or(std::cmp::Ordering::Equal)
    }
}

// =============================================================================
// Sweep tick loop
// =============================================================================

/// Run a backtest tick loop for one strategy instance over a TickBufferSet.
/// Uses `MergeCursor` for time-ordered delivery; each worker gets its own `Portfolio`.
fn run_sweep_tick_loop<S: Strategy>(
    buffer_set: Arc<TickBufferSet>,
    portfolio: &mut Portfolio,
    strategy: &mut S,
    commission: &CommissionConfig,
    initial_equity: f64,
) {
    use crate::engine::Signal as EngineSignal;

    let mut cursor = buffer_set.merge_cursor();

    // Pre-build EngineContext per instrument
    let signal_buses: std::collections::HashMap<u32, _> = buffer_set
        .instrument_ids()
        .iter()
        .map(|id| {
            let sb = std::sync::Arc::new(std::sync::Mutex::new(crate::signals::SignalBus::new()));
            let mut ctx = crate::engine::core::EngineContext::new(initial_equity, sb, std::ptr::null_mut());
            ctx.subscribe_instruments(vec![id.clone()]);
            (id.id, ctx)
        })
        .collect();

    let mut last_prices: std::collections::HashMap<u32, f64> = std::collections::HashMap::new();
    let mut strategy_started = false;

    while let Some(event) = cursor.advance() {
        let instrument_id = &event.instrument_id;
        let tick = event.tick;
        let price = tick.price_int as f64 / 1e9;
        let size = tick.size_int as f64 / 1e6;

        last_prices.insert(instrument_id.id, price);

        if let Some(state) = portfolio.state_mut(instrument_id) {
            state.update_unrealized_pnl(price);
        }
        portfolio.record_equity();

        // Get per-instrument context
        let ctx = match signal_buses.get(&instrument_id.id) {
            Some(ctx) => ctx,
            None => continue,
        };

        // Lazy start
        if !strategy_started {
            strategy.on_start();
            strategy_started = true;
        }

        let ntick = nexus_types::Tick {
            timestamp_ns: tick.timestamp_ns,
            price,
            size,
            vpin: 0.0,
        };

        if let Some(signal) = strategy.on_trade(instrument_id.clone(), &ntick, ctx) {
            let engine_signal = match signal {
                nexus_types::Signal::Buy => EngineSignal::Buy,
                nexus_types::Signal::Sell => EngineSignal::Sell,
                nexus_types::Signal::Close => EngineSignal::Close,
            };

            let position = portfolio.state(instrument_id).map(|s| s.position).unwrap_or(0.0);
            let has_position = position != 0.0;
            let is_long = position > 0.0;

            match (engine_signal, has_position, is_long) {
                (EngineSignal::Buy, false, _) | (EngineSignal::Buy, true, false) => {
                    if let Some(state) = portfolio.state_mut(instrument_id) {
                        let sz = if has_position { position.abs() + 1.0 } else { 1.0 };
                        let comm = commission.compute(price, sz.abs());
                        if state.position == 0.0 {
                            state.position = sz;
                            state.entry_price = price;
                        } else {
                            let pnl = if state.position > 0.0 {
                                (price - state.entry_price) * state.position.abs()
                            } else {
                                (state.entry_price - price) * state.position.abs()
                            };
                            state.realized_pnl += pnl;
                            state.position = sz;
                            state.entry_price = price;
                        }
                        state.equity -= comm;
                        state.commissions += comm;
                        state.num_trades += 1;
                    }
                }
                (EngineSignal::Sell, false, _) | (EngineSignal::Sell, true, true) => {
                    if let Some(state) = portfolio.state_mut(instrument_id) {
                        let sz = if has_position { position.abs() + 1.0 } else { 1.0 };
                        let comm = commission.compute(price, sz.abs());
                        if state.position == 0.0 {
                            state.position = -sz;
                            state.entry_price = price;
                        } else {
                            let pnl = if state.position > 0.0 {
                                (price - state.entry_price) * state.position.abs()
                            } else {
                                (state.entry_price - price) * state.position.abs()
                            };
                            state.realized_pnl += pnl;
                            state.position = -sz;
                            state.entry_price = price;
                        }
                        state.equity -= comm;
                        state.commissions += comm;
                        state.num_trades += 1;
                    }
                }
                (EngineSignal::Close, true, _) => {
                    if let Some(state) = portfolio.state_mut(instrument_id) {
                        let pnl = if state.position > 0.0 {
                            (price - state.entry_price) * state.position.abs()
                        } else {
                            (state.entry_price - price) * state.position.abs()
                        };
                        let comm = commission.compute(price, state.position.abs());
                        state.realized_pnl += pnl;
                        state.equity += pnl - comm;
                        state.commissions += comm;
                        state.position = 0.0;
                        state.entry_price = 0.0;
                        state.num_trades += 1;
                    }
                }
                _ => {}
            }
        }

        if let Some(state) = portfolio.state_mut(instrument_id) {
            if let Some(p) = last_prices.get(&instrument_id.id) {
                state.update_peak(*p);
            }
        }
    }

    if strategy_started {
        strategy.on_stop();
    }
}

// =============================================================================
// Utilities
// =============================================================================

/// Annualized Sharpe ratio from a return series.
fn compute_sharpe(returns: &[f64]) -> f64 {
    if returns.len() < 2 {
        return 0.0;
    }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
    let std_dev = variance.sqrt();
    if std_dev == 0.0 {
        return 0.0;
    }
    mean / std_dev * (252.0_f64).sqrt()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_param_range_new() {
        let pr = ParamRange::new("fast_ma", vec![5.0, 10.0, 15.0]);
        assert_eq!(pr.name, "fast_ma");
        assert_eq!(pr.values.len(), 3);
    }

    #[test]
    fn test_param_range_linspace() {
        let pr = ParamRange::linspace("thresh", 1.0, 3.0, 3);
        assert_eq!(pr.values, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn test_sweep_config_validate_ok() {
        let config = SweepConfig {
            mode: SweepMode::Grid,
            param_ranges: vec![
                ParamRange::new("a", vec![1.0, 2.0]),
                ParamRange::new("b", vec![10.0, 20.0]),
            ],
            metric: Metric::Sharpe,
            max_combos: Some(10),
        };
        assert_eq!(config.validate().unwrap(), 4); // 2×2 grid
    }

    #[test]
    fn test_sweep_config_validate_exceeded() {
        let config = SweepConfig {
            mode: SweepMode::Grid,
            param_ranges: vec![
                ParamRange::new("a", vec![1.0, 2.0, 3.0]),
                ParamRange::new("b", vec![10.0, 20.0, 30.0]),
            ],
            metric: Metric::PnL,
            max_combos: Some(5),
        };
        let err = config.validate().unwrap_err();
        assert!(err.contains("exceeds max_combos"));
    }

    #[test]
    fn test_random_mode_validate() {
        let config = SweepConfig {
            mode: SweepMode::Random { seed: 42, n: 3 },
            param_ranges: vec![ParamRange::new("a", vec![1.0, 2.0, 3.0, 4.0])],
            metric: Metric::MaxDD,
            max_combos: None,
        };
        assert_eq!(config.validate().unwrap(), 3);
    }

    #[test]
    fn test_enumerate_grid_3x3() {
        let config = SweepConfig {
            mode: SweepMode::Grid,
            param_ranges: vec![
                ParamRange::new("p1", vec![1.0, 2.0, 3.0]),
                ParamRange::new("p2", vec![10.0, 20.0, 30.0]),
            ],
            metric: Metric::Sharpe,
            max_combos: None,
        };
        let combos = SweepRunner::enumerate_grid(&config);
        assert_eq!(combos.len(), 9);
        assert_eq!(combos[0].get("p1"), Some(&1.0));
        assert_eq!(combos[0].get("p2"), Some(&10.0));
        assert_eq!(combos[8].get("p1"), Some(&3.0));
        assert_eq!(combos[8].get("p2"), Some(&30.0));
    }

    #[test]
    fn test_sweep_config_empty_params() {
        let config = SweepConfig {
            mode: SweepMode::Grid,
            param_ranges: vec![],
            metric: Metric::Sharpe,
            max_combos: None,
        };
        assert_eq!(config.validate().unwrap(), 0);
        let combos = SweepRunner::enumerate_grid(&config);
        assert!(combos.is_empty());
    }

    #[test]
    fn test_run_metrics_debug() {
        let m = RunMetrics {
            params: HashMap::from([("p1".to_string(), 1.0)]),
            sharpe: 1.5,
            pnl: 1000.0,
            max_dd: 0.05,
            num_trades: 42,
            duration_ms: 150,
        };
        let dbg = format!("{:?}", m);
        assert!(dbg.contains("p1"));
        assert!(dbg.contains("1.5"));
    }

    #[test]
    fn test_sweep_result_debug() {
        let result = SweepResult {
            best_params: HashMap::from([("p1".to_string(), 2.0)]),
            best_sharpe: 2.1,
            all_runs: vec![],
        };
        let dbg = format!("{:?}", result);
        assert!(dbg.contains("best_sharpe"));
    }

    #[test]
    fn test_sweep_mode_debug() {
        let grid = SweepMode::Grid;
        let rnd = SweepMode::Random { seed: 99, n: 50 };
        assert_eq!(format!("{:?}", grid), "Grid");
        assert!(format!("{:?}", rnd).contains("99"));
    }
}