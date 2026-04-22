//! Synthetic backtest benchmark — generates tick data and runs a simple strategy.
//!
//! Run with: cd /home/shadowarch/Nexus && cargo test --package nexus --test backtest_benchmark -- --nocapture

use nexus::buffer::TickBufferSet;
use nexus::engine::{CommissionConfig, Signal};
use nexus::instrument::InstrumentId;
use nexus::portfolio::{Portfolio, PortfolioConfig, PortfolioStrategy};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Simple momentum strategy: buy on uptick, sell on downtick.
struct MomentumStrategy {
    threshold: f64,
    last_price: f64,
    position_open: bool,
}

impl MomentumStrategy {
    fn new(threshold: f64) -> Self {
        Self {
            threshold,
            last_price: 0.0,
            position_open: false,
        }
    }

    fn reset(&mut self) {
        self.last_price = 0.0;
        self.position_open = false;
    }
}

impl Clone for MomentumStrategy {
    fn clone(&self) -> Self {
        Self::new(self.threshold)
    }
}

impl PortfolioStrategy for MomentumStrategy {
    fn on_trade(
        &mut self,
        _instrument_id: InstrumentId,
        _timestamp_ns: u64,
        price: f64,
        _size: f64,
        _portfolio: &mut Portfolio,
    ) -> Signal {
        if self.last_price == 0.0 {
            self.last_price = price;
            return Signal::Close;
        }

        let delta = (price - self.last_price) / self.last_price * 100.0;
        self.last_price = price;

        if delta > self.threshold {
            if !self.position_open {
                self.position_open = true;
                return Signal::Buy;
            }
        } else if delta < -self.threshold {
            if self.position_open {
                self.position_open = false;
                return Signal::Sell;
            }
        }

        Signal::Close
    }
}

/// Generate synthetic tick data in-memory and write to temp file.
fn generate_synthetic_ticks(n_ticks: usize) -> (std::path::PathBuf, InstrumentId) {
    use tvc::TvcWriter;
    use tvc::TradeTick;

    let path = std::path::PathBuf::from(format!("/tmp/bench_ticks_{}.tvc", std::process::id()));
    let instrument_id = InstrumentId::new("BTCUSDT", "BINANCE");

    let mut writer = TvcWriter::new(&path, 1u32, 10, 9).unwrap();

    let base_price = 50_000i64 * 1_000_000_000; // $50,000 in nanos
    let start_ts = 1_700_000_000_000_000_000u64; // Jan 2024
    let tick_interval = 1_000_000_000u64; // 1 second

    let mut price = base_price;
    let mut seq = 0u32;

    for i in 0..n_ticks {
        // Random walk: ±0.01% per tick
        let noise = ((i as i64 % 100) - 50) * 100_000_000; // ±50M nanos = ±$0.05
        price += noise;

        let tick = TradeTick::new(
            start_ts + (i as u64) * tick_interval,
            price,
            1_000_000_000i64, // 1 BTC
            (i % 2) as u8,     // buyer initiator
            1,                 // trade count
            seq,
        );
        writer.write_tick(&tick).unwrap();
        seq += 1;
    }

    writer.finalize().unwrap();
    (path, instrument_id)
}

fn run_benchmark(n_ticks: usize) -> BenchmarkResult {
    let (path, instrument_id) = generate_synthetic_ticks(n_ticks);

    // Load into TickBufferSet
    let buffer_set = TickBufferSet::from_files([(path.clone(), instrument_id.clone())])
        .expect("Failed to create buffer set from synthetic ticks");

    // Setup portfolio
    let config = PortfolioConfig::new(10_000.0, CommissionConfig::new(0.001));
    let mut portfolio = Portfolio::new(config.initial_equity_per_instrument);
    portfolio.register_instrument(instrument_id.clone());

    // Strategy
    let mut strategy = MomentumStrategy::new(0.005); // 0.005% threshold

    // Run benchmark
    let start = Instant::now();
    let mut cursor = buffer_set.merge_cursor();
    portfolio.run_portfolio::<MomentumStrategy>(&mut cursor, &config, || strategy.clone());
    let elapsed = start.elapsed();

    // Cleanup
    let _ = std::fs::remove_file(&path);

    BenchmarkResult {
        ticks_processed: n_ticks,
        duration_ns: elapsed.as nanos() as u64,
        final_equity: portfolio.portfolio_equity(),
        total_trades: portfolio.total_trades(),
        max_drawdown: portfolio.portfolio_max_drawdown(),
    }
}

#[derive(Debug)]
struct BenchmarkResult {
    ticks_processed: usize,
    duration_ns: u64,
    final_equity: f64,
    total_trades: usize,
    max_drawdown: f64,
}

impl BenchmarkResult {
    fn ticks_per_second(&self) -> f64 {
        (self.ticks_processed as f64) / (self.duration_ns as f64 / 1_000_000_000.0)
    }

    fn to_json(&self) -> String {
        format!(
            r#"{{"ticks_processed":{}, "duration_ns":{}, "duration_ms":{:.2}, "ticks_per_second":{:.0}, "final_equity":{:.2}, "total_trades":{}, "max_drawdown_pct":{:.2}}}"#,
            self.ticks_processed,
            self.duration_ns,
            self.duration_ns as f64 / 1_000_000.0,
            self.ticks_per_second(),
            self.final_equity,
            self.total_trades,
            self.max_drawdown
        )
    }
}

#[test]
fn test_backtest_1000_ticks() {
    let result = run_benchmark(1000);
    println!("\n=== 1000 ticks benchmark ===");
    println!("Duration: {:.2} ms", result.duration_ns as f64 / 1_000_000.0);
    println!("Throughput: {:.0} ticks/sec", result.ticks_per_second());
    println!("Final equity: ${:.2}", result.final_equity);
    println!("Trades: {}", result.total_trades);

    // Sanity check: should complete in reasonable time
    assert!(result.duration_ns < 1_000_000_000, "1000 ticks should complete in < 1 second");
    assert!(result.final_equity > 0.0, "Should have positive equity");
}

#[test]
fn test_backtest_10000_ticks() {
    let result = run_benchmark(10_000);
    println!("\n=== 10,000 ticks benchmark ===");
    println!("Duration: {:.2} ms", result.duration_ns as f64 / 1_000_000.0);
    println!("Throughput: {:.0} ticks/sec", result.ticks_per_second());
    println!("Final equity: ${:.2}", result.final_equity);
    println!("Trades: {}", result.total_trades);

    assert!(result.duration_ns < 5_000_000_000, "10k ticks should complete in < 5 seconds");
}

#[test]
fn test_backtest_100000_ticks() {
    let result = run_benchmark(100_000);
    println!("\n=== 100,000 ticks benchmark ===");
    println!("Duration: {:.2} ms", result.duration_ns as f64 / 1_000_000.0);
    println!("Throughput: {:.0} ticks/sec", result.ticks_per_second());
    println!("Final equity: ${:.2}", result.final_equity);
    println!("Trades: {}", result.total_trades);

    assert!(result.duration_ns < 30_000_000_000, "100k ticks should complete in < 30 seconds");
}