//! Baseline backtest — captures current behavior (no VPIN slippage) before fill simulation is wired.
//!
//! Run with: cargo test -p nexus --test baseline_backtest -- --nocapture

use std::io::Write;

use nexus::engine::{CommissionConfig, Signal};
use nexus::instrument::InstrumentId;
use nexus::portfolio::{Portfolio, PortfolioConfig, PortfolioStrategy};

mod mean_rev_strategy {
    use super::*;

    #[derive(Debug, Clone)]
    pub struct FillRecord {
        pub ts_ns: u64,
        pub price: f64,
        pub side: &'static str,
        pub pnl: f64,
    }

    #[derive(Clone)]
    pub struct MeanRevStrategy {
        threshold_bps: f64,
        position_size: f64,
        position_open: bool,
        last_price: f64,
        entry_price: f64,
        fills: Vec<FillRecord>,
    }

    impl MeanRevStrategy {
        pub fn new(threshold_bps: f64, position_size: f64) -> Self {
            Self {
                threshold_bps,
                position_size,
                position_open: false,
                last_price: 0.0,
                entry_price: 0.0,
                fills: Vec::new(),
            }
        }

        pub fn fill_summary(&self) -> FillSummary {
            let n = self.fills.len();
            let avg_slippage_bps = if n > 0 {
                self.fills.iter().map(|f| f.pnl.abs()).sum::<f64>() / n as f64 * 10000.0
            } else {
                0.0
            };
            FillSummary {
                num_fills: n,
                avg_slippage_bps,
                fills: self.fills.clone(),
            }
        }
    }

    #[derive(Debug)]
    pub struct FillSummary {
        pub num_fills: usize,
        pub avg_slippage_bps: f64,
        pub fills: Vec<FillRecord>,
    }

    impl PortfolioStrategy for MeanRevStrategy {
        fn on_trade(
            &mut self,
            instrument_id: InstrumentId,
            timestamp_ns: u64,
            price: f64,
            _size: f64,
            portfolio: &mut Portfolio,
        ) -> Signal {
            // Debug: track every tick
            eprintln!("[MeanRev] tick ts={} price={:.4}", timestamp_ns, price);

            if self.last_price == 0.0 {
                self.last_price = price;
                eprintln!("  -> first tick, initialized at {:.4}", price);
                return Signal::Close;
            }

            let change_bps = (price - self.last_price) / self.last_price * 10000.0;
            eprintln!("  change_bps={:.3} threshold={:.3} position_open={}", change_bps, self.threshold_bps, self.position_open);

            self.last_price = price;

            if !self.position_open && change_bps > self.threshold_bps {
                let comm = CommissionConfig::new(0.0001);
                portfolio.open_position(
                    &instrument_id,
                    price,
                    self.position_size,
                    Signal::Buy,
                    &comm,
                    None,
                    None,
                    None,
                );
                self.position_open = true;
                self.entry_price = price;
                eprintln!("  *** BUY SIGNAL ***");
                Signal::Buy
            } else if self.position_open && change_bps < -self.threshold_bps {
                let comm = CommissionConfig::new(0.0001);
                let pnl = portfolio.close_position(&instrument_id, price, &comm, timestamp_ns);

                self.fills.push(FillRecord {
                    ts_ns: timestamp_ns,
                    price,
                    side: if pnl >= 0.0 { "WIN" } else { "LOSS" },
                    pnl,
                });

                self.position_open = false;
                self.entry_price = 0.0;
                eprintln!("  *** SELL SIGNAL *** pnl={:.2}", pnl);
                Signal::Sell
            } else {
                Signal::Close
            }
        }
    }
}

use mean_rev_strategy::{FillSummary, MeanRevStrategy};

