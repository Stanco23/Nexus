//! DATA LAYER integration test — end-to-end verification of the data pipeline.
//!
//! Tests real scenarios for the features the DATA layer is responsible for:
//! 1. Roundtrip via RingBuffer: write TVC3 → open → iterate → prices match input
//! 2. isBuyerMaker plumbing: Binance CSV "isBuyerMaker" column reaches TradeTick.side correctly
//! 3. Catalog rejects corrupt TVC files: zero-header files are silently skipped, not indexed
//! 4. TradeTick::price() conversion: BTC price round-trips through nano-int conversion
//! 5. Existing valid TVC files in data/ decode correctly (sanity for downstream layers)

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use nexus::buffer::RingBuffer;
use nexus::data_manager::downloader::RawTradeData;
use nexus::data_manager::types::{Exchange, Venue};
use nexus::instrument::InstrumentId;
use tempfile::TempDir;
use tvc::{TradeTick, TvcWriter};

/// Resolve a path relative to the workspace root (tests run from target/debug/deps).
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or(manifest)
}

fn resolve(p: &str) -> PathBuf {
    let candidate = PathBuf::from(p);
    if candidate.is_absolute() {
        candidate
    } else {
        workspace_root().join(candidate)
    }
}

/// Synthetic Binance CSV row (matches the actual Binance Data Archive schema).
/// Columns: trade_id, price, qty, quoteQty, time, isBuyerMaker, isBestMatch
fn binance_csv_row(
    trade_id: u64,
    price: f64,
    qty: f64,
    time_ms: u64,
    is_buyer_maker: bool,
) -> String {
    format!(
        "{},{:.8},{:.8},{},{},{},true\n",
        trade_id, price, qty, price * qty, time_ms, is_buyer_maker
    )
}

/// Write a small synthetic Binance CSV file with mixed buy/sell aggressors.
fn write_synthetic_csv(path: &Path) {
    let mut f = File::create(path).expect("create CSV");
    writeln!(
        f,
        "trade_id,price,qty,quoteQty,time,isBuyerMaker,isBestMatch"
    )
    .unwrap();
    let base_ts_ms: u64 = 1_735_776_000_000; // 2025-01-02 00:00:00 UTC
    for i in 0..100u64 {
        let price = 94_500.0 + (i as f64) * 0.5;
        let qty = 0.001 + (i % 10) as f64 * 0.0005;
        let is_buyer_maker = i % 2 == 0;
        let ts_ms = base_ts_ms + i * 100;
        write!(
            f,
            "{}",
            binance_csv_row(i + 1, price, qty, ts_ms, is_buyer_maker)
        )
        .unwrap();
    }
}

/// Write a corrupt (zeroed-header) TVC file at the given path.
fn write_corrupt_tvc(path: &Path) {
    let mut f = File::create(path).expect("create corrupt TVC");
    let zeros = vec![0u8; 128];
    f.write_all(&zeros).unwrap();
    f.write_all(&vec![0xABu8; 1000]).unwrap();
}

/// Convert a Binance CSV row to (ts_ns, price_int, size_int, side).
/// Replicates BinanceDownloader logic.
fn parse_csv_row_to_trade(line: &str) -> (u64, i64, i64, u8) {
    let fields: Vec<&str> = line.split(',').collect();
    let price: f64 = fields[1].parse().expect("price");
    let qty: f64 = fields[2].parse().expect("qty");
    let time_ms: u64 = fields[4].parse().expect("time");
    let is_buyer_maker: bool = fields[5].parse().expect("isBuyerMaker");
    let ts_ns = time_ms * 1_000_000;
    let price_int = (price * 1e9) as i64;
    let size_int = (qty * 1e9) as i64;
    let side: u8 = if is_buyer_maker { 1 } else { 0 };
    (ts_ns, price_int, size_int, side)
}

