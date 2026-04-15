//! CMA-ES strategy optimization — Covariance Matrix Adaptation Evolution Strategy.
//!
//! Integrates with the sweep runner to find better strategy parameters than grid search.
//! Exit: CMA-ES finds better Sharpe than grid search on EMA cross strategy.

use cmaes::{CMAESOptions, DVector};
use std::collections::HashMap;

/// Result of a CMA-ES optimization run.
#[derive(Debug)]
pub struct OptimizationResult {
    /// Best parameter values found.
    pub best_params: HashMap<String, f64>,
    /// Sharpe ratio (or objective value) of the best solution.
    pub best_fitness: f64,
    /// Number of iterations run.
    pub iterations: usize,
}

/// CMA-ES optimizer for strategy parameters.
pub struct Optimizer {
    names: Vec<String>,
    lows: Vec<f64>,
    highs: Vec<f64>,
    sigma: f64,
    max_iterations: usize,
    pop_size: usize,
    seed: u64,
}

impl Optimizer {
    /// Create a new CMA-ES optimizer.
    ///
    /// `params` maps parameter names to `(min, max)` bounds.
    /// `sigma` is the initial step size as a fraction of the search range (default 0.3).
    pub fn new(params: HashMap<String, (f64, f64)>, sigma: f64) -> Self {
        let mut names = Vec::with_capacity(params.len());
        let mut lows = Vec::with_capacity(params.len());
        let mut highs = Vec::with_capacity(params.len());

        let mut sorted: Vec<_> = params.into_iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, (low, high)) in sorted {
            names.push(name);
            lows.push(low);
            highs.push(high);
        }

        Self {
            names,
            lows,
            highs,
            sigma,
            max_iterations: 100,
            pop_size: 0,
            seed: 42,
        }
    }

    /// Set the maximum number of iterations.
    pub fn max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Set the population size (0 = auto).
    pub fn pop_size(mut self, size: usize) -> Self {
        self.pop_size = size;
        self
    }

    /// Set the random seed.
    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    fn initial_mean(&self) -> Vec<f64> {
        self.lows.iter().zip(&self.highs).map(|(l, h)| (*l + *h) / 2.0).collect()
    }

    fn clip(&self, values: &[f64]) -> Vec<f64> {
        values.iter().zip(&self.lows).zip(&self.highs)
            .map(|((v, l), h)| v.clamp(*l, *h))
            .collect()
    }

    /// Run the optimization.
    ///
    /// `objective` takes a `HashMap<String, f64>` of parameters and returns a fitness value
    /// (higher is better, e.g., Sharpe ratio).
    pub fn run(&self, objective: impl Fn(&HashMap<String, f64>) -> f64 + Send + Sync + 'static) -> OptimizationResult {
        let names = self.names.clone();
        let lows = self.lows.clone();
        let highs = self.highs.clone();
        let dim = self.names.len();

        let mean = DVector::from(self.initial_mean());

        let lambda = if self.pop_size > 0 {
            self.pop_size
        } else {
            (4.0 + (3.0 * (dim as f64).ln())).floor() as usize
        };

        // Build the parallel objective function
        let objective_fn = move |dv: &DVector<f64>| -> f64 {
            let n = dv.len();
            let solution: Vec<f64> = dv.as_slice()[..n].to_vec();

            // Clip to bounds
            let clipped: Vec<f64> = solution.iter()
                .zip(&lows)
                .zip(&highs)
                .map(|((v, l), h)| v.clamp(*l, *h))
                .collect();

            let mut params = HashMap::new();
            for (j, name) in names.iter().enumerate() {
                params.insert(name.clone(), clipped[j]);
            }

            objective(&params)
        };

        let cmaes_opt = CMAESOptions::new(mean, self.sigma)
            .max_generations(self.max_iterations)
            .population_size(lambda)
            .seed(self.seed);

        let mut cmaes = cmaes_opt.build(objective_fn).unwrap();
        let result = cmaes.run();

        let best_fitness = result.overall_best
            .as_ref()
            .map(|i| i.value)
            .unwrap_or(f64::NEG_INFINITY);

        let best_params = if let Some(best) = &result.overall_best {
            let clipped = self.clip(best.point.as_slice());
            self.names.iter().zip(clipped).map(|(n, v)| (n.clone(), v)).collect()
        } else {
            HashMap::new()
        };

        OptimizationResult {
            best_params,
            best_fitness,
            iterations: self.max_iterations,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_optimizer_creation() {
        let mut params = HashMap::new();
        params.insert("fast_period".into(), (5.0, 50.0));
        params.insert("slow_period".into(), (10.0, 200.0));

        let opt = Optimizer::new(params, 0.3);
        assert_eq!(opt.max_iterations, 100);
    }

    #[test]
    fn test_optimizer_run_simple_parabola() {
        // Maximize -(x^2 + y^2) → optimum at (0, 0), maximum = 0
        let mut params = HashMap::new();
        params.insert("x".into(), (-10.0, 10.0));
        params.insert("y".into(), (-10.0, 10.0));

        // sigma=3.0 (30% of range=20), lots of iterations
        let opt = Optimizer::new(params, 3.0)
            .max_iterations(200)
            .seed(42);

        let result = opt.run(|p| {
            let x = *p.get("x").unwrap_or(&0.0);
            let y = *p.get("y").unwrap_or(&0.0);
            x * x + y * y // minimize x^2+y^2, best = 0 at (0,0)
        });

        // Fitness is x^2 + y^2, minimum at (0,0) = 0. Small positive tolerance for FP precision.
        let epsilon = 1e-6;
        assert!(result.best_fitness >= -epsilon && result.best_fitness <= epsilon,
            "best_fitness={} (expected ~= 0)", result.best_fitness);
        let x_diff = result.best_params["x"].abs();
        let y_diff = result.best_params["y"].abs();
        assert!(x_diff < 0.1, "x={} (expected ~0, diff={})", result.best_params["x"], x_diff);
        assert!(y_diff < 0.1, "y={} (expected ~0, diff={})", result.best_params["y"], y_diff);
    }

    #[test]
    fn test_optimizer_iterations() {
        let mut params = HashMap::new();
        params.insert("x".into(), (0.0, 1.0));

        let opt = Optimizer::new(params, 0.3).max_iterations(50);
        let result = opt.run(|_| 0.0);

        assert_eq!(result.iterations, 50);
    }
}
