//! ORB backtest using BacktestEngine (clean builder pattern API).
//!
//! Data file names say "Jan 1-5" but actual tick timestamps are Jan 1-5 EST.
//! RingBuffer header has wall-clock timestamps (wrong), tick data has correct timestamps.

use nexus::backtest::BacktestEngine;
use chrono::NaiveDate;

fn main() {
    let instrument = "BTCUSDT";
    let exchange = "BINANCE";
    // Actual data range: Jan 1-5 EST based on tick timestamps (not RingBuffer headers)
    let start_date = NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(); // Jan 2 EST trading day
    let end_date = NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
    let data_dir = std::path::PathBuf::from("/home/shadowarch/Nexus/data");

    println!("Running ORB backtest for {} {} on {}", instrument, exchange, start_date);
    println!("Data dir: {:?}", data_dir);
    println!("Note: RingBuffer headers show wrong timestamps (wall-clock at creation).");
    println!("      Actual tick timestamps confirm Jan 1-5 EST data.\n");

    let result = BacktestEngine::new()
        .with_instrument(instrument, exchange).expect("invalid instrument")
        .with_date_range(start_date, end_date).expect("invalid date range")
        .with_data_dir(data_dir).expect("data dir not found")
        .with_initial_equity(100_000.0)
        .with_commission_bps(0.5)
        .run(|| OrbStrategy::new())
        .expect("backtest failed");

    println!("\n=== Backtest Results ===");
    println!("PnL:              ${:.2}", result.pnl);
    println!("Max Drawdown:     ${:.2} ({:.2}%)", result.max_drawdown, result.max_drawdown_pct);
    println!("Trades:           {}", result.num_trades);
    println!("Ticks:            {}", result.num_ticks);
    println!("Win Rate:         {:.1}%", result.win_rate * 100.0);
    println!("Sharpe Ratio:     {:.3}", result.sharpe_ratio);
    println!("Avg Trade PnL:    ${:.2}", result.avg_trade_pnl);
    println!("Duration:          {:.1}s", result.duration_secs);
    println!("Start:            {}", ts_to_date(result.start_ts_ns));
    println!("End:              {}", ts_to_date(result.end_ts_ns));
}

