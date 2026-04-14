//! Integration test for data catalog -- Phase 1.7 exit criteria.

use nexus::catalog::{Catalog, CatalogEntry};
use std::fs;
use std::io::Write;
use tempfile::NamedTempFile;

fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.to_uppercase().as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

fn entry(symbol: &str, start: u64, end: u64, path: &str) -> CatalogEntry {
    CatalogEntry {
        instrument_id: fnv1a(symbol),
        symbol: symbol.to_string(),
        start_time_ns: start,
        end_time_ns: end,
        num_ticks: 100000,
        file_path: path.to_string(),
        checksum: "abc123".to_string(),
    }
}

#[test]
fn test_catalog_query_btc_usdt_date_range() {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    drop(tmp);
    fs::remove_file(&path).ok();

    let mut catalog = Catalog::new(&path);

    catalog.add_entry(entry(
        "BTCUSDT",
        1704067200000000000,
        1704153599000000000,
        "/data/btcusdt_2024-01-01.tvc",
    ));
    catalog.add_entry(entry(
        "BTCUSDT",
        1704153600000000000,
        1704239999000000000,
        "/data/btcusdt_2024-01-02.tvc",
    ));
    catalog.add_entry(entry(
        "BTCUSDT",
        1704240000000000000,
        1704326399000000000,
        "/data/btcusdt_2024-01-03.tvc",
    ));
    catalog.add_entry(entry(
        "ETHUSDT",
        1704153600000000000,
        1704239999000000000,
        "/data/ethusdt_2024-01-02.tvc",
    ));

    catalog.save().unwrap();

    let loaded = Catalog::load(&path).unwrap();

    let start_ns = 1704067200000000000u64;
    let end_ns = 1704671999000000000u64;

    let results = loaded.query("BTCUSDT", start_ns, end_ns);
    assert_eq!(
        results.len(),
        3,
        "Should return 3 BTCUSDT files for the week"
    );

    let paths: Vec<_> = results.iter().map(|e| e.file_path.as_str()).collect();
    assert!(paths.contains(&"/data/btcusdt_2024-01-01.tvc"));
    assert!(paths.contains(&"/data/btcusdt_2024-01-02.tvc"));
    assert!(paths.contains(&"/data/btcusdt_2024-01-03.tvc"));

    let single_day = loaded.query("BTCUSDT", 1704153600000000000, 1704239999000000000);
    assert_eq!(single_day.len(), 1);
    assert_eq!(single_day[0].file_path, "/data/btcusdt_2024-01-02.tvc");

    let eth = loaded.query("ETHUSDT", start_ns, end_ns);
    assert_eq!(eth.len(), 1);

    let empty = loaded.query("XRPUSDT", start_ns, end_ns);
    assert!(empty.is_empty());
}

#[test]
fn test_catalog_merge_overlapping() {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    drop(tmp);
    fs::remove_file(&path).ok();

    let mut catalog = Catalog::new(&path);
    catalog.add_entry(entry("BTCUSDT", 1000, 2000, "/a.tvc"));
    catalog.add_entry(entry("BTCUSDT", 1500, 2500, "/b.tvc"));
    catalog.save().unwrap();

    let loaded = Catalog::load(&path).unwrap();
    let all = loaded.get_instrument("BTCUSDT");

    assert_eq!(all.len(), 1, "Overlapping entries should be merged");
    assert_eq!(all[0].start_time_ns, 1000);
    assert_eq!(all[0].end_time_ns, 2500);
    assert_eq!(all[0].num_ticks, 200000); // 100k + 100k after merge
}

#[test]
fn test_catalog_checksum() {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    tmp.write_all(b"hello world").unwrap();
    let tmp_path = tmp.path().to_path_buf();
    // Keep file alive by not dropping tmp
    let checksum = Catalog::compute_checksum(&tmp_path).unwrap();
    drop(tmp);
    assert_eq!(checksum.len(), 64);
    assert_eq!(
        checksum,
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
    );
}