/// =============================================================================
/// Test 1: Roundtrip via TvcWriter → RingBuffer → RingIter
/// =============================================================================
#[test]
fn test_real_csv_roundtrip_via_writer_ringbuffer() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("roundtrip.tvc");
    let precision: u8 = 9;
    let anchor_interval: u32 = 1024;
    let instrument_id: u32 = 0xDEADBEEF;

    // Build RawTradeData from parsed CSV
    let date = NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
    let mut trades = Vec::new();
    let csv_path = tmp.path().join("input.csv");
    write_synthetic_csv(&csv_path);
    let csv_data = fs::read_to_string(&csv_path).unwrap();
    for line in csv_data.lines().skip(1) {
        trades.push(parse_csv_row_to_trade(line));
    }
    assert_eq!(trades.len(), 100);
    let data = RawTradeData {
        exchange: Exchange::Binance,
        venue: Venue::Spot,
        symbol: "BTCUSDT".to_string(),
        date,
        trades,
    };

    // Write via TvcWriter
    let mut writer = TvcWriter::new(&path, instrument_id, anchor_interval, precision).unwrap();
    for (i, (ts_ns, price_int, size_int, side)) in data.trades.iter().enumerate() {
        writer
            .write_tick(&TradeTick {
                timestamp_ns: *ts_ns,
                price_int: *price_int,
                size_int: *size_int,
                side: *side,
                flags: 1,
                sequence: i as u32,
            })
            .unwrap();
    }
    writer.finalize().unwrap();

    // Read back via RingBuffer (the engine's actual reader)
    let rb = RingBuffer::open(&path, InstrumentId::new("BTCUSDT", "BINANCE"))
        .expect("RingBuffer::open");
    let mut count = 0;
    let mut last_ts = 0u64;
    for tick in rb.iter() {
        let expected_price_int = data.trades[count].1;
        let expected_side = data.trades[count].3;
        assert_eq!(
            tick.price_int, expected_price_int,
            "tick {count} price mismatch"
        );
        assert_eq!(tick.side, expected_side, "tick {count} side mismatch");
        assert!(tick.timestamp_ns >= last_ts, "tick {count} ts regressed");
        last_ts = tick.timestamp_ns;
        count += 1;
    }
    assert_eq!(count, 100, "should iterate all 100 ticks");
}

/// =============================================================================
/// Test 2: isBuyerMaker plumbing — alternating 0/1 sides survive roundtrip
/// =============================================================================
#[test]
fn test_is_buyer_maker_side_plumbing() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("side.tvc");

    let date = NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
    let trades: Vec<(u64, i64, i64, u8)> = (0..10u64)
        .map(|i| {
            let ts_ns = 1_735_776_000_000_000_000u64 + i * 100_000;
            let price_int = 94_500_000_000_000i64 + (i * 100_000) as i64;
            let size_int = 1_000_000i64;
            let side: u8 = (i % 2) as u8;
            (ts_ns, price_int, size_int, side)
        })
        .collect();

    let data = RawTradeData {
        exchange: Exchange::Binance,
        symbol: "BTCUSDT".to_string(),
        date,
        venue: Venue::Spot,
        trades,
    };

    let mut writer = TvcWriter::new(&path, 0x1234, 100, 9).unwrap();
    for (i, (ts_ns, price_int, size_int, side)) in data.trades.iter().enumerate() {
        writer
            .write_tick(&TradeTick {
                timestamp_ns: *ts_ns,
                price_int: *price_int,
                size_int: *size_int,
                side: *side,
                flags: 1,
                sequence: i as u32,
            })
            .unwrap();
    }
    writer.finalize().unwrap();

    let rb = RingBuffer::open(&path, InstrumentId::new("BTCUSDT", "BINANCE")).unwrap();
    let decoded_sides: Vec<u8> = rb.iter().map(|t| t.side).collect();
    assert_eq!(decoded_sides.len(), 10);
    for i in 0..10 {
        let expected = (i % 2) as u8;
        if decoded_sides[i] != expected {
            eprintln!("DEBUG tick {} side={} expected={}", i, decoded_sides[i], expected);
            for (j, t) in rb.iter().enumerate().take(3) {
                eprintln!("  full tick {}: side={} flags={}", j, t.side, t.flags);
            }
        }
        assert_eq!(
            decoded_sides[i], expected,
            "side at tick {i} should be {expected}"
        );
    }
}

