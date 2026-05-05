//! Parameter sweeps — Rayon parallel grid search across parameter space.
//!
//! # Architecture
//! - `SweepRunner`: manages parallel grid search
//! - `Arc<RingBufferSet>` shared across workers (zero-copy)
//! - `Strategy: Clone` — each combo gets fresh strategy instance
//! - `run_grid(grid, filters, rank_by, top_n)` → parallel filtered results
//!
//! # Exit Criteria
//! 100-combo sweep wall time < sequential_time / num_cpus × 1.2. Results match sequential baseline.

use crate::buffer::buffer_set::RingBufferSet;
use crate::portfolio::{Portfolio, PortfolioConfig};
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
    pub win_rate: f64,
}

pub struct SweepRunner {
    buffer_set: Arc<RingBufferSet>,
    config: PortfolioConfig,
}

impl SweepRunner {
    pub fn new(buffer_set: Arc<RingBufferSet>, initial_equity: f64) -> Self {
        Self {
            buffer_set,
            config: PortfolioConfig::new(initial_equity, crate::engine::CommissionConfig::new(0.001)),
        }
    }

    pub fn with_config(mut self, config: PortfolioConfig) -> Self {
        self.config = config;
        self
    }

    /// Run a parameter grid sweep with a strategy factory.
    /// The factory receives a HashMap of param values and returns a strategy instance.
    pub fn run_grid<S: nexus_strategy::Strategy + Clone + Send + 'static>(
        &self,
        grid: &ParameterGrid,
        strategy_factory: impl Fn(HashMap<String, f64>) -> S + Send + Sync,
    ) -> Vec<SweepResult> {
        let combos: Vec<_> = grid.iter().collect();

        combos
            .par_iter()
            .filter_map(|params| {
                let strategy = strategy_factory(params.clone());
                let mut portfolio = Portfolio::new(self.config.initial_equity_per_instrument);

                for instrument_id in self.buffer_set.instrument_ids() {
                    portfolio.register_instrument(instrument_id.clone());
                }

                // Run tick loop using RingBufferSet iter_state_from_global_tick
                run_sweep_tick_loop(
                    &self.buffer_set,
                    &mut portfolio,
                    strategy,
                    &self.config,
                );

                let num_instruments = portfolio.num_instruments() as f64;
                let initial_total = self.config.initial_equity_per_instrument * num_instruments;
                let final_equity = portfolio.portfolio_equity();
                let pnl = final_equity - initial_total;
                let max_drawdown = portfolio.portfolio_max_drawdown();
                let num_trades = portfolio.total_trades();
                let win_rate = portfolio.win_rate();

                // Compute Sharpe from equity curve
                let returns = portfolio.returns();
                let sharpe = compute_sharpe(&returns);

                Some(SweepResult {
                    params: params.clone(),
                    pnl,
                    sharpe,
                    max_drawdown,
                    num_trades,
                    win_rate,
                })
            })
            .collect()
    }
}

