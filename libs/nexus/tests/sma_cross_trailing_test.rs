//! Integration test for SmaCrossTrailingStrategy.
//!
//! Verifies:
//! - Strategy instantiates correctly with all parameters
//! - SMA crossover logic produces correct Buy/Sell signals
//! - Per-instrument state is isolated
//! - Clone and reset work correctly

use nexus::engine::core::EngineContext;
use nexus::signals::SignalBus;
use nexus::Strategy;
use nexus_strategy::indicators::{Indicator, Sma};
use nexus_strategy::SmaCrossTrailingStrategy;
use nexus_types::{Bar, InstrumentId, ParameterType, ParameterValue, Signal as NexusSignal};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn make_bar(timestamp_ns: u64, close: f64) -> Bar {
    Bar {
        timestamp_ns,
        open: close - 0.1,
        high: close + 1.0,
        low: close - 1.0,
        close,
        volume: 1.0,
        buy_volume: 0.5,
        sell_volume: 0.5,
        tick_count: 10,
    }
}

// =============================================================================
// Smoke test: strategy construction and trait methods
// =============================================================================

#[test]
fn test_strategy_construction() {
    let strat = SmaCrossTrailingStrategy::new(20, 50, 1.0, 0.01, 0.02);

    assert_eq!(strat.name(), "sma_cross_trailing");
    assert_eq!(strat.mode(), nexus_types::BacktestMode::Bar);
    assert_eq!(strat.timeframe(), Some(Duration::from_secs(60)));

    let instruments = strat.subscribed_instruments();
    assert_eq!(instruments.len(), 2);
    assert!(instruments.contains(&InstrumentId::new("BTCUSDT", "BINANCE")));
    assert!(instruments.contains(&InstrumentId::new("ETHUSDT", "BINANCE")));
}

#[test]
fn test_strategy_parameters() {
    let strat = SmaCrossTrailingStrategy::new(20, 50, 1.0, 0.01, 0.02);
    let params = strat.parameters();

    assert_eq!(params.len(), 5);

    let fast_param = &params[0];
    assert_eq!(fast_param.name, "fast_period");
    assert_eq!(fast_param.param_type, ParameterType::Int);
    assert_eq!(fast_param.default, ParameterValue::Int(20));

    let trailing_param = &params[3];
    assert_eq!(trailing_param.name, "trailing_delta_pct");
    assert_eq!(trailing_param.param_type, ParameterType::Float);
    assert_eq!(trailing_param.default, ParameterValue::Float(0.01));

    let sl_param = &params[4];
    assert_eq!(sl_param.name, "sl_pct");
    assert_eq!(sl_param.default, ParameterValue::Float(0.02));
}

#[test]
fn test_strategy_clone_box() {
    let strat = SmaCrossTrailingStrategy::new(20, 50, 1.0, 0.01, 0.02);
    let cloned = strat.clone_box();

    assert_eq!(cloned.name(), "sma_cross_trailing");
    assert_eq!(cloned.parameters().len(), 5);
}

// =============================================================================
// SMA indicator correctness (unit-level)
// =============================================================================

#[test]
fn test_sma_mean_after_warmup() {
    let mut sma = Sma::new(5);
    assert!(sma.mean().is_none());

    sma.update(10.0);
    assert!(sma.mean().is_none()); // 1/5 < window

    sma.update(12.0);
    sma.update(14.0);
    sma.update(16.0); // 4/5 < window

    assert!(sma.mean().is_none());

    sma.update(18.0); // 5/5 == window
    assert_eq!(sma.mean(), Some(14.0)); // (10+12+14+16+18)/5 = 14
}

#[test]
fn test_sma_rolling_window() {
    let mut sma = Sma::new(3);

    assert_eq!(sma.update(10.0), None);
    assert_eq!(sma.update(20.0), None);
    assert_eq!(sma.update(30.0), Some(20.0)); // (10+20+30)/3

    assert_eq!(sma.update(40.0), Some(30.0)); // (20+30+40)/3
    assert_eq!(sma.update(50.0), Some(40.0)); // (30+40+50)/3
    assert_eq!(sma.update(60.0), Some(50.0)); // (40+50+60)/3
}

// =============================================================================
// Strategy on_bar: SMA crossover signal generation
// =============================================================================

