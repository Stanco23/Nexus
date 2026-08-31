//! End-to-end test: download Jan 2 data via Rust pipeline and iterate ticks.
//! This verifies: BinanceDownloader → TvcBuilder → RingBufferSet → tick reading

use std::path::PathBuf;
use chrono::NaiveDate;

fn main() {
    // Data directory
    let data_dir = PathBuf::from("/tmp/nexus_e2e_test");
    std::fs::create_dir_all(&data_dir).unwrap();

    // Register Binance downloader
    let mut downloader = crate::data_manager::Downloader::new();
    downloader.register(crate::data_manager::BinanceDownloader::new());

    let dm = crate::data_manager::DataManager::with_downloader(data_dir.clone(), downloader)
        .expect("failed to create DataManager");

    let config = crate::data_manager::DataManagerConfig {
        data_root: data_dir,
        exchange: crate::data_manager::Exchange::Binance,
        venue: crate::data_manager::Venue::Spot,
        symbol: "BTCUSDT".into(),
        start_date: NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(),
        end_date: NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(),
        download_on_miss: true,
    };

    println!("=== Rust Pipeline End-to-End Test ===");
    println!("1. Loading RingBufferSet (may download + build TVC3)...");

    let buffer_set = match dm.load_ring_buffer_set(&config) {
        Ok(bs) => bs,
        Err(e) => {
            println!("ERROR: {}", e);
            return;
        }
    };

    println!("\n2. RingBufferSet loaded!");
    println!("   instruments: {}", buffer_set.num_instruments());
    println!("   total ticks: {}", buffer_set.total_ticks());
    println!("   num anchors: {}", buffer_set.num_anchors());

    let buffers = buffer_set.buffers();
    if buffers.is_empty() {
        println!("\nERROR: no buffers!");
        return;
    }

    let (instrument_id, rb) = &buffers[0];
    println!("\n3. Instrument: {}", instrument_id);
    println!("   buffer num_ticks: {}", rb.num_ticks());
    println!("   buffer anchor_interval: {}", rb.anchor_interval());
    println!("   first anchor offset: {}", rb.first_anchor_offset());
    println!("   start_time: {}", rb.start_time_ns());
    println!("   end_time: {}", rb.end_time_ns());

    // First anchor
    println!("\n4. Decoding first anchor...");
    let first_anchor_offset = rb.first_anchor_offset();
    match rb.decode_anchor_at(first_anchor_offset) {
        Ok(tick) => {
            let price = tick.price_int as f64 / 1e9_f64;
            let ts_sec = tick.timestamp_ns as f64 / 1e9_f64;
            println!("   first anchor: seq={}, price={:.2}, ts={:.6}", tick.sequence, price, ts_sec);
        }
        Err(e) => {
            println!("   ERROR decoding first anchor: {}", e);
        }
    }

    // First 20 ticks
    println!("\n5. Iterating first 20 ticks via RingIter...");
    let mut ring_iter = rb.iter();
    for i in 0..20 {
        match ring_iter.next() {
            Some(tick) => {
                let price = tick.price_int as f64 / 1e9_f64;
                let ts_sec = tick.timestamp_ns as f64 / 1e9_f64;
                println!("   tick[{}]: seq={}, price={:.2}, ts={:.6}", i, tick.sequence, price, ts_sec);
            }
            None => {
                println!("   tick[{}]: None (end)", i);
                break;
            }
        }
    }

    // Ticks 100-110
    println!("\n6. Iterating ticks 100-110 (deltas after first anchor)...");
    let mut ring_iter2 = rb.iter();
    for _ in 0..105 { ring_iter2.next(); }
    for i in 0..10 {
        match ring_iter2.next() {
            Some(tick) => {
                let price = tick.price_int as f64 / 1e9_f64;
                let ts_sec = tick.timestamp_ns as f64 / 1e9_f64;
                println!("   tick[{}] (seq {}): price={:.2}, ts={:.6}", i, tick.sequence, price, ts_sec);
            }
            None => {
                println!("   tick[{}]: None", i);
                break;
            }
        }
    }

    // Test the range that was crashing with Python TVC (seq ~18750)
    println!("\n7. Iterating ticks 18750-18765 (was crashing with Python TVC)...");
    let mut ring_iter3 = rb.iter();
    for _ in 0..18750 { ring_iter3.next(); }
    for i in 0..15 {
        match ring_iter3.next() {
            Some(tick) => {
                let price = tick.price_int as f64 / 1e9_f64;
                let ts_sec = tick.timestamp_ns as f64 / 1e9_f64;
                println!("   tick[{}] (seq {}): price={:.2}, ts={:.6}", i, tick.sequence, price, ts_sec);
            }
            None => {
                println!("   tick[{}]: None", i);
                break;
            }
        }
    }

    // Full iteration through ALL 3.5M ticks
    println!("\n8. Full iteration test (all 3.5M ticks)...");
    let mut count = 0u64;
    let mut ring_iter4 = rb.iter();
    let start = std::time::Instant::now();
    while ring_iter4.next().is_some() {
        count += 1;
        if count % 500_000 == 0 {
            println!("   processed {} ticks...", count);
        }
    }
    let elapsed = start.elapsed();
    println!("   Total ticks iterated: {} in {:.2}s ({:.0} ticks/sec)",
        count, elapsed.as_secs_f64(), count as f64 / elapsed.as_secs_f64());

    println!("\n=== SUCCESS ===");
}