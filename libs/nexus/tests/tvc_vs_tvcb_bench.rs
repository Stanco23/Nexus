//! TVC vs TVCB throughput benchmark.
//!
//! Run with: cargo test -p nexus --test tvc_vs_tvcb_bench --release -- --nocapture

use std::path::PathBuf;
use std::time::Instant;

use tvc::tvcb::writer::TvcbWriter;
use tvc::tvcb::reader::TvcbReader;
use tvc::tvcb::BarIter;
use tvc::TvcReader;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn fnv1a_hash(s: &str) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for byte in s.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

fn make_tvc_path(n: usize) -> PathBuf {
    PathBuf::from(format!("/tmp/bench_ticks_{}.tvc", n))
}

fn make_tvcb_path(n: usize) -> PathBuf {
    PathBuf::from(format!("/tmp/bench_bars_{}.tvcb", n))
}

// ── TVC data generation ──────────────────────────────────────────────────────

fn generate_tvc(n_ticks: usize) -> (PathBuf, usize) {
    use tvc::TvcWriter;
    use tvc::types::TradeTick as TvcTick;

    let path = make_tvc_path(n_ticks);
    let instrument_hash = fnv1a_hash("BTCUSDT");

    let mut writer = TvcWriter::new(&path, instrument_hash, 10, 9).unwrap();
    let base_price = 50_000i64 * 1_000_000_000;
    let start_ts = 1_700_000_000_000_000_000u64;

    let mut price = base_price;
    for i in 0..n_ticks {
        let noise = ((i as i64 % 100) - 50) * 100_000_000;
        price += noise;
        let tick = TvcTick::new(
            start_ts + (i as u64) * 1_000_000_000,
            price,
            1_000_000_000,
            (i % 2) as u8,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let reader = TvcReader::open(&path).unwrap();
    let actual = reader.num_ticks() as usize;
    (path, actual)
}

// ── TVCB data generation ──────────────────────────────────────────────────────

fn generate_tvcb(n_bars: usize) -> (PathBuf, usize) {
    use tvc::tvcb::types::Bar as TvcbBar;

    let path = make_tvcb_path(n_bars);
    let instrument_hash = fnv1a_hash("BTCUSDT");
    let timeframe_ns = 900_000_000_000u64; // 15m bars
    let year = 2025u64;

    let mut writer = TvcbWriter::new(&path, instrument_hash, 10, 9, year, timeframe_ns).unwrap();

    let start_ts = 1_735_000_000_000_000_000u64;

    for i in 0..n_bars {
        let ts = start_ts + (i as u64) * timeframe_ns;
        let bar = TvcbBar::from_floats(
            ts,
            100.0 + i as f64 * 0.01,
            101.0 + i as f64 * 0.01,
            99.0 + i as f64 * 0.01,
            100.5 + i as f64 * 0.01,
            100.4 + i as f64 * 0.01,
            1000.0 + i as f64 * 0.5,
            600.0 + i as f64 * 0.3,
            400.0 + i as f64 * 0.2,
            10 + (i % 20) as u32,
            9,
        );
        writer.write_bar(&bar).unwrap();
    }
    writer.finalize().unwrap();

    let reader = TvcbReader::open(&path).unwrap();
    let actual = reader.num_bars() as usize;
    (path, actual)
}

// ── Benchmarks ───────────────────────────────────────────────────────────────

fn bench_tvc(n_ticks: usize) -> (f64, f64) {
    let (path, actual) = generate_tvc(n_ticks);

    let start = Instant::now();
    {
        let mut reader = TvcReader::open(&path).unwrap();
        let num_ticks = reader.num_ticks();
        let mut count = 0u64;
        for idx in 0..num_ticks {
            let offset = reader.seek_to_tick(idx).unwrap() as usize;
            let _tick = reader.decode_tick_at(offset).unwrap();
            count += 1;
        }
        assert_eq!(count, num_ticks);
    }
    let elapsed = start.elapsed();

    let _ = std::fs::remove_file(&path);

    let ns_per = elapsed.as_nanos() as f64 / actual as f64;
    let rate = actual as f64 / elapsed.as_secs_f64();
    (rate, ns_per)
}

fn bench_tvcb(n_bars: usize) -> (f64, f64) {
    let (path, actual) = generate_tvcb(n_bars);

    let start = Instant::now();
    {
        let files = vec![path.clone()];
        let start_ts = 1_735_000_000_000_000_000u64;
        let end_ts = start_ts + (actual as u64) * 900_000_000_000u64 + 1;
        let mut iter = BarIter::new(files, start_ts, end_ts).unwrap();
        let mut count = 0u64;
        while let Some(_bar) = iter.next() {
            count += 1;
        }
        assert_eq!(count, actual as u64);
    }
    let elapsed = start.elapsed();

    let _ = std::fs::remove_file(&path);

    let ns_per = elapsed.as_nanos() as f64 / actual as f64;
    let rate = actual as f64 / elapsed.as_secs_f64();
    (rate, ns_per)
}

// ── Test ──────────────────────────────────────────────────────────────────────

#[test]
fn test_tvc_vs_tvcb() {
    println!("\n============================================================");
    println!("{:^20} {:>22} {:>22} {:>10}", "", "TVC (ticks)", "TVCB (bars)", "Ratio");
    println!("============================================================");

    for (n, label) in [(1_000, "1K"), (10_000, "10K"), (100_000, "100K")] {
        let (tvc_rate, tvc_ns) = bench_tvc(n);
        let (tvcb_rate, tvcb_ns) = bench_tvcb(n);
        let ratio = tvc_ns / tvcb_ns;

        println!(
            "{:>6} {:>22} {:>22} {:>10.1}x",
            label,
            format!("{:.0} ticks/sec", tvc_rate),
            format!("{:.0} bars/sec", tvcb_rate),
            ratio
        );
    }

    println!("============================================================");
    println!("\nNote: 100K TVCB bars ~= 2.85 years of 15m data.");
    println!("      100K TVC ticks ~= 27 hours at 1 tick/sec.");
}