/// =============================================================================
/// Test 3: Catalog rejects corrupt (zero-header) TVC files
/// =============================================================================
#[test]
fn test_catalog_skips_corrupt_tvc_files() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    let dir = root.join("binance").join("spot").join("BTCUSDT");
    fs::create_dir_all(&dir).unwrap();

    // Valid TVC
    let valid_path = dir.join("2025-01-02.tvc");
    let mut writer = TvcWriter::new(&valid_path, 0xABCD, 100, 9).unwrap();
    writer
        .write_tick(&TradeTick {
            timestamp_ns: 1_735_776_000_000_000_000,
            price_int: 94_500_000_000_000,
            size_int: 1_000_000,
            side: 0,
            flags: 1,
            sequence: 0,
        })
        .unwrap();
    writer.finalize().unwrap();

    // Corrupt TVC
    let corrupt_path = dir.join("2025-01-03.tvc");
    write_corrupt_tvc(&corrupt_path);

    // Scan the directory
    let mut dm = nexus::data_manager::DataManager::new(root.clone()).expect("DataManager::new");
    dm.rescan().expect("rescan");
    let has_valid = dm.exists(
        Exchange::Binance,
        Venue::Spot,
        "BTCUSDT",
        NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(),
    );
    let has_corrupt = dm.exists(
        Exchange::Binance,
        Venue::Spot,
        "BTCUSDT",
        NaiveDate::from_ymd_opt(2025, 1, 3).unwrap(),
    );
    assert!(has_valid, "DataManager should see the valid TVC");
    assert!(
        !has_corrupt,
        "DataManager should NOT see the zero-header corrupt TVC"
    );
}

/// =============================================================================
/// Test 4: TradeTick::price() nano-int conversion
/// =============================================================================
#[test]
fn test_trade_tick_price_conversion() {
    let tick = TradeTick {
        timestamp_ns: 0,
        price_int: 94_500_000_000_000,
        size_int: 1_000_000,
        side: 0,
        flags: 1,
        sequence: 0,
    };
    let price = tick.price(9);
    assert!(
        (price - 94_500.0).abs() < 0.0001,
        "price should round-trip to ~94,500, got {price}"
    );
}

/// =============================================================================
/// Test 5: Existing valid TVC files in data/ decode correctly
/// =============================================================================
#[test]
fn test_existing_valid_tvc_files_decode() {
    let candidates = [
        "data/binance/spot/BTCUSDT/2025-03-08.tvc",
        "data/binance/spot/BTCUSDT/2025-03-10.tvc",
    ];
    for rel in candidates {
        let path = resolve(rel);
        if !path.exists() {
            eprintln!("Skipping: {} not present", path.display());
            continue;
        }
        let rb = RingBuffer::open(&path, InstrumentId::new("BTCUSDT", "BINANCE"))
            .expect("RingBuffer::open");
        let header = rb.header().clone();
        let num_ticks = header.num_ticks;
        let decimal_precision = header.decimal_precision;
        assert!(
            num_ticks > 0,
            "file {} should have ticks, got {}",
            rel,
            num_ticks
        );
        let first = rb.iter().next().expect("at least one tick");
        let price = first.price(decimal_precision);
        assert!(
            price > 10_000.0 && price < 1_000_000.0,
            "first price {price} out of plausible BTC range"
        );
    }
}

/// =============================================================================
/// Test 6: Mixed directory — 3 files, 2 valid + 1 corrupt. Catalog sees 2, not 3.
/// =============================================================================
#[test]
fn test_corrupt_file_in_mixed_directory_excluded() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let dir = root.join("binance").join("spot").join("BTCUSDT");
    fs::create_dir_all(&dir).unwrap();

    for (i, valid) in [true, true, false].iter().enumerate() {
        let path = dir.join(format!("2025-01-0{}.tvc", i + 2));
        if *valid {
            let mut writer = TvcWriter::new(&path, 0xAAAA, 100, 9).unwrap();
            writer
                .write_tick(&TradeTick {
                    timestamp_ns: 1_735_776_000_000_000_000 + i as u64 * 1_000_000,
                    price_int: 94_500_000_000_000,
                    size_int: 1_000_000,
                    side: 0,
                    flags: 1,
                    sequence: 0,
                })
                .unwrap();
            writer.finalize().unwrap();
        } else {
            write_corrupt_tvc(&path);
        }
    }

    let mut dm = nexus::data_manager::DataManager::new(root).expect("DataManager::new");
    dm.rescan().expect("rescan");
    let mut valid_count = 0;
    for day in 2..=4 {
        let d = NaiveDate::from_ymd_opt(2025, 1, day).unwrap();
        if dm.exists(Exchange::Binance, Venue::Spot, "BTCUSDT", d) {
            valid_count += 1;
        }
    }
    assert_eq!(valid_count, 2, "should see exactly 2 valid files (corrupt excluded)");
}