fn load_buffer_set() -> Option<nexus::buffer::TickBufferSet> {
    let data_dir = std::path::PathBuf::from("/home/shadowarch/Nexus/data");
    let instrument = InstrumentId::new("BTCUSDT", "BINANCE");

    // 2025-03-10 is the only valid TVC3 file (written after tvc_builder double-open fix).
    // 07/08/09 are corrupted (pre-fix), 11/12 have a separate write issue.
    let files = vec![
        (data_dir.join("binance/spot/BTCUSDT/2025-03-10.tvc"), instrument.clone()),
    ];

    let valid: Vec<_> = files.into_iter().filter(|(p, _)| p.exists()).collect();

    eprintln!("DEBUG: valid files count: {}", valid.len());
    for (i, (p, _)) in valid.iter().enumerate() {
        eprintln!("DEBUG: valid[{}] = {:?}", i, p);
    }

    if valid.is_empty() {
        return None;
    }

    match nexus::buffer::TickBufferSet::from_files(valid) {
        Ok(bs) => {
            eprintln!("DEBUG: TickBufferSet loaded, total_ticks={}", bs.total_ticks());
            Some(bs)
        }
        Err(e) => {
            eprintln!("DEBUG: TickBufferSet::from_files error: {:?}", e);
            None
        }
    }
}

fn run_baseline() -> BaselineResult {
    let buffer_set = match load_buffer_set() {
        Some(bs) => bs,
        None => {
            eprintln!("WARNING: No TVC data found. Running synthetic test instead.");
            return run_synthetic_baseline();
        }
    };

    let instrument = InstrumentId::new("BTCUSDT", "BINANCE");
    let commission = CommissionConfig::new(0.0001);
    let config = PortfolioConfig::new(100_000.0, commission);
    let mut portfolio = Portfolio::new(100_000.0);
    portfolio.register_instrument(instrument.clone());

    let mut strategy = MeanRevStrategy::new(2.0, 0.1);

    let start = std::time::Instant::now();
    let mut cursor = buffer_set.merge_cursor();
    portfolio.run_portfolio::<MeanRevStrategy>(&mut cursor, &config, || strategy.clone());
    let elapsed = start.elapsed();

    let fill_summary = strategy.fill_summary();

    BaselineResult {
        mode: "real_tvc".to_string(),
        pnl: portfolio.portfolio_equity() - 100_000.0,
        final_equity: portfolio.portfolio_equity(),
        max_drawdown: portfolio.portfolio_max_drawdown(),
        num_trades: portfolio.total_trades(),
        total_wins: portfolio.total_wins(),
        total_losses: portfolio.total_losses(),
        sharpe: compute_sharpe(&portfolio.returns()),
        duration_ms: elapsed.as_secs_f64() * 1000.0,
        ticks_processed: buffer_set.total_ticks(),
        fill_summary,
    }
}

fn run_synthetic_baseline() -> BaselineResult {
    use tvc::{TradeTick, TvcWriter};

    let path = std::path::PathBuf::from("/tmp/baseline_ticks.tvc");
    let instrument = InstrumentId::new("BTCUSDT", "BINANCE");

    let mut writer = TvcWriter::new(&path, 1u32, 10, 9).unwrap();
    let base_price = 50_000i64 * 1_000_000_000;
    let start_ts = 1_700_000_000_000_000_000u64;

    let mut price = base_price;
    let mut seq = 0u32;

    for i in 0..50_000 {
        let delta = if i % 2 == 0 {
            (i as i64 % 20) * 100_000_000
        } else {
            -(i as i64 % 20) * 100_000_000
        };
        price += delta;

        let tick = TradeTick::new(
            start_ts + (i as u64) * 1_000_000_000,
            price,
            1_000_000_000i64,
            (i % 2) as u8,
            1,
            seq,
        );
        writer.write_tick(&tick).unwrap();
        seq += 1;
    }
    writer.finalize().unwrap();

    let buffer_set =
        nexus::buffer::TickBufferSet::from_files([(path.clone(), instrument.clone())])
            .expect("failed to load synthetic buffer");

    let commission = CommissionConfig::new(0.0001);
    let config = PortfolioConfig::new(100_000.0, commission);
    let mut portfolio = Portfolio::new(100_000.0);
    portfolio.register_instrument(instrument.clone());

    let mut strategy = MeanRevStrategy::new(2.0, 0.1);

    let start = std::time::Instant::now();
    let mut cursor = buffer_set.merge_cursor();
    portfolio.run_portfolio::<MeanRevStrategy>(&mut cursor, &config, || strategy.clone());
    let elapsed = start.elapsed();

    let fill_summary = strategy.fill_summary();

    let _ = std::fs::remove_file(&path);

    BaselineResult {
        mode: "synthetic".to_string(),
        pnl: portfolio.portfolio_equity() - 100_000.0,
        final_equity: portfolio.portfolio_equity(),
        max_drawdown: portfolio.portfolio_max_drawdown(),
        num_trades: portfolio.total_trades(),
        total_wins: portfolio.total_wins(),
        total_losses: portfolio.total_losses(),
        sharpe: compute_sharpe(&portfolio.returns()),
        duration_ms: elapsed.as_secs_f64() * 1000.0,
        ticks_processed: 50_000,
        fill_summary,
    }
}

