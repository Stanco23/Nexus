//! Parameter sweep example — SMA Cross Trailing strategy over 3x3 parameter grid.
//!
//! Runs 9 combinations (3 fast × 3 slow periods) in parallel via Rayon.
//! Data auto-ingests via DataManager if missing.

use crate::backtest::BacktestEngine;
use crate::sweep::ParameterGrid;
use nexus_strategy::{InstrumentId, SmaCrossTrailingStrategy};
use chrono::NaiveDate;

fn main() {
    let start_date = NaiveDate::from_ymd_opt(2025, 3, 7).unwrap();
    let end_date = NaiveDate::from_ymd_opt(2025, 3, 14).unwrap();
    let data_dir = std::path::PathBuf::from("/home/shadowarch/Nexus/data");

    // Define parameter grid
    let grid = ParameterGrid::new()
        .add_param("fast_period", vec![10.0, 20.0, 30.0])
        .add_param("slow_period", vec![40.0, 50.0, 60.0]);

    println!("SMA Cross Trailing — Parameter Sweep");
    println!("Grid: {} combinations ({} parallel workers)",
             grid.num_combinations(), rayon::current_num_threads());

    let results = BacktestEngine::new()
        .with_instrument("BTCUSDT", "BINANCE")
        .expect("invalid instrument")
        .with_date_range(start_date, end_date)
        .expect("invalid date range")
        .with_data_dir(data_dir)
        .expect("data dir not found")
        .with_initial_equity(100_000.0)
        .with_commission_bps(0.5)
        .run_sweep(&grid, |params| {
            let fast = params.get("fast_period").copied().unwrap_or(20.0) as usize;
            let slow = params.get("slow_period").copied().unwrap_or(50.0) as usize;
            SmaCrossTrailingStrategy::new(fast, slow, 1.0, 0.01, 0.02)
        })
        .expect("sweep failed");

    // Sort by PnL descending
    let mut sorted = results.clone();
    sorted.sort_by(|a, b| b.pnl.partial_cmp(&a.pnl).unwrap_or(std::cmp::Ordering::Equal));

    println!("\n=== Top 5 Results ===");
    for (i, r) in sorted.iter().take(5).enumerate() {
        println!("{}. fast={:.0} slow={:.0} | PnL: {:>12.2} | DD: {:>10.2} | WR: {:>5.1}% | Trades: {} | Sharpe: {:.3}",
            i + 1,
            r.params.get("fast_period").unwrap_or(&0.0),
            r.params.get("slow_period").unwrap_or(&0.0),
            r.pnl,
            r.max_drawdown,
            r.win_rate * 100.0,
            r.num_trades,
            r.sharpe,
        );
    }

    println!("\n=== Summary ===");
    let best = sorted.first().unwrap();
    let worst = sorted.last().unwrap();
    println!("Best:  fast={:.0} slow={:.0} → PnL ${:.2}", best.params.get("fast_period").unwrap_or(&0.0), best.params.get("slow_period").unwrap_or(&0.0), best.pnl);
    println!("Worst: fast={:.0} slow={:.0} → PnL ${:.2}", worst.params.get("fast_period").unwrap_or(&0.0), worst.params.get("slow_period").unwrap_or(&0.0), worst.pnl);
}