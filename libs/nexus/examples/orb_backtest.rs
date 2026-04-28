//! ORB Backtest Runner — Nexus Rust
//! ==================================
//! Standalone binary. Run with:
//!   cargo run -p nexus --example orb_backtest -- --data ./data --output results_orb_nexus.csv
//!
//! Data: TVC files in --data directory (one per instrument).
//! Output: CSV with date,instrument,entry_price,exit_price,side,pnl,trade_count

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use chrono::TimeZone;
use chrono::NaiveDate;
use chrono::Timelike;
use nexus::buffer::buffer_set::TickBufferSet;
use nexus::engine::core::{Signal, PositionSide};
use nexus::engine::CommissionConfig;
use nexus::instrument::InstrumentId;
use nexus::portfolio::{Portfolio, PortfolioConfig, PortfolioStrategy};
use nexus::signals::SignalBus;

// ─────────────────────────────────────────────────────────────────────────────
// ORB Config
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct OrbConfig {
    pub instrument_id: String,
    /// Trading date (YYYY-MM-DD).
    pub trading_date: String,
    /// Parsed trading date for validation.
    pub trading_naive_date: NaiveDate,
    pub opening_window_minutes: f64,
    pub opening_start_hour: f64,
    pub session_end_hour: f64,
    pub atr_multiplier: f64,
    pub position_size: f64,
    pub tick_size: f64,
}

impl OrbConfig {
    fn from_date(trading_date: &str) -> Self {
        let naive_date = NaiveDate::parse_from_str(trading_date, "%Y-%m-%d")
            .expect("invalid trading_date format YYYY-MM-DD");
        Self {
            instrument_id: "BTCUSDT.BINANCE".to_string(),
            trading_date: trading_date.to_string(),
            trading_naive_date: naive_date,
            opening_window_minutes: 5.0,
            opening_start_hour: 9.5,
            session_end_hour: 16.0,
            atr_multiplier: 1.5,
            position_size: 1.0,
            tick_size: 0.01,
        }
    }
}

impl Default for OrbConfig {
    fn default() -> Self {
        Self::from_date("2025-01-09")
    }
}

