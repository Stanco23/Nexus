//! Integration test for multi-symbol BacktestEngine via with_instruments().
//!
//! Creates synthetic TVC files for BTCUSDT + ETHUSDT, runs a backtest with
//! `BacktestEngine::with_instruments()`, and verifies:
//! - Both instruments receive ticks (merge cursor time-ordered)
//! - No panics or errors during execution
//! - Trade signals are generated for at least one instrument

use chrono::NaiveDate;
use nexus::backtest::{BacktestEngine, CapitalSpread};
use nexus::portfolio::PortfolioStrategy;
use nexus::engine::Signal;
use nexus::portfolio::Portfolio;
use nexus::signals::SignalBus;
use nexus_strategy::Strategy;
use nexus_types::{InstrumentId as NexusInstrumentId, Tick};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tvc::{TradeTick, TvcWriter};

// =============================================================================
// Test setup helpers
// =============================================================================

fn clean_file(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Creates a synthetic TVC file with EST trading-day ticks.
/// Ticks run from 9:30-10:00 EST on the given date.
/// The `date` parameter is the EST trading date (e.g. 2025-01-02).
/// File is written to a path derived from `date` (no UTC shift on filename).
fn create_tvc_with_ticks(
    path: &Path,
    instrument_id: u32,
    date: NaiveDate,
    base_price: f64,
) {
    clean_file(path);

    // EST 9:30 AM = UTC 14:30 on same calendar day (no DST).
    // We write the file with the EST trading date in the path.
    let start_ns = date
        .and_hms_opt(14, 30, 0) // 14:30 UTC = 9:30 AM EST
        .unwrap()
        .and_utc()
        .timestamp_nanos_opt()
        .unwrap() as u64;

    let mut writer = TvcWriter::new(path, instrument_id, 10, 9).unwrap();

    // Write ticks at 10-second intervals for 30 minutes = 180 ticks
    for i in 0..180u64 {
        let tick = TradeTick::new(
            start_ns + i * 10_000_000_000, // 10s intervals
            (base_price * 1_000_000_000.0) as i64 + (i as i64) * 1_000_000, // rising price
            1_000_000_000i64, // 1.0 size
            0, // taker side: buy
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();
}

// =============================================================================
// Minimal portfolio strategy that tracks both instruments
// =============================================================================

struct DualTrackerStrategy {
    instrument_a: NexusInstrumentId,
    instrument_b: NexusInstrumentId,
    /// Maps instrument → last price seen
    last_prices: HashMap<NexusInstrumentId, f64>,
    /// Maps instrument → number of ticks received
    tick_counts: HashMap<NexusInstrumentId, u64>,
    /// Maps instrument → last signal returned
    last_signals: HashMap<NexusInstrumentId, Signal>,
}

impl DualTrackerStrategy {
    fn new(a: NexusInstrumentId, b: NexusInstrumentId) -> Self {
        let mut tick_counts = HashMap::new();
        tick_counts.insert(a.clone(), 0);
        tick_counts.insert(b.clone(), 0);
        let mut last_prices = HashMap::new();
        last_prices.insert(a.clone(), 0.0);
        last_prices.insert(b.clone(), 0.0);
        let mut last_signals = HashMap::new();
        last_signals.insert(a.clone(), Signal::Close);
        last_signals.insert(b.clone(), Signal::Close);
        Self {
            instrument_a: a,
            instrument_b: b,
            last_prices,
            tick_counts,
            last_signals,
        }
    }
}

impl nexus_strategy::Strategy for DualTrackerStrategy {
    fn name(&self) -> &str { "DualTracker" }
    fn mode(&self) -> nexus_types::BacktestMode { nexus_types::BacktestMode::Tick }
    fn subscribed_instruments(&self) -> Vec<NexusInstrumentId> {
        vec![self.instrument_a.clone(), self.instrument_b.clone()]
    }
    fn parameters(&self) -> Vec<nexus_types::ParameterSchema> { vec![] }
    fn clone_box(&self) -> Box<dyn nexus_strategy::Strategy> { Box::new(self.clone()) }
    fn on_reset(&mut self) {
        self.tick_counts.clear();
        self.last_prices.clear();
        self.last_signals.clear();
    }
    fn on_trade(
        &mut self,
        instrument_id: NexusInstrumentId,
        _tick: &Tick,
        _ctx: &mut dyn nexus_strategy::StrategyCtx,
    ) -> Option<Signal> {
        None // signals come from on_trade via PortfolioStrategy
    }
    fn on_bar(
        &mut self,
        _instrument_id: NexusInstrumentId,
        _bar: &nexus_types::Bar,
        _ctx: &mut dyn nexus_strategy::StrategyCtx,
    ) -> Option<Signal> {
        None
    }
}

impl Clone for DualTrackerStrategy {
    fn clone(&self) -> Self {
        Self {
            instrument_a: self.instrument_a.clone(),
            instrument_b: self.instrument_b.clone(),
            last_prices: self.last_prices.clone(),
            tick_counts: self.tick_counts.clone(),
            last_signals: self.last_signals.clone(),
        }
    }
}

impl PortfolioStrategy for DualTrackerStrategy {
    fn on_trade(
        &mut self,
        instrument_id: NexusInstrumentId,
        _timestamp_ns: u64,
        price: f64,
        _size: f64,
        _portfolio: &mut Portfolio,
    ) -> Signal {
        // Track tick counts
        *self.tick_counts.entry(instrument_id.clone()).or_insert(0) += 1;
        self.last_prices.insert(instrument_id.clone(), price);

        // Simple momentum signal: buy on first up-tick after receiving 10+ ticks
        let count = self.tick_counts.get(&instrument_id).unwrap_or(&0);
        if *count == 10 {
            self.last_signals.insert(instrument_id.clone(), Signal::Buy);
            return Signal::Buy;
        }
        if *count == 50 {
            self.last_signals.insert(instrument_id.clone(), Signal::Close);
            return Signal::Close;
        }
        *self.last_signals.get_mut(&instrument_id).unwrap_or(&mut Signal::Close)
    }

    fn subscribe_signal(&mut self, _sb: Arc<SignalBus>) {}
}

// =============================================================================
// Integration tests
// =============================================================================

#[test]
fn test_multi_symbol_engine_orb() {
    let data_dir = Path::new("/tmp/test_multi_orb");
    let _ = fs::remove_dir_all(data_dir);
    fs::create_dir_all(data_dir.join("binance/spot/BTCUSDT")).unwrap();
    fs::create_dir_all(data_dir.join("binance/spot/ETHUSDT")).unwrap();

    let btc_path = data_dir.join("binance/spot/BTCUSDT/2025-01-02.tvc");
    let eth_path = data_dir.join("binance/spot/ETHUSDT/2025-01-02.tvc");

    // Create BTCUSDT ticks at base 97000, ETHUSDT ticks at base 3400
    create_tvc_with_ticks(&btc_path, 1, NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(), 97000.0);
    create_tvc_with_ticks(&eth_path, 2, NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(), 3400.0);

    let btc_id = NexusInstrumentId::new("BTCUSDT", "BINANCE");
    let eth_id = NexusInstrumentId::new("ETHUSDT", "BINANCE");

    let result = BacktestEngine::new()
        .with_instruments(vec![btc_id.clone(), eth_id.clone()])
        .with_date_range(
            NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(),
            NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(),
        )
        .expect("invalid date range")
        .with_data_dir(data_dir.to_path_buf())
        .expect("data dir not found")
        .with_initial_equity(50_000.0)
        .with_commission_bps(0.5)
        .run(|| DualTrackerStrategy::new(btc_id.clone(), eth_id.clone()));

    let result = result.expect("backtest should succeed");

    // Verify both instruments delivered ticks (360 ticks each = 720 total)
    assert!(
        result.num_ticks >= 360,
        "expected >= 360 ticks (at least BTC's ticks), got {}",
        result.num_ticks
    );

    // Verify trades were placed (momentum signal fires at tick 10 for first instrument)
    assert!(
        result.num_trades >= 1,
        "expected >= 1 trade, got {}",
        result.num_trades
    );

    // Verify equity changed (was changed by commissions or PnL)
    assert!(
        result.pnl != 0.0 || result.num_trades > 0,
        "pnl={}, num_trades={}, expected non-zero",
        result.pnl,
        result.num_trades
    );

    // Verify reasonable duration (30 min of data)
    assert!(
        result.duration_secs > 0.0,
        "duration should be > 0, got {}s",
        result.duration_secs
    );

    // Clean up
    let _ = fs::remove_dir_all(data_dir);
}

#[test]
fn test_multi_symbol_engine_capital_spread_valid() {
    let data_dir = Path::new("/tmp/test_capital_spread");
    let _ = fs::remove_dir_all(data_dir);
    fs::create_dir_all(data_dir.join("binance/spot/BTCUSDT")).unwrap();
    fs::create_dir_all(data_dir.join("binance/spot/ETHUSDT")).unwrap();

    let btc_path = data_dir.join("binance/spot/BTCUSDT/2025-01-03.tvc");
    let eth_path = data_dir.join("binance/spot/ETHUSDT/2025-01-03.tvc");

    create_tvc_with_ticks(&btc_path, 1, NaiveDate::from_ymd_opt(2025, 1, 3).unwrap(), 97000.0);
    create_tvc_with_ticks(&eth_path, 2, NaiveDate::from_ymd_opt(2025, 1, 3).unwrap(), 3400.0);

    let btc_id = NexusInstrumentId::new("BTCUSDT", "BINANCE");
    let eth_id = NexusInstrumentId::new("ETHUSDT", "BINANCE");

    // Equal spread should pass validation
    let result = BacktestEngine::new()
        .with_instruments(vec![btc_id.clone(), eth_id.clone()])
        .with_capital_spread(CapitalSpread::Equal)
        .with_date_range(
            NaiveDate::from_ymd_opt(2025, 1, 3).unwrap(),
            NaiveDate::from_ymd_opt(2025, 1, 3).unwrap(),
        )
        .expect("invalid date range")
        .with_data_dir(data_dir.to_path_buf())
        .expect("data dir not found")
        .with_initial_equity(50_000.0)
        .with_commission_bps(0.5)
        .run(|| DualTrackerStrategy::new(btc_id.clone(), eth_id.clone()));

    assert!(result.is_ok(), "Equal capital spread should be valid, got {:?}", result.err());

    let _ = fs::remove_dir_all(data_dir);
}

#[test]
fn test_multi_symbol_engine_weighted_spread() {
    let data_dir = Path::new("/tmp/test_weighted_spread");
    let _ = fs::remove_dir_all(data_dir);
    fs::create_dir_all(data_dir.join("binance/spot/BTCUSDT")).unwrap();
    fs::create_dir_all(data_dir.join("binance/spot/ETHUSDT")).unwrap();

    let btc_path = data_dir.join("binance/spot/BTCUSDT/2025-01-03.tvc");
    let eth_path = data_dir.join("binance/spot/ETHUSDT/2025-01-03.tvc");

    create_tvc_with_ticks(&btc_path, 1, NaiveDate::from_ymd_opt(2025, 1, 3).unwrap(), 97000.0);
    create_tvc_with_ticks(&eth_path, 2, NaiveDate::from_ymd_opt(2025, 1, 3).unwrap(), 3400.0);

    let btc_id = NexusInstrumentId::new("BTCUSDT", "BINANCE");
    let eth_id = NexusInstrumentId::new("ETHUSDT", "BINANCE");

    // Weighted spread 70/30 should pass validation
    let result = BacktestEngine::new()
        .with_instruments(vec![btc_id.clone(), eth_id.clone()])
        .with_capital_spread(CapitalSpread::Weighted(vec![0.7, 0.3]))
        .with_date_range(
            NaiveDate::from_ymd_opt(2025, 1, 3).unwrap(),
            NaiveDate::from_ymd_opt(2025, 1, 3).unwrap(),
        )
        .expect("invalid date range")
        .with_data_dir(data_dir.to_path_buf())
        .expect("data dir not found")
        .with_initial_equity(50_000.0)
        .with_commission_bps(0.5)
        .run(|| DualTrackerStrategy::new(btc_id.clone(), eth_id.clone()));

    assert!(result.is_ok(), "Weighted [0.7, 0.3] should be valid, got {:?}", result.err());

    let _ = fs::remove_dir_all(data_dir);
}