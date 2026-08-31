//! Measure compression on real Binance BTCUSDT data.
//!
//! Reads `data/BTCUSDT_2025-01-02.csv`, writes to a TVC file, then reports
//! average bytes per tick and a breakdown by tick type (base vs overflow).
//!
//! Run with:
//!   cargo run --release --example compression_measure -p nexus

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let csv_path = "data/BTCUSDT_2025-01-02.csv";
    if !Path::new(csv_path).exists() {
        eprintln!("CSV not found at {}; skipping", csv_path);
        std::process::exit(0);
    }

    let tvc_path = std::env::temp_dir().join("compression_measure.tvc");
    let _ = std::fs::remove_file(&tvc_path);

    println!("Reading {}...", csv_path);
    let file = File::open(csv_path)?;
    let reader = BufReader::new(file);
    let mut ticks = Vec::with_capacity(4_000_000);
    let mut csv_bytes: u64 = 0;
    for (i, line) in reader.lines().enumerate() {
        if i == 0 { continue; } // header
        let line = line?;
        csv_bytes += line.len() as u64 + 1; // +1 for newline
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() < 5 { continue; }
        let ts_ns: u64 = fields[0].parse()?;
        let price: f64 = fields[1].parse()?;
        let qty: f64 = fields[2].parse()?;
        let side: u8 = match fields[3] {
            "BUY" => 1,
            "SELL" => 0,
            _ => continue,
        };
        let price_int = (price * 1e9) as i64;
        let qty_int = (qty * 1e9) as i64;
        ticks.push(tvc::TradeTick {
            timestamp_ns: ts_ns,
            price_int,
            size_int: qty_int,
            side,
            flags: 1,
            sequence: ticks.len() as u32,
        });
    }
    let n = ticks.len();
    println!("Loaded {} ticks", n);
    println!("CSV size:  {:.2} MB", csv_bytes as f64 / 1_048_576.0);

    // Write TVC with anchor_interval=1024
    println!("Writing TVC (anchor_interval=1024)...");
    let mut writer = tvc::TvcWriter::new(&tvc_path, 999, 1024, 9)?;
    for tick in &ticks {
        writer.write_tick(tick)?;
    }
    writer.finalize()?;

    let tvc_size = std::fs::metadata(&tvc_path)?.len();
    println!("TVC size:  {:.2} MB", tvc_size as f64 / 1_048_576.0);
    println!("Raw bytes/tick (8 fields × 8 bytes): {:.2}", 64.0);

    // Subtract fixed overhead: 128-byte header, index (4 + 16 * num_anchors),
    // and 32-byte SHA256. Anchors every 1024 ticks.
    let num_anchors = (n + 1023) / 1024;
    let fixed_overhead = 128 + (4 + 16 * num_anchors) + 32;
    let data_bytes = tvc_size - fixed_overhead as u64;
    let avg_per_tick = data_bytes as f64 / n as f64;
    println!();
    println!("=== Compression report ===");
    println!("  Total ticks:              {}", n);
    println!("  Anchors (1024 interval):  {}", num_anchors);
    println!("  TVC file size:            {} bytes ({:.2} MB)", tvc_size, tvc_size as f64 / 1_048_576.0);
    println!("  Fixed overhead:           {} bytes (header + index + sha256)", fixed_overhead);
    println!("  Pure tick data:           {} bytes", data_bytes);
    println!("  Average bytes/tick:       {:.2}", avg_per_tick);
    println!("  vs CSV:                   {:.2}x smaller ({:.2} bytes/tick CSV)",
             csv_bytes as f64 / data_bytes as f64,
             csv_bytes as f64 / n as f64);
    println!();
    println!("Note: anchors are 30 bytes every 1024 ticks = {:.3} bytes/tick overhead from anchors.",
             30.0 / 1024.0);

    // Verify round-trip by reading back and checking key invariants
    println!();
    println!("Round-trip check...");
    let rb = nexus::buffer::RingBuffer::open(&tvc_path, nexus::instrument::InstrumentId::new("BTCUSDT", "BINANCE"))?;
    let mut count = 0u64;
    let mut last_ts = 0u64;
    let mut ts_violations = 0u64;
    let mut sides = std::collections::HashSet::new();
    for tick in rb.iter() {
        if tick.timestamp_ns < last_ts {
            ts_violations += 1;
        }
        last_ts = tick.timestamp_ns;
        sides.insert(tick.side);
        count += 1;
    }
    println!("  Decoded ticks:        {}", count);
    println!("  TS monotonicity:      {} violations", ts_violations);
    println!("  Distinct side values: {:?}", sides);
    println!("  Match:                {}", count == n as u64);

    Ok(())
}
