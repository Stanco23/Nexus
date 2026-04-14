use nexus::instrument::{parse_formula, Formula, InstrumentId, SyntheticInstrument};
use std::collections::HashMap;

#[test]
fn test_synthetic_btceth_spread_deviation_trade() {
    let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
    let eth_id = InstrumentId::new("ETHUSDT", "BINANCE");

    // ETHBTC spread: ETHUSDT / BTCUSDT = 3000/50000 = 0.06
    let synth = SyntheticInstrument::new(
        "ETHBTC",
        "SYNTH",
        vec![eth_id, btc_id], // ETH first, BTC second for ETH/BTC ratio
        Formula::Div,
        1.0,
    );

    let mut prices = HashMap::new();
    prices.insert(btc_id.id, 50_000_000_000i64);
    prices.insert(eth_id.id, 3_000_000_000i64);

    let price = synth.compute_price(&prices).unwrap();
    let fair_value = 0.055; // set fair value different from price (0.06) to trigger deviation
    let deviation = synth.deviation_bps(&prices, fair_value).unwrap();

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

    let synth =
        SyntheticInstrument::new("ETHBTC", "SYNTH", vec![eth_id, btc_id], Formula::Div, 1.0);

    let mut prices = HashMap::new();
    prices.insert(btc_id.id, 50_000_000_000i64);
    prices.insert(eth_id.id, 3_000_000_000i64);

    let price = synth.compute_price(&prices).unwrap();
    let fair_value = price;
    let deviation = synth.deviation_bps(&prices, fair_value).unwrap();
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

    let synth =
        SyntheticInstrument::new("BTC-ETH", "SYNTH", vec![btc_id, eth_id], Formula::Sub, 1.0);

    let mut prices = HashMap::new();
    prices.insert(btc_id.id, 50_000_000_000i64);
    prices.insert(eth_id.id, 3_000_000_000i64);

    let price = synth.compute_price(&prices).unwrap();
    assert!((price - 47.0).abs() < 1.0, "price = {}", price);
}

#[test]
fn test_parse_formula_btceth() {
    let parsed = parse_formula("BTCUSDT / ETHUSDT");
    assert!(parsed.is_some());
    let (left, formula, right) = parsed.unwrap();
    assert_eq!(left, "BTCUSDT");
    assert_eq!(right, "ETHUSDT");
    assert_eq!(formula, Formula::Div);
}
