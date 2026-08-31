//! Capital spread configuration for multi-instrument backtests.
//!
//! Defines how initial equity is distributed across multiple instruments.
//! Used by BacktestEngine when multiple instruments are configured.

use std::fmt;
use thiserror::Error;

/// How to spread capital across multiple instruments.
#[derive(Debug, Clone)]
pub enum CapitalSpread {
    /// Equal capital split across all instruments.
    Equal,
    /// Weighted capital allocation per instrument.
    /// Weights are applied in order of instrument registration.
    /// Validation: weights must sum to > 0 and not contain NaN.
    Weighted(Vec<f64>),
}

impl CapitalSpread {
    /// Validate the spread configuration.
    /// Returns Ok(()) if valid, error otherwise.
    pub fn validate(&self) -> Result<(), CapitalSpreadError> {
        match self {
            CapitalSpread::Equal => Ok(()),
            CapitalSpread::Weighted(weights) => {
                if weights.is_empty() {
                    return Err(CapitalSpreadError::EmptyWeights);
                }
                let sum: f64 = weights.iter().sum();
                if sum == 0.0 {
                    return Err(CapitalSpreadError::ZeroSum);
                }
                if sum.is_nan() {
                    return Err(CapitalSpreadError::NaNSum);
                }
                if weights.iter().any(|w| w.is_nan()) {
                    return Err(CapitalSpreadError::NaNWeight);
                }
                if weights.iter().any(|w| w < &0.0) {
                    return Err(CapitalSpreadError::NegativeWeight);
                }
                Ok(())
            }
        }
    }

    /// Get the number of instruments this spread applies to.
    pub fn len(&self) -> usize {
        match self {
            CapitalSpread::Equal => 0,
            CapitalSpread::Weighted(w) => w.len(),
        }
    }

    /// Returns true if this is an Equal spread.
    pub fn is_equal(&self) -> bool {
        matches!(self, CapitalSpread::Equal)
    }
}

impl fmt::Display for CapitalSpread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CapitalSpread::Equal => write!(f, "Equal"),
            CapitalSpread::Weighted(w) => {
                write!(f, "Weighted({})", w.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", "))
            }
        }
    }
}

/// Validation errors for CapitalSpread.
#[derive(Debug, Error)]
pub enum CapitalSpreadError {
    #[error("weights list is empty")]
    EmptyWeights,

    #[error("weights sum to 0 — must be positive")]
    ZeroSum,

    #[error("weights contain NaN")]
    NaNWeight,

    #[error("weights sum to NaN")]
    NaNSum,

    #[error("negative weight found — weights must be non-negative")]
    NegativeWeight,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_equal_validate() {
        assert!(CapitalSpread::Equal.validate().is_ok());
    }

    #[test]
    fn test_weighted_valid() {
        let w = CapitalSpread::Weighted(vec![0.5, 0.5]);
        assert!(w.validate().is_ok());
    }

    #[test]
    fn test_weighted_zero_sum() {
        let w = CapitalSpread::Weighted(vec![0.0, 0.0]);
        assert!(matches!(w.validate(), Err(CapitalSpreadError::ZeroSum)));
    }

    #[test]
    fn test_weighted_nan() {
        let w = CapitalSpread::Weighted(vec![0.5, f64::NAN]);
        assert!(matches!(w.validate(), Err(CapitalSpreadError::NaNWeight)));
    }

    #[test]
    fn test_weighted_negative() {
        let w = CapitalSpread::Weighted(vec![0.5, -0.5]);
        assert!(matches!(w.validate(), Err(CapitalSpreadError::NegativeWeight)));
    }

    #[test]
    fn test_weighted_empty() {
        let w = CapitalSpread::Weighted(vec![]);
        assert!(matches!(w.validate(), Err(CapitalSpreadError::EmptyWeights)));
    }
}