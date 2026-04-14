//! Integration test for exchange ingestion -- Phase 1.6 exit criteria.
//!
//! Exit: Ingest synthetic ticks -> write TVC3 -> read back -> count matches.

use tempfile::NamedTempFile;
use tvc::{TradeTick, TvcReader, TvcWriter};

fn synthetic_binance_trade_msg(seq: u64) -> Vec<u8> {
    let price = 50000.0 + (seq as f64) * 0.01;
    let size = 0.001 + (seq % 10) as f64 * 0.001;
    let side = if seq % 2 == 0 { "false" } else { "true" };
    let ts_ms = 1672515782135u64 + seq;
    serde_json::to_vec(&serde_json::json!({
        "e": "trade",
        "E": ts_ms + 1,
        "s": "BTCUSDT",
        "t": seq,
        "p": format!("{:.2}", price),
        "q": format!("{:.6}", size),
        "T": ts_ms,
        "m": side == "true", // boolean, not string
        "M": true
    }))
    .unwrap()
}

#[test]
fn test_ingest_binance_ticks_write_and_read_back() {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    drop(tmp);

    let instrument_id = 0x12345678u32;
    let precision = 9u8;
    let anchor_interval = 1000u32;

    // Write using BinanceAdapter-style processing
    let mut writer = TvcWriter::new(&path, instrument_id, anchor_interval, precision).unwrap();

    let num_ticks = 10_000usize;
    let mut count = 0usize;

    for seq in 0..num_ticks {
        let msg = synthetic_binance_trade_msg(seq as u64);

        // Parse JSON (simulating what BinanceAdapter does)
        let msg_str = std::str::from_utf8(&msg).unwrap();
        if !msg_str.contains("\"e\":\"trade\"") {
            continue;
        }

        // Quick parse: extract price, qty, side, ts directly
        // Simulate adapter by extracting values
        let v: serde_json::Value = serde_json::from_str(msg_str).unwrap();
        let price: f64 = v["p"].as_str().unwrap().parse().unwrap();
        let size: f64 = v["q"].as_str().unwrap().parse().unwrap();
        let ts_ms: u64 = v["T"].as_u64().unwrap();
        let is_buyer_maker: bool = v["m"].as_bool().unwrap();
        let side = if is_buyer_maker { 1u8 } else { 0u8 };
        let ts_ns = ts_ms * 1_000_000;

        let tick = TradeTick::from_floats(ts_ns, price, size, side, precision, seq as u32);
        writer.write_tick(&tick).unwrap();
        count += 1;
    }

    let digest = writer.finalize().unwrap();
    assert_eq!(digest.len(), 32);

    // Read back and verify count
    let reader = TvcReader::open(&path).unwrap();
    assert_eq!(
        reader.num_ticks(),
        count as u64,
        "Read back tick count must match written count"
    );
    // Copy from packed struct to avoid unaligned access UB
    let header_instrument_id = reader.header().instrument_id;
    assert_eq!(header_instrument_id, instrument_id);

    // Verify header metadata (copy fields to avoid UB)
    let header = reader.header();
    let header_num_ticks = header.num_ticks;
    let header_start = header.start_time_ns;
    let header_end = header.end_time_ns;
    assert_eq!(header_num_ticks, count as u64);
    assert!(header_start > 0);
    assert!(header_end >= header_start);
}

#[test]
fn test_checkpoint_preserves_partial_data() {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    drop(tmp);

    let instrument_id = 0xABCDEFu32;
    let mut writer = TvcWriter::new(&path, instrument_id, 100, 9).unwrap();

    // Write 500 ticks
    for i in 0u64..500 {
        let tick = TradeTick::from_floats(
            1_000_000_000 + i * 1_000_000,
            100.0 + i as f64 * 0.1,
            1.0,
            (i % 2) as u8,
            9,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }

    writer.checkpoint().unwrap();

    // Write 500 more ticks (simulate resume after reconnect)
    for i in 500u64..1000 {
        let tick = TradeTick::from_floats(
            1_000_000_000 + i * 1_000_000,
            100.0 + i as f64 * 0.1,
            1.0,
            (i % 2) as u8,
            9,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }

    let digest = writer.finalize().unwrap();
    assert_eq!(digest.len(), 32);

    // Read back and verify total count
    let reader = TvcReader::open(&path).unwrap();
    assert_eq!(reader.num_ticks(), 1000);
}

#[test]
fn test_binance_adapter_normalizes_side_correctly() {
    use nexus::ingestion::{BinanceAdapter, BinanceVenue};

    let adapter = BinanceAdapter::new("BTCUSDT", BinanceVenue::Spot);

    // BUY trade: m=false (buyer is taker, aggressor is buy)
    let buy_msg = br#"{"e":"trade","E":1672515782136,"s":"BTCUSDT","t":1,"p":"50000.00","q":"0.01","T":1672515782135,"m":false,"M":true}"#;
    let mut buy_count = 0usize;
    adapter
        .parse_and_process(buy_msg, |tick| {
            assert_eq!(tick.side, 0, "m=false should produce side=0 (buy)");
            buy_count += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(buy_count, 1);

    // SELL trade: m=true (buyer is maker, seller aggressed)
    let sell_msg = br#"{"e":"trade","E":1672515782136,"s":"BTCUSDT","t":2,"p":"50001.00","q":"0.01","T":1672515782136,"m":true,"M":true}"#;
    let mut sell_count = 0usize;
    adapter
        .parse_and_process(sell_msg, |tick| {
            assert_eq!(tick.side, 1, "m=true should produce side=1 (sell)");
            sell_count += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(sell_count, 1);
}