impl OrbConfig {
    fn opening_end_hour(&self) -> f64 {
        self.opening_start_hour + self.opening_window_minutes / 60.0
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ORB Strategy
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct OrbStrategy {
    config: OrbConfig,
    orb_high: Option<f64>,
    orb_low: Option<f64>,
    orb_armed: bool,
    position_open: bool,
    entry_price: Option<f64>,
    stop_price: Option<f64>,
    take_profit: Option<f64>,
    position_side: PositionSide,
}

impl OrbStrategy {
    fn new(config: OrbConfig) -> Self {
        Self {
            config,
            orb_high: None,
            orb_low: None,
            orb_armed: false,
            position_open: false,
            entry_price: None,
            stop_price: None,
            take_profit: None,
            position_side: PositionSide::Flat,
        }
    }

    /// Converts unix_ns to EST (UTC-5) broken-down datetime.
    /// Uses FixedOffset to avoid DST complications (EST does not observe DST).
    fn to_est_datetime(unix_ns: u64) -> chrono::DateTime<chrono::FixedOffset> {
        // EST = UTC - 5 hours, no DST
        let est_offset = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
        let utc_dt = chrono::DateTime::from_timestamp(unix_ns as i64 / 1_000_000_000, 0).unwrap();
        est_offset.from_utc_datetime(&utc_dt.naive_utc())
    }

    /// Returns EST hour-of-day (0-24) for a timestamp.
    /// Fixes the % 86400.0 modulo bug by not stripping the calendar date.
    fn est_hour_of_day(unix_ns: u64) -> f64 {
        let dt = Self::to_est_datetime(unix_ns);
        dt.hour() as f64 + dt.minute() as f64 / 60.0 + dt.second() as f64 / 3600.0
    }

    fn round_to_tick(&self, price: f64) -> f64 {
        (price / self.config.tick_size).round() * self.config.tick_size
    }

    fn position_size(&self) -> f64 {
        self.config.position_size
    }

    fn close_position(&mut self) {
        self.position_open = false;
        self.entry_price = None;
        self.stop_price = None;
        self.take_profit = None;
        self.position_side = PositionSide::Flat;
    }
}

impl PortfolioStrategy for OrbStrategy {
    fn on_trade(
        &mut self,
        instrument_id: InstrumentId,
        timestamp_ns: u64,
        price: f64,
        _size: f64,
        _portfolio: &mut Portfolio,
    ) -> Signal {
        let expected_id = InstrumentId::parse(&self.config.instrument_id).expect("invalid instrument ID");
        if instrument_id != expected_id {
            return Signal::Close;
        }

        // Validate tick is on the correct trading date (in EST).
        // This fixes the % 86400.0 modulo bug which was stripping calendar dates
        // and causing ORB to apply across ALL dates in the data.
        let est_dt = Self::to_est_datetime(timestamp_ns);
        if est_dt.date_naive() != self.config.trading_naive_date {
            // Tick is not on the trading date — reset state and return close
            self.close_position();
            return Signal::Close;
        }

        let hour_of_day = Self::est_hour_of_day(timestamp_ns);
        let opening_end = self.config.opening_end_hour();

        // Phase 1: Build opening range
        if !self.orb_armed && hour_of_day < opening_end {
            if self.orb_high.is_none() || price > self.orb_high.unwrap() {
                self.orb_high = Some(price);
            }
            if self.orb_low.is_none() || price < self.orb_low.unwrap() {
                self.orb_low = Some(price);
            }
            return Signal::Close;
        }

        // Phase 2: Arm ORB when window closes
        if !self.orb_armed && hour_of_day >= opening_end {
            if self.orb_high.is_some() && self.orb_low.is_some() {
                self.orb_armed = true;
            }
            return Signal::Close;
        }

        // Phase 3: Only trade during regular session
        if hour_of_day < opening_end || hour_of_day >= self.config.session_end_hour {
            return Signal::Close;
        }

        // Phase 4: Entry
        if !self.position_open {
            let range_width = self.orb_high.unwrap() - self.orb_low.unwrap();
            let tick = self.config.tick_size;

            if price > self.orb_high.unwrap() {
                self.entry_price = Some(price);
                self.stop_price = Some(self.orb_low.unwrap() - tick);
                self.take_profit = Some(self.round_to_tick(price + range_width * self.config.atr_multiplier));
                self.position_open = true;
                self.position_side = PositionSide::Long;
                return Signal::Buy;
            }
            if price < self.orb_low.unwrap() {
                self.entry_price = Some(price);
                self.stop_price = Some(self.orb_high.unwrap() + tick);
                self.take_profit = Some(self.round_to_tick(price - range_width * self.config.atr_multiplier));
                self.position_open = true;
                self.position_side = PositionSide::Short;
                return Signal::Sell;
            }
            return Signal::Close;
        }

        // Phase 5: Exit
        if self.position_open {
            let Some(_entry) = self.entry_price else {
                return Signal::Close;
            };

            let direction = if self.position_side == PositionSide::Long { 1.0 } else { -1.0 };
            let pnl_ticks = direction * (price - _entry) / self.config.tick_size;

            // Stop loss — 1 tick
            if pnl_ticks <= -1.0 {
                self.close_position();
                return Signal::Close;
            }

            // Take profit — 1.5× range width
            let tp_ticks = (self.take_profit.unwrap() - _entry) / self.config.tick_size * direction;
            if pnl_ticks >= tp_ticks {
                self.close_position();
                return Signal::Close;
            }
        }

        Signal::Close
    }

    fn subscribe_signal(&mut self, _signal_bus: Arc<SignalBus>) {}
}

// ─────────────────────────────────────────────────────────────────────────────
// Main
// ─────────────────────────────────────────────────────────────────────────────

fn main() {
    let start = Instant::now();

    // Parse args
    let args: Vec<String> = std::env::args().collect();
    let data_dir = args
        .iter()
        .position(|a| a == "--data")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./data"));

    let output_path = args
        .iter()
        .position(|a| a == "--output")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "results_orb_nexus.csv".to_string());

    let instrument_id = args
        .iter()
        .position(|a| a == "--instrument")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "BTCUSDT.BINANCE".to_string());