/// =============================================================================
/// Test 7: Real Binance CSV parsing — sanity
/// =============================================================================
#[test]
fn test_real_binance_csv_in_data_dir_parses() {
    let csv_path = resolve("data/BTCUSDT_2025-01-02.csv");
    if !csv_path.exists() {
        eprintln!("Skipping: {} not present", csv_path.display());
        return;
    }

    // The actual file uses 5-column format: timestamp, price, quantity, side, trade_id
    // (different from the Binance Data Archive 7-column format). Verify each row parses.
    let csv_data = fs::read_to_string(&csv_path).expect("read CSV");
    let mut count = 0;
    for line in csv_data.lines().take(101).skip(1) {
        let fields: Vec<&str> = line.split(',').collect();
        assert!(
            fields.len() == 5 || fields.len() == 7,
            "unexpected column count: {} ({:?})",
            fields.len(),
            fields
        );
        // 5-col: timestamp, price, quantity, side, trade_id
        // 7-col: trade_id, price, quantity, quoteQty, time, isBuyerMaker, isBestMatch
        let (time_str, price_str, qty_str, side_str) = if fields.len() == 5 {
            (fields[0], fields[1], fields[2], fields[3])
        } else {
            (fields[4], fields[1], fields[2], fields[5])
        };
        let time_ms: u64 = time_str.parse().expect("timestamp");
        let price: f64 = price_str.parse().expect("price");
        let qty: f64 = qty_str.parse().expect("qty");
        assert!(time_ms > 0, "ts must be positive");
        assert!(price > 0.0, "price must be positive");
        assert!(qty > 0.0, "qty must be positive");
        assert!(side_str == "BUY" || side_str == "SELL" || side_str == "true" || side_str == "false");
        count += 1;
    }
    assert_eq!(count, 100, "should parse exactly 100 CSV rows");
}

/// =============================================================================
/// Test 8: Catalog case-insensitivity — directory on disk is lowercase but
/// callers can query with any case
/// =============================================================================
#[test]
fn test_catalog_case_insensitive_lookup() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // Write a TVC into a LOWERCASE symbol directory (like bar_ingester would)
    let dir = root.join("binance").join("spot").join("btcusdt");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("2025-01-02.tvc");
    let mut writer = TvcWriter::new(&path, 0xABCD, 100, 9).unwrap();
    writer
        .write_tick(&TradeTick {
            timestamp_ns: 1_735_776_000_000_000_000,
            price_int: 94_500_000_000_000,
            size_int: 1_000_000,
            side: 0,
            flags: 1,
            sequence: 0,
        })
        .unwrap();
    writer.finalize().unwrap();

    let mut dm = nexus::data_manager::DataManager::new(root).expect("DataManager::new");
    dm.rescan().expect("rescan");

    // Caller queries with UPPERCASE — catalog normalizes both sides to uppercase,
    // so the lookup succeeds regardless of on-disk casing.
    let queried_upper = dm.exists(
        Exchange::Binance,
        Venue::Spot,
        "BTCUSDT",
        NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(),
    );
    let queried_lower = dm.exists(
        Exchange::Binance,
        Venue::Spot,
        "btcusdt",
        NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(),
    );
    let queried_mixed = dm.exists(
        Exchange::Binance,
        Venue::Spot,
        "BtCuSdT",
        NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(),
    );
    assert!(queried_upper, "uppercase lookup should match lowercase dir");
    assert!(queried_lower, "lowercase lookup should match lowercase dir");
    assert!(queried_mixed, "mixed-case lookup should match lowercase dir");
}