//! Minimal direct Binance API test.
//!
//! Run with: cargo run --example test_binance_api -p nexus

use std::path::PathBuf;
use chrono::NaiveDate;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use crate::data_manager::{DataManager, Exchange, InstrumentType};

    let base_dir = PathBuf::from("/tmp/tvcb_test");
    let _ = std::fs::remove_dir_all(&base_dir);

    let dm = DataManager::new(base_dir.clone())?;

    println!("Fetching Binance BTCUSDT 15m bars for Jan 2024...");
    println!("Base URL will be: https://api.binance.com/api/v3/klines");
    println!("Timeframe ns: {}", crate::data_manager::bar_ingester::timeframe_to_ns("15m"));

    let start = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
    let end   = NaiveDate::from_ymd_opt(2024, 1, 31).unwrap();

    let result = dm.ingest_bars(
        Exchange::Binance,
        InstrumentType::Spot,
        "BTCUSDT",
        "15m",
        start,
        end,
    ).await;

    match result {
        Ok(paths) => {
            println!("\nSUCCESS: {} files created", paths.len());
            for p in &paths {
                println!("  {:?}", p);
                let data = std::fs::read(p)?;
                let header = tvc::tvcb::types::bytes_to_header(&data[..128].try_into().unwrap());
                println!("    bars: {}, year: {}", header.num_bars, header.year);
            }
        }
        Err(e) => {
            println!("\nERROR: {}", e);
        }
    }

    Ok(())
}