#[test]
fn test_sma_crossover_buy_signal() {
    let mut strat = SmaCrossTrailingStrategy::new(3, 5, 1.0, 0.01, 0.02);
    let id = InstrumentId::new("BTCUSDT", "BINANCE");
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10_000.0, signal_bus, std::ptr::null_mut());

    // Build bars to force a fast SMA cross above slow SMA:
    // Prices: 100 -> 95 -> 90 -> 95 -> 100 -> 105 -> 110 -> 105 -> 100 -> 95
    // SMA(3) should cross above SMA(5) after the uptick sequence
    let prices = vec![
        100.0, 95.0, 90.0,  // warmup period for SMA(5)
        95.0,              // SMA(3)=95, SMA(5)=93.8 (close)
        100.0,             // SMA(3)=95, SMA(5)=94.6
        105.0,             // SMA(3)=100, SMA(5)=96.8
        110.0,             // SMA(3)=101.7, SMA(5)=99.0
        115.0,             // SMA(3)=110, SMA(5)=102.0 -- first cross above
        120.0,             // buy maintained
    ];

    let mut buy_seen = false;

    for (i, price) in prices.iter().enumerate() {
        let bar = make_bar(i as u64 * 60_000_000_000, *price);
        let result = strat.on_bar(id.clone(), &bar, &mut ctx);
        if result == Some(NexusSignal::Buy) {
            buy_seen = true;
        }
    }

    assert!(buy_seen, "Expected at least one buy signal after crossover");
}

#[test]
fn test_sma_crossover_no_signal_without_warmup() {
    let mut strat = SmaCrossTrailingStrategy::new(5, 10, 1.0, 0.01, 0.02);
    let id = InstrumentId::new("ETHUSDT", "BINANCE");
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10_000.0, signal_bus, std::ptr::null_mut());

    // Fewer bars than slow_period (10) — no crossover possible
    for i in 0..5 {
        let bar = make_bar(i as u64 * 60_000_000_000, 100.0 + (i as f64));
        let result = strat.on_bar(id.clone(), &bar, &mut ctx);
        assert_eq!(result, None, "No signal expected during warmup period");
    }
}

// =============================================================================
// Per-instrument state isolation
// =============================================================================

#[test]
fn test_per_instrument_independent_state() {
    let mut strat = SmaCrossTrailingStrategy::new(3, 5, 1.0, 0.01, 0.02);
    let btc = InstrumentId::new("BTCUSDT", "BINANCE");
    let eth = InstrumentId::new("ETHUSDT", "BINANCE");
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10_000.0, signal_bus, std::ptr::null_mut());

    // BTC: 10 bars of rising price → SMA(3) crosses above SMA(5) → BUY
    let btc_prices: Vec<f64> = (0..10).map(|i| 100.0 + (i as f64)).collect();
    let mut btc_signals = 0;
    for (i, price) in btc_prices.iter().enumerate() {
        let bar = make_bar(i as u64 * 60_000_000_000, *price);
        if strat.on_bar(btc.clone(), &bar, &mut ctx) == Some(NexusSignal::Buy) {
            btc_signals += 1;
        }
    }

    // ETH: 5 bars only — insufficient for SMA(5) warmup → no signals
    for i in 0..5 {
        let bar = make_bar(i as u64 * 60_000_000_000, 100.0 + (i as f64));
        strat.on_bar(eth.clone(), &bar, &mut ctx);
    }

    assert_eq!(btc_signals, 1, "BTC should produce exactly one buy signal");
}

// =============================================================================
// on_reset clears state
// =============================================================================

#[test]
fn test_on_reset_clears_state() {
    let mut strat = SmaCrossTrailingStrategy::new(3, 5, 1.0, 0.01, 0.02);
    let id = InstrumentId::new("BTCUSDT", "BINANCE");
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10_000.0, signal_bus, std::ptr::null_mut());

    // Warm up with 10 bars (enough for SMA(5) to produce a value)
    for i in 0..10 {
        let bar = make_bar(i as u64 * 60_000_000_000, 100.0 + (i as f64));
        strat.on_bar(id.clone(), &bar, &mut ctx);
    }

    // Reset clears all internal state
    strat.on_reset();

    // After reset, only 3 bars — not enough for SMA(5) warmup → no signals
    for i in 0..3 {
        let bar = make_bar(i as u64 * 60_000_000_000, 100.0);
        let result = strat.on_bar(id.clone(), &bar, &mut ctx);
        assert_eq!(result, None, "No signal expected immediately after reset + warmup");
    }
}
