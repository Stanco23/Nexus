//! Test: use DataManager to download, ingest, and iterate Jan 2 data
//! Verifies the complete Rust pipeline: BinanceDownloader -> TvcBuilder -> RingBuffer

use std::path::PathBuf;
use crate::data_manager::{DataManager, Downloader};
use crate::data_manager::downloaders::binance::BinanceDownloader;
use crate::data_manager::types::{Exchange, Venue, DataManagerConfig};
use crate::buffer::buffer_set::TickBufferSet;
use chrono::NaiveDate;

fn main() {
    let output_dir = PathBuf::from("/tmp/nexus_data_test");
    std::fs::create_dir_all(&output_dir).unwrap();

    let downloader = Downloader::new()
        .register(BinanceDownloader::new());

    let dm = DataManager::with_downloader(output_dir.clone(), downloader)
        .unwrap();

    let config = DataManagerConfig {
        data_root: output_dir.clone(),
        exchange: Exchange::Binance,
        venue: Venue::Spot,
        symbol: "BTCUSDT".into(),
        start_date: NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(),
        end_date: NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(),
        download_on_miss: true,
    };

    println!("=== Testing Rust Data Pipeline ===");
    println!("1. Downloading Jan 2 data via BinanceDownloader...");
    
    match dm.load(&config) {
        Ok(buffer_set) => {
            println!("\n2. Load successful!");
            println!("   instruments: {}", buffer_set.num_instruments());
            println!("   total ticks: {}", buffer_set.total_ticks());
            
            // Now try to iterate as RingBufferSet via TickBufferSet
            // TickBufferSet::from_buffers wraps RingBufferSet
            println!("\n3. Testing Tick iteration...");
            let buffers = buffer_set.buffers();
            for (instrument_id, tb) in buffers.iter() {
                println!("   Instrument: {}", instrument_id);
                println!("   Buffer ticks: {}", tb.len());
                // Print first 5 ticks
                for (i, tick) in tb.iter().take(5).enumerate() {
                    println!("   tick[{}]: price={} ({}), ts={}",
                        i, tick.price_int, tick.price_int as f64 / 1e9_f64, tick.timestamp_ns);
                }
            }
        }
        Err(e) => {
            println!("ERROR: {}", e);
        }
    }
}
