//! CSV → TVC3 converter for Nexus
//! =================================
//! Reads a CSV with columns: timestamp_ns, price, quantity, side, [trade_id]
//! and writes a Nexus-compatible TVC3 file.
//!
//! Usage:
//!   cargo run -p nexus --example csv_to_tvc -- \
//!     --input ./data/BTCUSDT.csv \
//!     --output ./data/BTCUSDT.tvc \
//!     --symbol BTCUSDT.BINANCE
//!
//! TVC3 stores prices as nano-integers (price × 1e9).

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let input = args
        .iter()
        .position(|a| a == "--input")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data/BTCUSDT.csv"));

    let output = args
        .iter()
        .position(|a| a == "--output")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data/BTCUSDT.tvc"));

    let symbol = args
        .iter()
        .position(|a| a == "--symbol")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or("BTCUSDT.BINANCE");

    // Precision: TVC3 stores price_int = price × 10^precision
    // precision=6: price × 1e6 (micro-satoshis), supports min-tick=0.000001
    // For BTC ~50,000: 50_000_000_000 at 1e6 precision (fits in i64)
    // Delta ±1M dollars * 1e6 = ±1e12 (fits in i64, no overflow)
    let precision: u8 = 6;
    // Anchor interval: full anchor written every 1024 ticks (compression ratio target: ~5-7 bytes/tick)
    let anchor_interval: u32 = 1024;

    // Compute FNV-1a hash for instrument_id (same as InstrumentId::new in Nexus)
    let instrument_id = nexus::instrument::fnv1a_hash(symbol.as_bytes());

    println!("Reading: {:?}", input);
    println!("Writing: {:?} (instrument_id={}, precision={})", output, instrument_id, precision);

    let mut rdr = csv::Reader::from_path(&input).expect("Cannot open CSV");
    let mut writer =
        tvc::TvcWriter::new(&output, instrument_id, anchor_interval, precision)
            .expect("Cannot create TVC");

    let mut count = 0u64;
    for result in rdr.records() {
        let row = result.expect("CSV parse error");
        let ts_ns: u64 = row[0].parse().expect("Invalid timestamp");
        let price: f64 = row[1].parse().expect("Invalid price");
        let qty: f64 = row[2].parse().expect("Invalid quantity");
        // CSV side: "BUY" = aggressor is buy = 0, "SELL" = 1
        let side: u8 = if row[3].trim() == "BUY" { 0 } else { 1 };

        // Convert floats → micro-integers for TVC3 (precision=1e6)
        let price_int = (price * 1_000_000.0) as i64;
        let size_int = (qty * 1_000_000.0) as i64;

        let tick = tvc::TradeTick::new(ts_ns, price_int, size_int, side, 1, count as u32);
        writer.write_tick(&tick).expect("Write error");
        count += 1;
    }

    let hash = writer.finalize().expect("Finalize error");
    println!(
        "Wrote {} ticks → {:?} (SHA256: {:x?})",
        count, output, hash
    );
}
