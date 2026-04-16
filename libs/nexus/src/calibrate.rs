//! VPIN slippage calibration — fit constants to historical fill data.
//!
//! Takes a CSV of (order_size_ticks, vpin, actual_impact_bps) and fits the
//! `SlippageConfig` parameters via OLS. Reports R², compares linear vs
//! non-linear models, and validates on a held-out set.

#![allow(non_snake_case)]
#![allow(nonstandard_style)]

use serde::Deserialize;
use serde::Serialize;
use std::fs;

/// Calibration input row.
#[derive(Debug, Clone, Deserialize)]
pub struct CalibrationRow {
    pub order_size_ticks: f64,
    pub vpin: f64,
    pub actual_impact_bps: f64,
}

/// Fitted calibration result.
#[derive(Debug, Clone, Serialize)]
pub struct CalibrationResult {
    pub adverse_prob_factor: f64,
    pub vpin_impact_coef: f64,
    pub r_squared: f64,
    pub linear_r2: f64,
    pub quadratic_r2: f64,
    pub piecewise_r2: f64,
    pub non_linear_warning: bool,
    pub holdout_pass: bool,
    pub holdout_accuracy_pct: f64,
}

/// Fit a linear model: impact = a * sqrt(size)/100 + b * vpin
///
/// We decompose this into two terms and solve via OLS on the stacked terms.
fn fit_linear_model(rows: &[CalibrationRow]) -> (f64, f64, f64) {
    let n = rows.len() as f64;
    let mut Y: f64 = 0.0;
    let mut X0Y: f64 = 0.0;
    let mut X1Y: f64 = 0.0;
    let mut X0X0: f64 = 0.0;
    let mut X0X1: f64 = 0.0;
    let mut X1X1: f64 = 0.0;

    for r in rows {
        let size_term = r.order_size_ticks.sqrt() / 100.0;
        let vpin_term = r.vpin;
        let y = r.actual_impact_bps;

        Y += y;
        X0Y += size_term * y;
        X1Y += vpin_term * y;
        X0X0 += size_term * size_term;
        X0X1 += size_term * vpin_term;
        X1X1 += vpin_term * vpin_term;
    }

    // Solve 2x2 normal equations via Cramer's rule
    // [X0X0 X0X1][a] = [X0Y]
    // [X0X1 X1X1][b]   [X1Y]
    let det = X0X0 * X1X1 - X0X1 * X0X1;
    if det.abs() < 1e-12 {
        return (10.0, 5.0, 0.0);
    }
    let a = (X0Y * X1X1 - X1Y * X0X1) / det;
    let b = (X0X0 * X1Y - X0X1 * X0Y) / det;

    // Compute R²
    let y_mean = Y / n;
    let mut ss_tot: f64 = 0.0;
    let mut ss_res: f64 = 0.0;
    for r in rows {
        let pred = a * (r.order_size_ticks.sqrt() / 100.0) + b * r.vpin;
        ss_tot += (r.actual_impact_bps - y_mean).powi(2);
        ss_res += (r.actual_impact_bps - pred).powi(2);
    }
    let r2 = if ss_tot > 0.0 { 1.0 - ss_res / ss_tot } else { 0.0 };
    (a, b, r2)
}

