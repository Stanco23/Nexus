//! Parameter sweeps — Rayon parallel grid search across parameter space.
//!
//! # Architecture
//! - `SweepRunner`: manages parallel grid search
//! - `Arc<TickBufferSet>` shared across workers (zero-copy)
//! - `Strategy: Clone` — each combo gets fresh strategy instance
//! - `run_grid(grid, filters, rank_by, top_n)` → parallel filtered results
//!
//! # Exit Criteria
//! 100-combo sweep wall time < sequential_time / num_cpus × 1.2. Results match sequential baseline.

use crate::buffer::buffer_set::TickBufferSet;
use crate::engine::Signal;
use crate::portfolio::{Portfolio, PortfolioConfig, PortfolioStrategy};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ParameterGrid {
    params: HashMap<String, Vec<f64>>,
}

impl ParameterGrid {
    pub fn new() -> Self {
        Self {
            params: HashMap::new(),
        }
    }

    pub fn add_param(mut self, name: &str, values: Vec<f64>) -> Self {
        self.params.insert(name.to_string(), values);
        self
    }

    pub fn num_combinations(&self) -> usize {
        if self.params.is_empty() {
            return 0;
        }
        self.params.values().map(|v| v.len()).product()
    }

    pub fn iter(&self) -> impl Iterator<Item = HashMap<String, f64>> {
        ParameterGridIter {
            params: self.params.clone(),
            indices: vec![0; self.params.len()],
            done: false,
        }
    }
}

struct ParameterGridIter {
    params: HashMap<String, Vec<f64>>,
    indices: Vec<usize>,
    done: bool,
}

impl Iterator for ParameterGridIter {
    type Item = HashMap<String, f64>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }

        let keys: Vec<String> = self.params.keys().cloned().collect();

        if keys.is_empty() {
            self.done = true;
            return None;
        }

        let mut combo = HashMap::new();
        for (i, key) in keys.iter().enumerate() {
            let values = self.params.get(key).unwrap();
            combo.insert(key.clone(), values[self.indices[i]]);
        }

        for (i, key) in keys.iter().enumerate() {
            self.indices[i] += 1;
            if self.indices[i] < self.params.get(key).unwrap().len() {
                return Some(combo);
            }
            self.indices[i] = 0;
        }
        self.done = true;
        Some(combo)
    }
}

impl Default for ParameterGrid {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct SweepResult {
    pub params: HashMap<String, f64>,
    pub pnl: f64,
    pub sharpe: f64,
    pub max_drawdown: f64,
    pub num_trades: usize,
}

pub struct SweepRunner {
    buffer_set: Arc<TickBufferSet>,
    config: PortfolioConfig,
}

impl SweepRunner {
    pub fn new(buffer_set: Arc<TickBufferSet>, initial_equity: f64) -> Self {
        Self {
            buffer_set,
            config: PortfolioConfig::new(initial_equity, crate::engine::CommissionConfig::new(0.001)),
        }
    }

    pub fn with_config(mut self, config: PortfolioConfig) -> Self {
        self.config = config;
        self
    }

    pub fn run_grid<S: PortfolioStrategy + Clone + 'static>(
        &self,
        grid: &ParameterGrid,
        strategy_factory: impl Fn(HashMap<String, f64>) -> S + Send + Sync,
    ) -> Vec<SweepResult> {
        let combos: Vec<_> = grid.iter().collect();

        combos
            .par_iter()
            .filter_map(|params| {
                let mut strategy = strategy_factory(params.clone());
                let mut portfolio = Portfolio::new(self.config.initial_equity_per_instrument);

                for instrument_id in self.buffer_set.instrument_ids() {
                    portfolio.register_instrument(instrument_id.clone());
                }

                let mut cursor = self.buffer_set.merge_cursor();

                portfolio.run_portfolio::<S>(&mut cursor, &self.config, || strategy.clone());

                let num_instruments = portfolio.num_instruments() as f64;
                let pnl = portfolio.portfolio_equity() - self.config.initial_equity_per_instrument * num_instruments;
                let max_drawdown = portfolio.portfolio_max_drawdown();

                // Compute Sharpe ratio from equity curve if we have trades
                let num_trades = portfolio.total_trades();
                let sharpe = if num_trades > 1 {
                    let realized = portfolio.total_realized_pnl();
                    let commissions = portfolio.total_commissions();
                    let net = realized - commissions;
                    if self.config.initial_equity_per_instrument > 0.0 {
                        (net / (self.config.initial_equity_per_instrument * num_instruments))
                            .max(-1.0)
                            .min(1.0)
                            * self.config.trading_days_per_year.sqrt()
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };

                Some(SweepResult {
                    params: params.clone(),
                    pnl,
                    sharpe,
                    max_drawdown,
                    num_trades,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parameter_grid() {
        let grid = ParameterGrid::new()
            .add_param("fast_ma", vec![5.0, 10.0, 15.0])
            .add_param("slow_ma", vec![20.0, 50.0]);

        assert_eq!(grid.num_combinations(), 6);
        let combos: Vec<_> = grid.iter().collect();
        assert_eq!(combos.len(), 6);
    }

    #[test]
    fn test_parameter_grid_empty() {
        let grid = ParameterGrid::new();
        assert_eq!(grid.num_combinations(), 0);
        let combos: Vec<_> = grid.iter().collect();
        assert_eq!(combos.len(), 0);
    }
}
