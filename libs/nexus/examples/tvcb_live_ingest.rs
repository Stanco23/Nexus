//! End-to-end live ingestion test across Binance, Bybit, and OKX (Spot).
//!
//! Run with: cargo run --example tvcb_live_ingest -p nexus --release

use std::path::PathBuf;
use std::time::Duration;
use chrono::NaiveDate;

use crate::data_manager::{DataManager, Exchange, InstrumentType};
use crate::backtest::engine::BacktestEngine;
use nexus_types::Bar;
use nexus_strategy::{Strategy, StrategyCtx};

struct SimpleStrategy { position_opened: bool }
impl SimpleStrategy { fn new() -> Self { Self { position_opened: false } } }
impl Strategy for SimpleStrategy {
    fn name(&self) -> &str { "SimpleStrategy" }
    fn mode(&self) -> nexus_types::BacktestMode { nexus_types::BacktestMode::Bar }
    fn subscribed_instruments(&self) -> Vec<nexus_types::InstrumentId> { vec![] }
    fn parameters(&self) -> Vec<nexus_strategy::ParameterSchema> { vec![] }
    fn clone_box(&self) -> Box<dyn Strategy> { Box::new(Self::new()) }
    fn timeframe(&self) -> Option<Duration> { Some(Duration::from_secs(900)) }
    fn on_bar(&mut self, _id: nexus_types::InstrumentId, _bar: &Bar, _ctx: &mut dyn StrategyCtx) -> Option<nexus_strategy::Signal> {
        if !self.position_opened { self.position_opened = true; Some(nexus_strategy::Signal::Buy) } else { None }
    }
    fn on_trade(&mut self, _: nexus_types::InstrumentId, _: &nexus_strategy::Tick, _: &mut dyn StrategyCtx) -> Option<nexus_strategy::Signal> { None }
}

fn run_exchange(
    output_dir: &PathBuf,
    exchange: Exchange,
    itype: InstrumentType,
    symbol: &str,
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let start = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
    let end   = NaiveDate::from_ymd_opt(2024, 1, 31).unwrap();
    let tf = "15m";

    println!("\n----------------------------------------");
    println!("  {} — {} {} [{} → {}]", label, symbol, tf, start, end);
    println!("----------------------------------------");

    let dm = DataManager::new(output_dir.clone())?;

    // ── 1. Ingest with debug ───────────────────────────────────────────────
    let t0 = std::time::Instant::now();
    let paths: Vec<PathBuf> = {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        match rt.block_on(async { dm.ingest_bars(exchange, itype, symbol, tf, start, end).await }) {
            Ok(p) => {
                if p.is_empty() { eprintln!("  [{}] ingest_bars returned 0 paths (no data or all years failed)", label); }
                p
            }
            Err(e) => {
                eprintln!("  [{}] ingest_bars Err: {}", label, e);
                return Ok(());
            }
        }
    };
    let ingest_secs = t0.elapsed().as_secs_f64();

    if paths.is_empty() {
        println!("  [{}] WARNING: no files created — check stderr for details", label);
        return Ok(());
    }

    let total_bars: usize = paths.iter().map(|p| {
        let data = std::fs::read(p).unwrap();
        let header = tvc::tvcb::types::bytes_to_header(&data[..128].try_into().unwrap());
        header.num_bars as usize
    }).sum();

    println!("  [{}] Ingested {} bars in {:.1}s", label, total_bars, ingest_secs);

    // ── 2. Load bars ────────────────────────────────────────────────────────
    let bars = dm.load_bars(exchange, itype, symbol, tf, start, end)?;
    let bar_count = bars.filter_map(|r| r.ok()).count();
    println!("  [{}] BarIter yielded {} bars", label, bar_count);

    // ── 3. Backtest ─────────────────────────────────────────────────────────
    let bars2 = dm.load_bars(exchange, itype, symbol, tf, start, end)?;
    let result = BacktestEngine::new()
        .with_instrument(symbol, &exchange.as_str().to_uppercase())?
        .with_instrument_type(itype)
        .with_timeframe(tf)
        .with_data_dir(output_dir.clone())?
        .with_date_range(start, end)?
        .with_initial_equity(100_000.0)
        .run(|| Box::new(SimpleStrategy::new()) as Box<dyn Strategy>)?;

    println!("  [{}] Backtest — PnL: ${:.2}, Trades: {}", label, result.pnl, result.num_trades);
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base_dir = PathBuf::from("/tmp/tvcb_live_test");
    let symbol = "BTCUSDT";

    println!("\n=== TVCB Live Ingestion — 3 Exchanges ===");
    println!("Symbol: {} | Timeframe: 15m | Period: Jan 2024", symbol);

    let _ = std::fs::remove_dir_all(&base_dir);

    for (exchange, itype, label) in [
        (Exchange::Binance, InstrumentType::Spot, "Binance Spot"),
        (Exchange::Bybit,   InstrumentType::Spot, "Bybit Spot"),
        (Exchange::Okx,     InstrumentType::Spot, "OKX Spot"),
    ] {
        if let Err(e) = run_exchange(&base_dir, exchange, itype, symbol, label) {
            eprintln!("  {} failed: {}", label, e);
        }
    }

    println!("\n=== Done ===");
    Ok(())
}