/// Fit a quadratic model with vpin² term.
fn fit_quadratic_model(rows: &[CalibrationRow]) -> (f64, f64, f64, f64) {
    let n = rows.len() as f64;
    let mut Y: f64 = 0.0;
    let mut X0Y: f64 = 0.0;
    let mut X1Y: f64 = 0.0;
    let mut X2Y: f64 = 0.0;
    let mut X0X0: f64 = 0.0;
    let mut X0X1: f64 = 0.0;
    let mut X0X2: f64 = 0.0;
    let mut X1X1: f64 = 0.0;
    let mut X1X2: f64 = 0.0;
    let mut X2X2: f64 = 0.0;

    for r in rows {
        let size_term = r.order_size_ticks.sqrt() / 100.0;
        let vpin_sq = r.vpin * r.vpin;
        let y = r.actual_impact_bps;

        Y += y;
        X0Y += size_term * y;
        X1Y += r.vpin * y;
        X2Y += vpin_sq * y;
        X0X0 += size_term * size_term;
        X0X1 += size_term * r.vpin;
        X0X2 += size_term * vpin_sq;
        X1X1 += r.vpin * r.vpin;
        X1X2 += r.vpin * vpin_sq;
        X2X2 += vpin_sq * vpin_sq;
    }

    // Solve 3x3 via Gaussian elimination (no nalgebra SVD needed)
    // Build augmented matrix [A|b]
    let mut A: [[f64; 3]; 3] = [
        [X0X0, X0X1, X0X2],
        [X0X1, X1X1, X1X2],
        [X0X2, X1X2, X2X2],
    ];
    let mut b_vec: [f64; 3] = [X0Y, X1Y, X2Y];

    // Gaussian elimination with partial pivoting
    for col in 0..3 {
        let mut max_row = col;
        for row in (col + 1)..3 {
            if A[row][col].abs() > A[max_row][col].abs() {
                max_row = row;
            }
        }
        A.swap(col, max_row);
        b_vec.swap(col, max_row);

        let pivot = A[col][col];
        if pivot.abs() < 1e-12 {
            continue;
        }
        for row in (col + 1)..3 {
            let factor = A[row][col] / pivot;
            #[allow(clippy::needless_range_loop)]
            for k in col..3 {
                A[row][k] -= factor * A[col][k];
            }
            b_vec[row] -= factor * b_vec[col];
        }
    }

    // Back-substitution
    let mut x = [0.0; 3];
    for i in (0..3).rev() {
        let pivot = A[i][i];
        if pivot.abs() < 1e-12 {
            x[i] = 0.0;
            continue;
        }
        let mut sum = b_vec[i];
        for j in (i + 1)..3 {
            sum -= A[i][j] * x[j];
        }
        x[i] = sum / pivot;
    }

    // Compute R²
    let y_mean = Y / n;
    let mut ss_tot: f64 = 0.0;
    let mut ss_res: f64 = 0.0;
    for r in rows {
        let pred = x[0] * (r.order_size_ticks.sqrt() / 100.0) + x[1] * r.vpin + x[2] * r.vpin * r.vpin;
        ss_tot += (r.actual_impact_bps - y_mean).powi(2);
        ss_res += (r.actual_impact_bps - pred).powi(2);
    }
    let r2 = if ss_tot > 0.0 { 1.0 - ss_res / ss_tot } else { 0.0 };
    (x[0], x[1], x[2], r2)
}

/// Fit a 3-segment piecewise linear model on VPIN buckets.
fn fit_piecewise_model(rows: &[CalibrationRow]) -> f64 {
    // Bucket into 3 groups by VPIN: low [0, 0.33), mid [0.33, 0.66), high [0.66, 1.0]
    let (mut bucket_0, mut bucket_1, mut bucket_2): (Vec<&CalibrationRow>, Vec<&CalibrationRow>, Vec<&CalibrationRow>) =
        (Vec::new(), Vec::new(), Vec::new());

    for r in rows {
        if r.vpin < 0.33 {
            bucket_0.push(r);
        } else if r.vpin < 0.66 {
            bucket_1.push(r);
        } else {
            bucket_2.push(r);
        }
    }

    // Weighted average impact per bucket as proxy slope
    let avg_impact_ref = |bucket: &[&CalibrationRow]| -> f64 {
        if bucket.is_empty() {
            return 0.0;
        }
        bucket.iter().map(|r| r.actual_impact_bps).sum::<f64>() / bucket.len() as f64
    };
    let avg_impact_val = |bucket: &[CalibrationRow]| -> f64 {
        if bucket.is_empty() {
            return 0.0;
        }
        bucket.iter().map(|r| r.actual_impact_bps).sum::<f64>() / bucket.len() as f64
    };

    // Single-parameter: average impact per bucket weighted by bucket count
    let b0 = avg_impact_ref(&bucket_0);
    let b1 = avg_impact_ref(&bucket_1);
    let b2 = avg_impact_ref(&bucket_2);

    // Return mean of bucket impacts as R² proxy (simplified — actual piecewise would fit 3 slopes)
    // For proper piecewise, we'd fit 3 separate linear models and average R²
    let mean_impact = avg_impact_val(rows);
    if mean_impact.abs() < 0.001 {
        return 0.0;
    }
    // Simplified R² as 1 - (weighted_residual_variance / total_variance)
    let mut ss_res: f64 = 0.0;
    let mut ss_tot: f64 = 0.0;
    for r in rows {
        let bucket_avg = if r.vpin < 0.33 { b0 } else if r.vpin < 0.66 { b1 } else { b2 };
        ss_res += (r.actual_impact_bps - bucket_avg).powi(2);
        ss_tot += (r.actual_impact_bps - mean_impact).powi(2);
    }
    if ss_tot > 0.0 {
        1.0 - ss_res / ss_tot
    } else {
        0.0
    }
}