    // Trading date: try to extract from first matching TVC3 filename (e.g. BTCUSDT_2025-01-01.tvc),
    // fall back to --date CLI arg, then to default "2025-01-09".
    let trading_date = std::fs::read_dir(&data_dir)
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map_or(false, |e| e == "tvc"))
                .find(|p| {
                    let stem = p.file_stem().unwrap_or_default().to_string_lossy();
                    let sym = instrument_id.split('.').next().unwrap_or(&instrument_id);
                    stem == sym || stem.starts_with(&format!("{}_", sym))
                })
        })
        .and_then(|p| {
            let stem = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let sym = instrument_id.split('.').next().unwrap_or(&instrument_id);
            // Extract date suffix: "BTCUSDT_2025-01-01" → "2025-01-01"
            if stem.starts_with(&format!("{}_", sym)) {
                Some(stem[sym.len() + 1..].to_string())
            } else {
                None
            }
        })
        .or_else(|| {
            args.iter()
                .position(|a| a == "--date")
                .and_then(|i| args.get(i + 1).cloned())
        })
        .unwrap_or_else(|| "2025-01-09".to_string());

    println!("=== ORB Backtest — Nexus ===");
    println!("Data dir: {:?}", data_dir);
    println!("Output: {}", output_path);
    println!("Instrument: {}", instrument_id);
    println!("Trading date: {}", trading_date);

    // Load TVC files — accepts both "BTCUSDT.tvc" and "BTCUSDT_2025-01-09.tvc"
    let symbol = instrument_id.split('.').next().unwrap_or(&instrument_id);
    let files: Vec<(std::path::PathBuf, InstrumentId)> = std::fs::read_dir(&data_dir)
        .expect("Cannot read data directory")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |e| e == "tvc"))
        .filter(|p| {
            let stem = p.file_stem().unwrap_or_default().to_string_lossy();
            // Accept "BTCUSDT.tvc" or "BTCUSDT_2025-01-09.tvc" (date-stamped TVC3)
            stem == symbol || stem.starts_with(&format!("{}_", symbol))
        })
        .map(|p| {
            let stem = p.file_stem().unwrap().to_string_lossy().to_string();
            // Handle both "BTCUSDT.tvc" and "BTCUSDT_2025-01-09.tvc" → symbol = "BTCUSDT"
            let inst_symbol = if stem.starts_with(&format!("{}_", symbol)) {
                &stem[..symbol.len()]
            } else {
                &stem
            };
            let inst_id = InstrumentId::new(inst_symbol, "BINANCE");
            (p, inst_id)
        })
        .collect();

    let buffer_set = TickBufferSet::from_files(files).expect("Failed to load TVC files");
    println!(
        "Loaded {} instruments, {} total ticks",
        buffer_set.num_instruments(),
        buffer_set.total_ticks()
    );

    // Build portfolio
    // ORB is a signal-based market-order strategy — disable fill engines so the
    // market-order path handles position updates directly (no double execution).
    let config = PortfolioConfig::new(100_000.0, CommissionConfig::new(0.0004))
        .with_fill_engine_disabled();
    let mut portfolio = Portfolio::new(config.initial_equity_per_instrument);

    for id in buffer_set.instrument_ids() {
        portfolio.register_instrument(id.clone());
    }

    // Run ORB strategy
    let mut orb_config = OrbConfig::from_date(&trading_date);
    orb_config.instrument_id = instrument_id.clone();
    let strategy = OrbStrategy::new(orb_config);

    let mut cursor = buffer_set.merge_cursor();
    portfolio.run_portfolio::<OrbStrategy>(&mut cursor, &config, || strategy.clone());

    // Collect results
    let pnl = portfolio.portfolio_equity() - config.initial_equity_per_instrument;
    let max_dd = portfolio.portfolio_max_drawdown();
    let num_trades = portfolio.total_trades();

    println!("\n=== Results ===");
    println!("Total PnL:       ${:.2}", pnl);
    println!("Max Drawdown:     ${:.2}", max_dd);
    println!("Total Trades:     {}", num_trades);
    println!("Runtime:          {:?}", start.elapsed());

    // Write CSV
    let mut wtr = csv::Writer::from_path(&output_path).expect("Cannot open output CSV");
    wtr.write_record(&["metric", "value"]).ok();
    wtr.write_record(&["instrument", &instrument_id]).ok();
    wtr.write_record(&["pnl", &format!("{:.2}", pnl)]).ok();
    wtr.write_record(&["max_drawdown", &format!("{:.2}", max_dd)]).ok();
    wtr.write_record(&["num_trades", &num_trades.to_string()]).ok();
    wtr.flush().ok();

    println!("\nResults written to {}", output_path);
}

// Add csv dependency to Cargo.toml example metadata at top of file
// ─────────────────────────────────────────────────────────────────────────────
// Add to libs/nexus/Cargo.toml:
