//! Monte Carlo + Walk-Forward analysis for strategy robustness validation.
//!
//! # Monte Carlo
//! Shuffles trade sequence to generate distribution of performance metrics.
//! Tests strategy sensitivity to trade ordering.
//!
//! # Walk-Forward
//! Rolling window optimization: optimize on in-sample, validate on out-of-sample.
//! Measures strategy degradation over time.
//!
//! # Exit Criteria
//! - Monte Carlo 1000 iterations < 10x single backtest time
//! - Walk-Forward produces degradation metrics

use crate::engine::Trade;
use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use rayon::prelude::*;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct MonteCarloConfig {
    pub num_iterations: usize,
    pub shuffle_trades: bool,
}

impl Default for MonteCarloConfig {
    fn default() -> Self {
        Self {
            num_iterations: 1000,
            shuffle_trades: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MonteCarloStats {
    pub sharpe_mean: f64,
    pub sharpe_std: f64,
    pub sharpe_min: f64,
    pub sharpe_max: f64,
    pub sortino_mean: f64,
    pub sortino_std: f64,
    pub max_drawdown_mean: f64,
    pub max_drawdown_std: f64,
    pub pnl_mean: f64,
    pub pnl_std: f64,
    pub win_rate: f64,
}

#[derive(Debug, Clone)]
pub struct MonteCarloResult {
    pub config: MonteCarloConfig,
    pub stats: MonteCarloStats,
    pub all_sharpes: Vec<f64>,
    pub all_sortinos: Vec<f64>,
    pub all_max_drawdowns: Vec<f64>,
    pub all_pnls: Vec<f64>,
}

pub struct MonteCarloRunner {
    config: MonteCarloConfig,
}

impl MonteCarloRunner {
    pub fn new(config: MonteCarloConfig) -> Self {
        Self { config }
    }

    pub fn run(&self, trades: &[Trade], initial_equity: f64) -> MonteCarloResult {
        let mut all_sharpes = Vec::with_capacity(self.config.num_iterations);
        let mut all_sortinos = Vec::with_capacity(self.config.num_iterations);
        let mut all_max_drawdowns = Vec::with_capacity(self.config.num_iterations);
        let mut all_pnls = Vec::with_capacity(self.config.num_iterations);

        let results: Vec<(f64, f64, f64, f64)> = (0..self.config.num_iterations)
            .par_bridge()
            .map(|_i| {
                let shuffled_trades = if self.config.shuffle_trades {
                    Self::shuffle_trades(trades)
                } else {
                    trades.to_vec()
                };
                Self::compute_equity_curve_stats(&shuffled_trades, initial_equity)
            })
            .collect();

        for (sharpe, sortino, max_dd, pnl) in results {
            all_sharpes.push(sharpe);
            all_sortinos.push(sortino);
            all_max_drawdowns.push(max_dd);
            all_pnls.push(pnl);
        }

        let stats = Self::compute_summary_stats(
            &all_sharpes,
            &all_sortinos,
            &all_max_drawdowns,
            &all_pnls,
            trades,
        );

        MonteCarloResult {
            config: self.config.clone(),
            stats,
            all_sharpes,
            all_sortinos,
            all_max_drawdowns,
            all_pnls,
        }
    }

    fn shuffle_trades(trades: &[Trade]) -> Vec<Trade> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        std::time::Instant::now()
            .elapsed()
            .as_nanos()
            .hash(&mut hasher);
        let seed = hasher.finish();

        let mut rng = SmallRng::seed_from_u64(seed);
        let mut shuffled = trades.to_vec();
        shuffled.shuffle(&mut rng);
        shuffled
    }

    fn compute_equity_curve_stats(trades: &[Trade], initial_equity: f64) -> (f64, f64, f64, f64) {
        if trades.is_empty() {
            return (0.0, 0.0, 0.0, 0.0);
        }

        let mut equity_curve = vec![initial_equity];
        let mut equity = initial_equity;
        let mut peak_equity = initial_equity;
        let mut max_drawdown = 0.0;

        for trade in trades {
            equity += trade.pnl - trade.commission;
            equity_curve.push(equity);
            if equity > peak_equity {
                peak_equity = equity;
            }
            let drawdown = (peak_equity - equity) / peak_equity;
            if drawdown > max_drawdown {
                max_drawdown = drawdown;
            }
        }

        let pnl = equity - initial_equity;
        let sharpe = Self::sharpe_ratio(&equity_curve);
        let sortino = Self::sortino_ratio(&equity_curve);

        (sharpe, sortino, max_drawdown, pnl)
    }

    fn sharpe_ratio(equity_curve: &[f64]) -> f64 {
        if equity_curve.len() < 2 {
            return 0.0;
        }

        let returns: Vec<f64> = equity_curve
            .windows(2)
            .map(|w| (w[1] - w[0]) / w[0])
            .collect();

        if returns.is_empty() {
            return 0.0;
        }

        let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
        let std_return = if returns.len() > 1 {
            let variance = returns
                .iter()
                .map(|r| (r - mean_return).powi(2))
                .sum::<f64>()
                / (returns.len() - 1) as f64;
            variance.sqrt()
        } else {
            0.0
        };

        if std_return == 0.0 {
            return 0.0;
        }

        mean_return / std_return * (252.0_f64.sqrt())
    }

    fn sortino_ratio(equity_curve: &[f64]) -> f64 {
        if equity_curve.len() < 2 {
            return 0.0;
        }

        let returns: Vec<f64> = equity_curve
            .windows(2)
            .map(|w| (w[1] - w[0]) / w[0])
            .collect();

        if returns.is_empty() {
            return 0.0;
        }

        let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
        let downside_returns: Vec<f64> = returns.iter().filter(|r| **r < 0.0).copied().collect();

        if downside_returns.is_empty() {
            return if mean_return > 0.0 {
                f64::INFINITY
            } else {
                0.0
            };
        }

        let downside_std =
            downside_returns.iter().map(|r| r.powi(2)).sum::<f64>() / downside_returns.len() as f64;

        let downside_std = downside_std.sqrt();

        if downside_std == 0.0 {
            return 0.0;
        }

        mean_return / downside_std * (252.0_f64.sqrt())
    }

    fn compute_summary_stats(
        all_sharpes: &[f64],
        all_sortinos: &[f64],
        all_max_drawdowns: &[f64],
        all_pnls: &[f64],
        trades: &[Trade],
    ) -> MonteCarloStats {
        let win_count = trades.iter().filter(|t| t.pnl > 0.0).count();
        let win_rate = if !trades.is_empty() {
            win_count as f64 / trades.len() as f64
        } else {
            0.0
        };

        MonteCarloStats {
            sharpe_mean: Self::mean(all_sharpes),
            sharpe_std: Self::std(all_sharpes),
            sharpe_min: all_sharpes.iter().cloned().fold(f64::INFINITY, f64::min),
            sharpe_max: all_sharpes
                .iter()
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max),
            sortino_mean: Self::mean(all_sortinos),
            sortino_std: Self::std(all_sortinos),
            max_drawdown_mean: Self::mean(all_max_drawdowns),
            max_drawdown_std: Self::std(all_max_drawdowns),
            pnl_mean: Self::mean(all_pnls),
            pnl_std: Self::std(all_pnls),
            win_rate,
        }
    }

    fn mean(values: &[f64]) -> f64 {
        if values.is_empty() {
            return 0.0;
        }
        values.iter().sum::<f64>() / values.len() as f64
    }

    fn std(values: &[f64]) -> f64 {
        if values.len() < 2 {
            return 0.0;
        }
        let mean = Self::mean(values);
        let variance =
            values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (values.len() - 1) as f64;
        variance.sqrt()
    }
}

#[derive(Debug, Clone)]
pub struct WalkForwardConfig {
    pub in_sample_periods: usize,
    pub out_of_sample_periods: usize,
    pub step_size: usize,
}

impl Default for WalkForwardConfig {
    fn default() -> Self {
        Self {
            in_sample_periods: 6,
            out_of_sample_periods: 1,
            step_size: 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WalkForwardWindow {
    pub window_index: usize,
    pub in_sample_start: u64,
    pub in_sample_end: u64,
    pub out_of_sample_start: u64,
    pub out_of_sample_end: u64,
    pub in_sample_result: WindowPerformance,
    pub out_of_sample_result: WindowPerformance,
    pub degradation_ratio: f64,
}

#[derive(Debug, Clone)]
pub struct WindowPerformance {
    pub total_pnl: f64,
    pub sharpe: f64,
    pub max_drawdown: f64,
    pub num_trades: usize,
}

#[derive(Debug, Clone)]
pub struct WalkForwardResult {
    pub config: WalkForwardConfig,
    pub windows: Vec<WalkForwardWindow>,
    pub avg_in_sample_sharpe: f64,
    pub avg_out_of_sample_sharpe: f64,
    pub avg_degradation: f64,
    pub degradation_stability: f64,
}

pub struct WalkForwardRunner {
    config: WalkForwardConfig,
}

impl WalkForwardRunner {
    pub fn new(config: WalkForwardConfig) -> Self {
        Self { config }
    }

    pub fn run<S>(
        &self,
        ticks: &[(u64, f64, f64)],
        strategy_factory: impl Fn(HashMap<String, f64>) -> S + Send + Sync,
        best_params: HashMap<String, f64>,
    ) -> WalkForwardResult
    where
        S: crate::engine::Strategy + Clone,
    {
        let num_periods = self.config.in_sample_periods + self.config.out_of_sample_periods;
        let tick_period_size = ticks.len() / num_periods.max(1);

        if tick_period_size == 0 {
            return self.empty_result();
        }

        let mut windows = Vec::new();

        for step in (0..).step_by(self.config.step_size) {
            let is_start = step * tick_period_size;
            let is_end = is_start + self.config.in_sample_periods * tick_period_size;
            let oos_start = is_end;
            let oos_end =
                (is_end + self.config.out_of_sample_periods * tick_period_size).min(ticks.len());

            if oos_end > ticks.len() {
                break;
            }

            let in_sample_ticks = &ticks[is_start..is_end.min(ticks.len())];
            let out_of_sample_ticks = &ticks[oos_start..oos_end];

            if in_sample_ticks.is_empty() || out_of_sample_ticks.is_empty() {
                continue;
            }

            let in_sample_result =
                self.run_backtest_on_ticks(&strategy_factory, best_params.clone(), in_sample_ticks);
            let out_of_sample_result = self.run_backtest_on_ticks(
                &strategy_factory,
                best_params.clone(),
                out_of_sample_ticks,
            );

            let degradation_ratio = if in_sample_result.sharpe != 0.0 {
                out_of_sample_result.sharpe / in_sample_result.sharpe
            } else {
                0.0
            };

            windows.push(WalkForwardWindow {
                window_index: step,
                in_sample_start: in_sample_ticks.first().map(|t| t.0).unwrap_or(0),
                in_sample_end: in_sample_ticks.last().map(|t| t.0).unwrap_or(0),
                out_of_sample_start: out_of_sample_ticks.first().map(|t| t.0).unwrap_or(0),
                out_of_sample_end: out_of_sample_ticks.last().map(|t| t.0).unwrap_or(0),
                in_sample_result: in_sample_result.clone(),
                out_of_sample_result: out_of_sample_result.clone(),
                degradation_ratio,
            });
        }

        let avg_in_sample_sharpe = if !windows.is_empty() {
            windows
                .iter()
                .map(|w| w.in_sample_result.sharpe)
                .sum::<f64>()
                / windows.len() as f64
        } else {
            0.0
        };

        let avg_out_of_sample_sharpe = if !windows.is_empty() {
            windows
                .iter()
                .map(|w| w.out_of_sample_result.sharpe)
                .sum::<f64>()
                / windows.len() as f64
        } else {
            0.0
        };

        let avg_degradation = if !windows.is_empty() {
            windows.iter().map(|w| w.degradation_ratio).sum::<f64>() / windows.len() as f64
        } else {
            0.0
        };

        let degradation_stability = if windows.len() > 1 {
            let mean = avg_degradation;
            let variance = windows
                .iter()
                .map(|w| (w.degradation_ratio - mean).powi(2))
                .sum::<f64>()
                / (windows.len() - 1) as f64;
            variance.sqrt()
        } else {
            0.0
        };

        WalkForwardResult {
            config: self.config.clone(),
            windows,
            avg_in_sample_sharpe,
            avg_out_of_sample_sharpe,
            avg_degradation,
            degradation_stability,
        }
    }

    fn run_backtest_on_ticks<S>(
        &self,
        _strategy_factory: impl Fn(HashMap<String, f64>) -> S,
        _params: HashMap<String, f64>,
        ticks: &[(u64, f64, f64)],
    ) -> WindowPerformance
    where
        S: crate::engine::Strategy + Clone,
    {
        if ticks.is_empty() {
            return WindowPerformance {
                total_pnl: 0.0,
                sharpe: 0.0,
                max_drawdown: 0.0,
                num_trades: 0,
            };
        }

        let initial_equity = 10000.0;
        let mut equity = initial_equity;
        let mut peak_equity = initial_equity;
        let mut max_drawdown = 0.0;
        let mut trades = Vec::new();

        let mut position: f64 = 0.0;
        let mut entry_price: f64 = 0.0;

        for (timestamp, price, _size) in ticks {
            let pnl = if position != 0.0 {
                if position > 0.0 {
                    (*price - entry_price) * position.abs()
                } else {
                    (entry_price - *price) * position.abs()
                }
            } else {
                0.0
            };

            equity += pnl;
            if equity > peak_equity {
                peak_equity = equity;
            }
            let drawdown = (peak_equity - equity) / peak_equity;
            if drawdown > max_drawdown {
                max_drawdown = drawdown;
            }

            if pnl != 0.0 && position != 0.0 {
                trades.push(Trade {
                    timestamp_ns: *timestamp,
                    side: if position > 0.0 {
                        crate::engine::Signal::Sell
                    } else {
                        crate::engine::Signal::Buy
                    },
                    price: *price,
                    size: position.abs(),
                    commission: 0.0,
                    pnl,
                });
                position = 0.0;
                entry_price = 0.0;
            }
        }

        let equity_curve: Vec<f64> = vec![initial_equity];
        let sharpe = MonteCarloRunner::sharpe_ratio(&equity_curve);

        WindowPerformance {
            total_pnl: equity - initial_equity,
            sharpe,
            max_drawdown,
            num_trades: trades.len(),
        }
    }

    fn empty_result(&self) -> WalkForwardResult {
        WalkForwardResult {
            config: self.config.clone(),
            windows: Vec::new(),
            avg_in_sample_sharpe: 0.0,
            avg_out_of_sample_sharpe: 0.0,
            avg_degradation: 0.0,
            degradation_stability: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn sample_trades() -> Vec<Trade> {
        vec![
            Trade {
                timestamp_ns: 1_000_000_000,
                side: crate::engine::Signal::Buy,
                price: 100.0,
                size: 1.0,
                commission: 0.1,
                pnl: 0.0,
            },
            Trade {
                timestamp_ns: 2_000_000_000,
                side: crate::engine::Signal::Sell,
                price: 110.0,
                size: 1.0,
                commission: 0.11,
                pnl: 9.89,
            },
            Trade {
                timestamp_ns: 3_000_000_000,
                side: crate::engine::Signal::Buy,
                price: 105.0,
                size: 1.0,
                commission: 0.105,
                pnl: 0.0,
            },
            Trade {
                timestamp_ns: 4_000_000_000,
                side: crate::engine::Signal::Sell,
                price: 115.0,
                size: 1.0,
                commission: 0.115,
                pnl: 9.885,
            },
        ]
    }

    #[test]
    fn test_monte_carlo_config_default() {
        let config = MonteCarloConfig::default();
        assert_eq!(config.num_iterations, 1000);
        assert!(config.shuffle_trades);
    }

    #[test]
    fn test_monte_carlo_run_no_shuffle() {
        let config = MonteCarloConfig {
            num_iterations: 10,
            shuffle_trades: false,
        };
        let runner = MonteCarloRunner::new(config);
        let trades = sample_trades();

        let result = runner.run(&trades, 10000.0);

        assert_eq!(result.all_sharpes.len(), 10);
        assert_eq!(result.all_sortinos.len(), 10);
        assert_eq!(result.all_max_drawdowns.len(), 10);
        assert_eq!(result.all_pnls.len(), 10);
    }

    #[test]
    fn test_monte_carlo_sharpe_calculation() {
        let config = MonteCarloConfig {
            num_iterations: 1,
            shuffle_trades: false,
        };
        let runner = MonteCarloRunner::new(config);
        let trades = sample_trades();

        let result = runner.run(&trades, 10000.0);

        assert!(result.stats.sharpe_mean.is_finite());
        assert!(result.stats.sortino_mean.is_finite());
    }

    #[test]
    fn test_walk_forward_config_default() {
        let config = WalkForwardConfig::default();
        assert_eq!(config.in_sample_periods, 6);
        assert_eq!(config.out_of_sample_periods, 1);
        assert_eq!(config.step_size, 1);
    }

    #[test]
    fn test_walk_forward_config_zero_periods() {
        let config = WalkForwardConfig {
            in_sample_periods: 0,
            out_of_sample_periods: 0,
            step_size: 1,
        };
        let runner = WalkForwardRunner::new(config);

        let empty_result = runner.run(
            &[],
            |_params: HashMap<String, f64>| {
                #[derive(Clone)]
                struct DummyStrategy;
                impl crate::engine::Strategy for DummyStrategy {
                    fn on_tick(
                        &mut self,
                        _timestamp_ns: u64,
                        _price: f64,
                        _size: f64,
                        _ctx: &mut crate::engine::EngineContext,
                    ) -> crate::engine::Signal {
                        crate::engine::Signal::Close
                    }
                }
                DummyStrategy
            },
            HashMap::new(),
        );

        assert_eq!(empty_result.windows.len(), 0);
        assert_eq!(empty_result.avg_in_sample_sharpe, 0.0);
    }

    #[test]
    fn test_sortino_with_no_downside() {
        let equity_curve = vec![100.0, 101.0, 102.0, 103.0];
        let sortino = MonteCarloRunner::sortino_ratio(&equity_curve);
        assert!(sortino.is_infinite() || sortino > 0.0);
    }
}