/// Run VPIN slippage calibration.
///
/// `input_path` — CSV with columns: `order_size_ticks, vpin, actual_impact_bps`
/// `output_path` — JSON file to write the resulting `SlippageConfig`
pub fn calibrate_vpin(input_path: &str, output_path: &str) -> CalibrationResult {
    let contents = fs::read_to_string(input_path).expect("Failed to read input CSV");
    let mut rows: Vec<CalibrationRow> = Vec::new();
    for (i, line) in contents.lines().enumerate() {
        if i == 0 && line.starts_with("order_size_ticks") {
            continue; // skip header
        }
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() < 3 {
            continue;
        }
        let order_size_ticks: f64 = fields[0].trim().parse().unwrap_or(1.0);
        let vpin: f64 = fields[1].trim().parse().unwrap_or(0.0);
        let actual_impact_bps: f64 = fields[2].trim().parse().unwrap_or(0.0);
        rows.push(CalibrationRow { order_size_ticks, vpin, actual_impact_bps });
    }

    if rows.len() < 10 {
        panic!("Need at least 10 rows for calibration, got {}", rows.len());
    }

    // 20% hold-out
    let holdout_size = (rows.len() as f64 * 0.20) as usize;
    let holdout_size = holdout_size.max(1);
    let fit_rows = &rows[..rows.len() - holdout_size];
    let holdout_rows = &rows[rows.len() - holdout_size..];

    // Fit linear model
    let (size_coef, vpin_coef, linear_r2) = fit_linear_model(fit_rows);

    // Fit quadratic model
    let (_q0, _q1, _q2, quadratic_r2) = fit_quadratic_model(fit_rows);

    // Fit piecewise model
    let piecewise_r2 = fit_piecewise_model(fit_rows);

    // Non-linearity check: quadratic or piecewise gains > 0.02 R² over linear
    let non_linear_warning =
        (quadratic_r2 - linear_r2) > 0.02 || (piecewise_r2 - linear_r2) > 0.02;

    // Map to SlippageConfig fields
    // adverse_prob_factor maps to delay_multiplier sensitivity
    // vpin_impact_coef maps to vpin_impact_max
    let adverse_prob_factor = size_coef.clamp(1.0, 20.0);
    let vpin_impact_coef = vpin_coef.clamp(0.5, 10.0);

    // Hold-out validation
    let mut within_1bp = 0usize;
    for r in holdout_rows {
        let predicted = size_coef * (r.order_size_ticks.sqrt() / 100.0) + vpin_coef * r.vpin;
        if (predicted - r.actual_impact_bps).abs() <= 1.0 {
            within_1bp += 1;
        }
    }
    let holdout_accuracy_pct = (within_1bp as f64 / holdout_rows.len() as f64) * 100.0;
    let holdout_pass = holdout_accuracy_pct >= 80.0;

    // Print summary
    println!("=== VPIN Slippage Calibration ===");
    println!("Fitting rows: {}", fit_rows.len());
    println!("Hold-out rows: {}", holdout_rows.len());
    println!("Linear R²: {:.4}", linear_r2);
    println!("Quadratic R²: {:.4}", quadratic_r2);
    println!("Piecewise R²: {:.4}", piecewise_r2);
    if non_linear_warning {
        println!("WARNING: Non-linear model gains >0.02 R² over linear — relationship may be non-linear");
    }
    println!("Size coefficient (adverse_prob_factor proxy): {:.4}", adverse_prob_factor);
    println!("VPIN coefficient (vpin_impact_coef proxy): {:.4}", vpin_impact_coef);
    println!("Hold-out accuracy: {:.1}% ({}/{} within 1bp)", holdout_accuracy_pct, within_1bp, holdout_rows.len());
    if holdout_pass {
        println!("Hold-out PASS: {:.1}% >= 80% threshold", holdout_accuracy_pct);
    } else {
        println!("Hold-out FAIL: {:.1}% < 80% threshold", holdout_accuracy_pct);
    }

    // Build SlippageConfig with calibrated values
    let config = crate::slippage::SlippageConfig {
        delay_multiplier: adverse_prob_factor,
        delay_cap_ns: 200_000_000,
        size_impact_max: size_coef.clamp(5.0, 20.0),
        vpin_impact_max: vpin_impact_coef.clamp(2.0, 10.0),
        total_impact_max: 15.0,
    };

    let json = serde_json::to_string_pretty(&config).expect("Failed to serialize config");
    fs::write(output_path, json).expect("Failed to write output JSON");
    println!("Wrote calibrated SlippageConfig to {}", output_path);

    CalibrationResult {
        adverse_prob_factor,
        vpin_impact_coef,
        r_squared: linear_r2,
        linear_r2,
        quadratic_r2,
        piecewise_r2,
        non_linear_warning,
        holdout_pass,
        holdout_accuracy_pct,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calibration_row_fields() {
        let row = CalibrationRow {
            order_size_ticks: 100.0,
            vpin: 0.5,
            actual_impact_bps: 2.5,
        };
        assert!((row.order_size_ticks - 100.0).abs() < 0.001);
        assert!((row.vpin - 0.5).abs() < 0.001);
        assert!((row.actual_impact_bps - 2.5).abs() < 0.001);
    }
}