fn ts_to_date(ns: u64) -> String {
    use chrono::DateTime;
    if ns == 0 { return "N/A".to_string(); }
    let dt = DateTime::from_timestamp_nanos(ns as i64);
    dt.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

// =============================================================================
// ORB Strategy
// =============================================================================

use nexus::StrategyCtx;
use nexus_types::{InstrumentId, Signal, Tick};

/// Opening Range Breakout strategy.
/// - Tracks high/low during first 5 minutes after 9:30 AM EST market open
/// - At 9:35 EST: arms breakout — longs if price > HH, shorts if price < LL
/// - 1% stop loss, EOD close at 4:00 PM EST
#[derive(Clone)]
struct OrbStrategy {
    /// High of the opening range (set during 9:30-9:35 EST)
    orb_high: Option<f64>,
    /// Low of the opening range (set during 9:30-9:35 EST)
    orb_low: Option<f64>,
    /// Whether position is currently open
    position_open: bool,
    /// Direction: 1=long, -1=short, 0=none
    position_dir: i8,
    /// Upper band (HH at arming)
    hh: f64,
    /// Lower band (LL at arming)
    ll: f64,
    /// Entry price when position opened
    entry_price: f64,
    /// Stop loss price
    stop_price: f64,
}

impl OrbStrategy {
    fn new() -> Self {
        Self {
            orb_high: None,
            orb_low: None,
            position_open: false,
            position_dir: 0,
            hh: 0.0,
            ll: 0.0,
            entry_price: 0.0,
            stop_price: 0.0,
        }
    }

    fn reset(&mut self) {
        self.orb_high = None;
        self.orb_low = None;
        self.position_open = false;
        self.position_dir = 0;
        self.hh = 0.0;
        self.ll = 0.0;
        self.entry_price = 0.0;
        self.stop_price = 0.0;
    }
}

impl nexus_strategy::Strategy for OrbStrategy {
    fn name(&self) -> &str { "ORB" }

    fn mode(&self) -> nexus_types::BacktestMode {
        nexus_types::BacktestMode::Tick
    }

    fn subscribed_instruments(&self) -> Vec<InstrumentId> {
        vec![InstrumentId::new("BTCUSDT", "BINANCE")]
    }

    fn parameters(&self) -> Vec<nexus_types::ParameterSchema> {
        vec![]
    }

    fn clone_box(&self) -> Box<dyn nexus_strategy::Strategy> {
        Box::new(self.clone())
    }

    fn on_reset(&mut self) {
        self.reset();
    }

    fn on_trade(
        &mut self,
        _instrument_id: InstrumentId,
        tick: &Tick,
        _ctx: &mut dyn StrategyCtx,
    ) -> Option<Signal> {
        let price = tick.price;
        let ts = tick.timestamp_ns;

        // Compute EST minute-of-day from UTC timestamp
        let utc_h = ((ts / 3_600_000_000_000u64) % 24) as u32;
        let utc_m = ((ts / 60_000_000_000u64) % 60) as u32;
        let est_h = if utc_h >= 5 { utc_h - 5 } else { utc_h + 19 };
        let est_min = est_h * 60 + utc_m;

        // Key EST minutes:
        // - 570 = 9:30 AM (market open)
        // - 575 = 9:35 AM (ORB end + breakout start)
        // - 960 = 4:00 PM (EOD close)
        const MARKET_OPEN_MIN: u32 = 570;
        const ORB_END_MIN: u32 = 575;
        const EOD_CLOSE_MIN: u32 = 960;

        // ── ORB Range Tracking (9:30-9:35 EST, minutes 570-574) ──
        let in_orb = est_min >= MARKET_OPEN_MIN && est_min < ORB_END_MIN;
        if in_orb {
            if self.orb_high.is_none() {
                self.orb_high = Some(price);
                self.orb_low = Some(price);
            } else {
                self.orb_high = Some(self.orb_high.unwrap().max(price));
                self.orb_low = Some(self.orb_low.unwrap().min(price));
            }
        }

        // ── Arming: set HH/LL on first tick at or after minute 575 (9:35 EST) ──
        // Note: est_min >= 575 (not ==) because ticks arrive ~60s apart, so we may
        // never hit exactly minute 575. The first tick with est_min >= 575 arms ORB.
        if est_min >= ORB_END_MIN && self.hh == 0.0 && self.orb_high.is_some() {
            self.hh = self.orb_high.unwrap();
            self.ll = self.orb_low.unwrap();
        }

        // ── Breakout Entry (every tick once armed) ──
        if !self.position_open && self.hh > 0.0 {
            if price > self.hh {
                self.position_open = true;
                self.position_dir = 1;
                self.entry_price = price;
                self.stop_price = self.ll - 0.01 * price;
                return Some(Signal::Buy);
            } else if price < self.ll {
                self.position_open = true;
                self.position_dir = -1;
                self.entry_price = price;
                self.stop_price = self.hh + 0.01 * price;
                return Some(Signal::Sell);
            }
        }

        // ── Stop Loss Check ──
        if self.position_open && self.position_dir != 0 {
            let stopped = match self.position_dir {
                1 => price < self.stop_price,
                -1 => price > self.stop_price,
                _ => false,
            };

            if stopped {
                self.position_open = false;
                self.position_dir = 0;
                self.hh = 0.0;
                self.ll = 0.0;
                self.orb_high = None;
                self.orb_low = None;
                return Some(Signal::Close);
            }

            // ── EOD Close at or after 4:00 PM EST ──
            if est_min >= EOD_CLOSE_MIN {
                self.position_open = false;
                self.position_open = false;
                self.position_dir = 0;
                self.hh = 0.0;
                self.ll = 0.0;
                self.orb_high = None;
                self.orb_low = None;
                return Some(Signal::Close);
            }
        }

        None
    }

    fn on_bar(
        &mut self,
        _instrument_id: InstrumentId,
        _bar: &nexus_types::Bar,
        _ctx: &mut dyn StrategyCtx,
    ) -> Option<Signal> {
        None // ORB uses tick mode
    }
}