fn compute_sharpe(returns: &[f64]) -> f64 {
    if returns.is_empty() {
        return 0.0;
    }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
    let std_dev = variance.sqrt();
    if std_dev == 0.0 {
        return 0.0;
    }
    let sharpe = mean / std_dev * (252.0_f64.sqrt());
    if !sharpe.is_finite() {
        0.0
    } else {
        sharpe
    }
}

#[derive(Debug)]
struct BaselineResult {
    mode: String,
    pnl: f64,
    final_equity: f64,
    max_drawdown: f64,
    num_trades: usize,
    total_wins: usize,
    total_losses: usize,
    sharpe: f64,
    duration_ms: f64,
    ticks_processed: u64,
    fill_summary: FillSummary,
}

impl BaselineResult {
    fn print(&self) {
        println!("\n=== BASELINE BACKTEST ({} mode) ===", self.mode);
        println!("PnL:              ${:.2}", self.pnl);
        println!("Final Equity:     ${:.2}", self.final_equity);
        println!("Max Drawdown:     ${:.2}", self.max_drawdown);
        println!("Trades:           {}", self.num_trades);
        println!("  Wins:           {}", self.total_wins);
        println!("  Losses:         {}", self.total_losses);
        if self.total_wins + self.total_losses > 0 {
            let wr = self.total_wins as f64 / (self.total_wins + self.total_losses) as f64 * 100.0;
            println!("Win Rate:         {:.1}%", wr);
        }
        println!("Sharpe:           {:.3}", self.sharpe);
        println!("Duration:         {:.1} ms", self.duration_ms);
        println!("Ticks Processed: {}", self.ticks_processed);
        println!(
            "Avg Slippage:     {:.2} bps",
            self.fill_summary.avg_slippage_bps
        );
        println!("\nFill Details (first 10):");
        for (i, fill) in self.fill_summary.fills.iter().enumerate().take(10) {
            println!(
                "  fill[{}] ts={} price={:.4} {} pnl={:.2}",
                i, fill.ts_ns, fill.price, fill.side, fill.pnl
            );
        }
        if self.fill_summary.fills.len() > 10 {
            println!("  ... ({} more fills)", self.fill_summary.fills.len() - 10);
        }
    }
}

#[test]
fn test_baseline_orbs_backtest() {
    let result = run_baseline();
    result.print();

    assert!(result.ticks_processed > 0, "should process ticks");
    assert!(result.duration_ms > 0.0, "should measure duration");

    let json = serde_json::json!({
        "mode": result.mode,
        "pnl": result.pnl,
        "final_equity": result.final_equity,
        "max_drawdown": result.max_drawdown,
        "num_trades": result.num_trades,
        "total_wins": result.total_wins,
        "total_losses": result.total_losses,
        "sharpe": result.sharpe,
        "duration_ms": result.duration_ms,
        "ticks_processed": result.ticks_processed,
        "avg_slippage_bps": result.fill_summary.avg_slippage_bps,
        "num_fills": result.fill_summary.num_fills,
    });

    let out_path = std::path::PathBuf::from("/tmp/nexus_baseline_results.json");
    let mut f = std::fs::File::create(&out_path).unwrap();
    let _ = f.write_all(json.to_string().as_bytes());
    println!("\nResults saved to: {:?}", out_path);
}