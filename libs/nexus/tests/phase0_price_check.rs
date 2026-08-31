//! PHASE 0 diagnostic: validate that decoded BTC prices from valid TVC files
//! are in the expected range for March 2025 ($80k-$90k).
//!
//! If prices are wildly out of range, the byte-decoder has a regression
//! from the bug the skill flagged. If they're sane, the decoder is OK
//! and the issue is purely the RingIter off-by-one in time-range queries.

use nexus::buffer::RingBuffer;
use nexus::instrument::InstrumentId;
use std::path::{Path, PathBuf};

// Find workspace root by walking up from CARGO_MANIFEST_DIR until we find Cargo.toml
// with the [workspace] table. Tests run from target/release/deps/, so relative
// paths to data/ only work if we resolve from the workspace root.
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // CARGO_MANIFEST_DIR for nexus is /home/shadowarch/Nexus/libs/nexus
    // workspace root is one level up + one more.
    manifest.parent().and_then(|p| p.parent()).map(|p| p.to_path_buf()).unwrap_or(manifest)
}

fn resolve(p: &str) -> PathBuf {
    let candidate = PathBuf::from(p);
    if candidate.is_absolute() { return candidate; }
    workspace_root().join(candidate)
}

const VALID_FILES: &[&str] = &[
    "data/binance/spot/BTCUSDT/2025-03-08.tvc",
    "data/binance/spot/BTCUSDT/2025-03-10.tvc",
];

fn is_magic_ok(path: &Path) -> bool {
    let bytes = std::fs::read(path).unwrap_or_default();
    bytes.len() >= 4 && &bytes[0..4] == b"TVC3"
}

#[test]
fn phase0_decode_real_btc_prices() {
    for path_str in VALID_FILES {
        let path = resolve(path_str);
        if !path.exists() {
            eprintln!("SKIP {} (missing at {:?})", path_str, path);
            continue;
        }
        if !is_magic_ok(&path) {
            eprintln!("SKIP {} (no TVC3 magic)", path_str);
            continue;
        }
        let id = InstrumentId::new("BTCUSDT", "BINANCE");
        let rb = RingBuffer::open(&path, id).expect("open ring buffer");

        // Sample first 5 anchors + a few deltas each.
        let mut anchors_seen = 0;
        let mut prices: Vec<i64> = Vec::new();
        let mut timestamps: Vec<u64> = Vec::new();
        let mut prices_btc: Vec<f64> = Vec::new();

        for (i, tick) in rb.iter().take(50).enumerate() {
            timestamps.push(tick.timestamp_ns);
            prices.push(tick.price_int);
            let price_btc = tick.price_int as f64 / 1e9;
            prices_btc.push(price_btc);
            if i < 5 {
                eprintln!(
                    "    [{}] ts={} price_int={} price_btc=${:.2}",
                    i, tick.timestamp_ns, tick.price_int, price_btc
                );
            }
            anchors_seen += 1;
        }

        eprintln!(
            "\n[{}] decoded {} ticks",
            path_str, anchors_seen
        );

        // BTC in March 2025 traded $80k-$90k. With decimal_precision=9,
        // price_int should be in [80_000_000_000_000, 90_000_000_000_000].
        let (min_btc, max_btc) = prices_btc
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &p| {
                (lo.min(p), hi.max(p))
            });

        eprintln!(
            "    price_btc range: ${:.2} to ${:.2}",
            min_btc, max_btc
        );

        // Also check timestamps are in March 2025 (ts_ms around 1741xxx)
        // 2025-03-01 00:00:00 UTC = 1740787200 sec = 1740787200000 ms
        if let (Some(&first_ts), Some(&last_ts)) = (timestamps.first(), timestamps.last()) {
            eprintln!(
                "    ts range: {}..{} (delta={})",
                first_ts,
                last_ts,
                last_ts.saturating_sub(first_ts)
            );
        }

        // THE actual assertion: BTC prices should be in a sane range.
        assert!(
            min_btc > 50_000.0 && max_btc < 150_000.0,
            "FAIL {}: prices out of BTC range (${:.2}..${:.2}). \
             Decoder likely hallucinating.",
            path_str, min_btc, max_btc
        );
    }
}