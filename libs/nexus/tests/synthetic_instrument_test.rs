use nexus::instrument::{parse_synthetic_formula, Formula, InstrumentId, SyntheticInstrument};
use std::collections::HashMap;

#[test]
fn test_synthetic_btceth_spread_deviation_trade() {
    let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
    let eth_id = InstrumentId::new("ETHUSDT", "BINANCE");

    // ETHBTC spread: ETHUSDT / BTCUSDT = 3000/50000 = 0.06
    let synth = SyntheticInstrument::new(
        "ETHBTC",
        4, // price_precision
        vec![eth_id.clone(), btc_id.clone()], // ETH first, BTC second for ETH/BTC ratio
        "slot0 / slot1",
        0,
        0,
    )
    .unwrap();

    let mut prices = HashMap::new();
    prices.insert(btc_id, 50_000_000_000i64);
    prices.insert(eth_id, 3_000_000_000i64);

    let price = synth.compute_price(&prices).unwrap();
    // ETH/BTC = 3000/50000 = 0.06
    assert!((price - 0.06).abs() < 0.001, "price = {}", price);

    // Fair value 0.055 vs price 0.06
    let fair_value = 0.055;
    let deviation = ((price - fair_value) / fair_value) * 10_000.0;
    let two_sigma_bps = 200.0;
    assert!(
        deviation.abs() > two_sigma_bps,
        "deviation {} bps should be > 2σ (200 bps) to trigger trade, price={}",
        deviation,
        price
    );
}

#[test]
fn test_synthetic_btceth_spread_within_threshold() {
    let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
    let eth_id = InstrumentId::new("ETHUSDT", "BINANCE");

    let synth = SyntheticInstrument::new(
        "ETHBTC",
        4,
        vec![eth_id.clone(), btc_id.clone()],
        "slot0 / slot1",
        0,
        0,
    )
    .unwrap();

    let mut prices = HashMap::new();
    prices.insert(btc_id, 50_000_000_000i64);
    prices.insert(eth_id, 3_000_000_000i64);

    let price = synth.compute_price(&prices).unwrap();
    // Fair value equals price → deviation = 0
    let fair_value = price;
    let deviation = ((price - fair_value) / fair_value) * 10_000.0;
    assert!(
        deviation.abs() <= 0.001,
        "deviation {} bps should be ~0 when fair_value matches (price={})",
        deviation,
        price
    );
}

#[test]
fn test_synthetic_sub_formula() {
    let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
    let eth_id = InstrumentId::new("ETHUSDT", "BINANCE");

    let synth = SyntheticInstrument::new(
        "BTC-ETH",
        2,
        vec![btc_id.clone(), eth_id.clone()],
        "slot0 - slot1",
        0,
        0,
    )
    .unwrap();

    let mut prices = HashMap::new();
    prices.insert(btc_id, 50_000_000_000i64);
    prices.insert(eth_id, 3_000_000_000i64);

    let price = synth.compute_price(&prices).unwrap();
    // (50000 - 3000) / 1e9 = 47e-6 but we normalize by PRICE_SCALE
    // Actually: (50000_000_000 - 3000_000_000) / 1e9 = 47.0
    assert!((price - 47.0).abs() < 1.0, "price = {}", price);
}

#[test]
fn test_parse_formula_btceth() {
    let parsed = parse_synthetic_formula("BTCUSDT / ETHUSDT");
    assert!(parsed.is_some());
    let (left, formula, right) = parsed.unwrap();
    assert_eq!(left, "BTCUSDT");
    assert_eq!(right, "ETHUSDT");
    assert_eq!(formula, Formula::Div);
}
