//! VWAP Momentum — run on March 7-14, 2025.

use crate::backtest::BacktestEngine;
use nexus_types::InstrumentId;
use nexus_strategy::VwapMomentumStrategy;
use chrono::NaiveDate;

fn main() {
    let start_date = NaiveDate::from_ymd_opt(2025, 3, 7).unwrap();
    let end_date = NaiveDate::from_ymd_opt(2025, 3, 14).unwrap();
    let data_dir = std::path::PathBuf::from("/home/shadowarch/Nexus/data");

    let btc = InstrumentId::new("BTCUSDT", "BINANCE");

    println!("VWAP Momentum — March 7-14, 2025");
    println!("Data dir: {:?}\n", data_dir);

    let result = BacktestEngine::new()
        .with_instrument("BTCUSDT", "BINANCE").expect("invalid instrument")
        .with_date_range(start_date, end_date).expect("invalid date range")
        .with_data_dir(data_dir).expect("data dir not found")
        .with_initial_equity(100_000.0)
        .with_commission_bps(0.5)
        .run(|| VwapMomentumStrategy::with_instrument(btc.clone()))
        .expect("backtest failed");

    println!("\n=== Results ===");
    println!("PnL:           ${:.2}", result.pnl);
    println!("Max Drawdown:  ${:.2} ({:.2}%)", result.max_drawdown, result.max_drawdown_pct);
    println!("Trades:        {}", result.num_trades);
    println!("Win Rate:      {:.1}%", result.win_rate * 100.0);
    println!("Sharpe:        {:.3}", result.sharpe_ratio);
    println!("Duration:      {:.1}s", result.duration_secs);
}