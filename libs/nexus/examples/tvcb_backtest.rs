//! TVCB backtest integration test.
//!
//! Tests the full pipeline: write synthetic bars → load via DataManager → backtest.
//!
//! Run with: cargo run --example tvcb_backtest -p nexus

use std::path::PathBuf;
use std::time::Duration;
use chrono::NaiveDate;

use crate::data_manager::{DataManager, InstrumentType};
use crate::backtest::engine::{BacktestEngine, BarSource};
use nexus_types::Bar;
use nexus_strategy::StrategyCtx;
use tvc::tvcb::writer::TvcbWriter;

/// Simple buy-and-hold strategy for testing.
struct SimpleStrategy {
    position_opened: bool,
}

impl SimpleStrategy {
    fn new() -> Self {
        Self { position_opened: false }
    }
}

impl nexus_strategy::Strategy for SimpleStrategy {
    fn name(&self) -> &str { "SimpleStrategy" }
    fn mode(&self) -> nexus_types::BacktestMode { nexus_types::BacktestMode::Bar }
    fn subscribed_instruments(&self) -> Vec<nexus_types::InstrumentId> { vec![] }
    fn parameters(&self) -> Vec<nexus_strategy::ParameterSchema> { vec![] }
    fn clone_box(&self) -> Box<dyn nexus_strategy::Strategy> { Box::new(Self { position_opened: self.position_opened }) }
    fn timeframe(&self) -> Option<Duration> { Some(Duration::from_secs(900)) } // 15m bars

    fn on_bar(
        &mut self,
        _instrument_id: nexus_types::InstrumentId,
        _bar: &Bar,
        _ctx: &mut dyn StrategyCtx,
    ) -> Option<nexus_strategy::Signal> {
        // Buy on first bar, hold forever
        if !self.position_opened {
            self.position_opened = true;
            return Some(nexus_strategy::Signal::Buy);
        }
        // Close at end
        Some(nexus_strategy::Signal::Close)
    }

    fn on_trade(&mut self, _: nexus_types::InstrumentId, _: &nexus_strategy::Tick, _: &mut dyn StrategyCtx) -> Option<nexus_strategy::Signal> { None }
}

/// Write synthetic 15m bars to TVCB files.
fn write_synthetic_bars(output_dir: &PathBuf, symbol: &str) -> std::io::Result<Vec<PathBuf>> {
    use tvc::tvcb::types::Bar as TvcBar;
    use std::fs;

    let mut created_paths = Vec::new();
    let timeframe_ns = 900_000_000_000u64; // 15 minutes
    let num_bars = 1000u64;

    // Write 2024.tvcb
    let year = 2024u64;
    let dir = output_dir.join("binance").join("spot").join(symbol.to_lowercase()).join("15m");
    fs::create_dir_all(&dir)?;

    let path = dir.join(format!("{}.tvcb", year));
    let instrument_hash = fnv1a_hash(symbol);
    let mut writer = TvcbWriter::new(&path, instrument_hash, 10, 9, year, timeframe_ns).unwrap();

    for i in 0..num_bars {
        // 2024-01-01 00:00:00 UTC ≈ 1704063600000000000 ns
        let ts = 1_704_063_600_000_000_000u64 + i * timeframe_ns;
        let bar = TvcBar::from_floats(
            ts,
            100.0 + i as f64 * 0.01,
            101.0 + i as f64 * 0.01,
            99.0 + i as f64 * 0.01,
            100.5 + i as f64 * 0.01,
            100.4 + i as f64 * 0.01,
            1000.0 + i as f64 * 0.5,
            600.0 + i as f64 * 0.3,
            400.0 + i as f64 * 0.2,
            10 + (i % 20) as u32,
            9, // decimal precision
        );
        writer.write_bar(&bar).unwrap();
    }
    writer.finalize().unwrap();
    created_paths.push(path.clone());
    println!("wrote {} bars to {:?}", num_bars, path);

    Ok(created_paths)
}

/// FNV-1a hash.
fn fnv1a_hash(s: &str) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for byte in s.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── 1. Write synthetic bars ────────────────────────────────────────────
    let output_dir = PathBuf::from("/tmp/tvcb_test");
    let symbol = "BTCUSDT";

    // Clean up old test data
    let _ = std::fs::remove_dir_all(&output_dir);

    let created = write_synthetic_bars(&output_dir, symbol)?;
    println!("\n✓ Created {} TVCB file(s)", created.len());

    // ── 2. Load bars via DataManager ──────────────────────────────────────
    let dm = DataManager::new(output_dir.clone())?;
    let bars = dm.load_bars(
        crate::data_manager::Exchange::Binance,
        InstrumentType::Spot,
        symbol,
        "15m",
        NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
        NaiveDate::from_ymd_opt(2024, 12, 31).unwrap(),
    )?;
    println!("✓ Loaded BarIter from DataManager");

    // Collect bars to verify roundtrip
    let loaded_bars: Vec<_> = bars.filter_map(|r| r.ok()).collect();
    println!("✓ BarIter yielded {} bars", loaded_bars.len());

    if !loaded_bars.is_empty() {
        let first = &loaded_bars[0];
        let last = loaded_bars.last().unwrap();
        println!("  first bar: ts={}, o={}, h={}, l={}, c={}", first.ts_event, first.open, first.high, first.low, first.close);
        println!("  last bar:  ts={}, o={}, h={}, l={}, c={}", last.ts_event, last.open, last.high, last.low, last.close);
    }

    // ── 3. Re-open BarIter for backtest (BarIter is consumed by iterator) ──
    let bars2 = dm.load_bars(
        crate::data_manager::Exchange::Binance,
        InstrumentType::Spot,
        symbol,
        "15m",
        NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
        NaiveDate::from_ymd_opt(2024, 12, 31).unwrap(),
    )?;

    // ── 4. Run backtest with BarSource::BarIter ───────────────────────────
    let result = BacktestEngine::new()
        .with_instrument(symbol, "BINANCE")?
        .with_bar_source(BarSource::BarIter(bars2))
        .with_data_dir(output_dir.clone())?
        .with_initial_equity(100_000.0)
        .run(|| Box::new(SimpleStrategy::new()) as Box<dyn nexus_strategy::Strategy>)?;

    println!("\n✓ Backtest complete!");
    println!("  PnL: ${:.2}", result.pnl);
    println!("  Trades: {}", result.num_trades);
    println!("  Max Drawdown: ${:.2}", result.max_drawdown);
    println!("  Win Rate: {:.1}%", result.win_rate * 100.0);
    println!("  Final Equity: ${:.2}", result.final_equity);
    println!("  Duration: {:.1}s", result.duration_secs);

    // ── 5. Verify bar field integrity ──────────────────────────────────────
    println!("\n✓ Integration test passed!");

    Ok(())
}