/// Run a sweep tick loop over a RingBufferSet using merged anchor iteration.
fn run_sweep_tick_loop<S: nexus_strategy::Strategy>(
    buffer_set: &RingBufferSet,
    portfolio: &mut Portfolio,
    mut strategy: S,
    config: &PortfolioConfig,
) {
    use crate::engine::Signal as EngineSignal;
    use nexus_strategy::{Strategy, StrategyCtx};
    use crate::buffer::buffer_set::RingBufferSet;
    use crate::engine::core::EngineContext;
    use std::collections::HashMap;

    let total_ticks = buffer_set.total_ticks();
    let instrument_ids = buffer_set.instrument_ids();
    let mut last_prices: HashMap<u32, f64> = HashMap::new();

    for global_tick in 0..total_ticks {
        let Some((buffer, offset, tick_idx, anchor_slot)) =
            buffer_set.iter_state_from_global_tick(global_tick)
        else {
            continue;
        };

        let Ok(tick) = buffer.decode_anchor_at(offset) else {
            continue;
        };

        let ts = tick.timestamp_ns;
        let price = tick.price_int as f64 / 1e9;
        let size = tick.size_int as f64 / 1e9;

        // Get instrument id from tick (buffer_idx -> instrument from RingBufferSet)
        let anchor = &buffer_set.merged_anchors()[global_tick as usize];
        let instrument_id = instrument_ids.get(anchor.buffer_idx).cloned()
            .unwrap_or_else(|| instrument_ids.first().cloned().unwrap());

        // Update last price
        last_prices.insert(instrument_id.id, price);

        // Update unrealized PnL
        if let Some(state) = portfolio.state_mut(&instrument_id) {
            state.update_unrealized_pnl(price);
        }

        // Record equity
        portfolio.record_equity();

        // Build context
        let mut ctx = EngineContext::new(
            config.initial_equity_per_instrument,
            std::sync::Arc::new(std::sync::Mutex::new(crate::signals::SignalBus::new())),
            std::ptr::null_mut(),
        );
        ctx.subscribe_instruments(vec![instrument_id.clone()]);

        // Convert to nexus_types::Tick
        let ntick = nexus_types::Tick {
            timestamp_ns: tick.timestamp_ns,
            price,
            size,
            vpin: 0.0,
        };

        // Call strategy
        if let Some(signal) = strategy.on_trade(instrument_id.clone(), &ntick, &mut ctx) {
            // Route signal through portfolio
            let engine_signal = match signal {
                nexus_types::Signal::Buy => EngineSignal::Buy,
                nexus_types::Signal::Sell => EngineSignal::Sell,
                nexus_types::Signal::Close => EngineSignal::Close,
            };

            let position = portfolio.state(&instrument_id)
                .map(|s| s.position).unwrap_or(0.0);
            let has_position = position != 0.0;
            let is_long = position > 0.0;

            match (engine_signal, has_position, is_long) {
                (EngineSignal::Buy, false, _) | (EngineSignal::Buy, true, false) => {
                    if let Some(state) = portfolio.state_mut(&instrument_id) {
                        let size = if has_position { position.abs() + 1.0 } else { 1.0 };
                        let comm = config.commission.compute(price, size.abs());
                        if state.position == 0.0 {
                            state.position = size;
                            state.entry_price = price;
                        } else {
                            let pnl = if state.position > 0.0 {
                                (price - state.entry_price) * state.position.abs()
                            } else {
                                (state.entry_price - price) * state.position.abs()
                            };
                            state.realized_pnl += pnl;
                            state.position = size;
                            state.entry_price = price;
                        }
                        state.equity -= comm;
                        state.commissions += comm;
                        state.num_trades += 1;
                    }
                }
                (EngineSignal::Sell, false, _) | (EngineSignal::Sell, true, true) => {
                    if let Some(state) = portfolio.state_mut(&instrument_id) {
                        let size = if has_position { position.abs() + 1.0 } else { 1.0 };
                        let comm = config.commission.compute(price, size.abs());
                        if state.position == 0.0 {
                            state.position = -size;
                            state.entry_price = price;
                        } else {
                            let pnl = if state.position > 0.0 {
                                (price - state.entry_price) * state.position.abs()
                            } else {
                                (state.entry_price - price) * state.position.abs()
                            };
                            state.realized_pnl += pnl;
                            state.position = -size;
                            state.entry_price = price;
                        }
                        state.equity -= comm;
                        state.commissions += comm;
                        state.num_trades += 1;
                    }
                }
                (EngineSignal::Close, true, _) => {
                    if let Some(state) = portfolio.state_mut(&instrument_id) {
                        let pnl = if state.position > 0.0 {
                            (price - state.entry_price) * state.position.abs()
                        } else {
                            (state.entry_price - price) * state.position.abs()
                        };
                        let comm = config.commission.compute(price, state.position.abs());
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

        // Update peaks
        for (id, p) in last_prices.iter() {
            if let Some(state) = portfolio.state_mut(&instrument_id) {
                state.update_peak(*p);
            }
        }
    }
}

/// Compute annualized Sharpe ratio from return series.
fn compute_sharpe(returns: &[f64]) -> f64 {
    if returns.len() < 2 {
        return 0.0;
    }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter()
        .map(|r| (r - mean).powi(2))
        .sum::<f64>() / returns.len() as f64;
    let std_dev = variance.sqrt();
    if std_dev == 0.0 {
        return 0.0;
    }
    let daily_sharpe = mean / std_dev;
    daily_sharpe * (252.0_f64).sqrt()
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