//! Integration tests for TickBuffer using actual TVC files.

use nexus::buffer::{RingBuffer, TickBuffer};
use std::fs;
use std::path::Path;
use tvc::{TradeTick, TvcWriter};

fn clean_file(path: &Path) {
    let _ = fs::remove_file(path);
}

// =============================================================================
// VPIN Computation Tests
// =============================================================================

#[test]
fn test_vpin_computation_simple() {
    let path = Path::new("/tmp/test_vpin_simple.tvc");
    clean_file(path);

    // 100 ticks, 50 buckets (2 ticks per bucket)
    let mut writer = TvcWriter::new(path, 1u32, 50, 9).unwrap();
    let start_ts = 1_000_000_000u64;

    // Alternating buy/sell, 100 size each
    for i in 0..100u64 {
        let side = if i % 2 == 0 { 0 } else { 1 };
        let tick = TradeTick::new(
            start_ts + i * 1000,
            100000i64 + (i as i64) * 10,
            100i64,
            side,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let buffer = RingBuffer::open(path, 1u64).unwrap();
    let tick_buffer = TickBuffer::from_ring_buffer(&buffer, 50).unwrap();

    assert_eq!(tick_buffer.num_ticks(), 100);
    assert_eq!(tick_buffer.num_buckets(), 50);

    // For simple alternating pattern: cumulative VPIN at bucket boundaries
    // After 50 buckets, we should have valid VPIN values
    for i in 0..50 {
        let vpin = tick_buffer.bucket_vpin(i);
        assert!(vpin.is_some(), "bucket {} should have VPIN", i);
    }

    clean_file(path);
}

#[test]
fn test_vpin_known_values() {
    let path = Path::new("/tmp/test_vpin_known.tvc");
    clean_file(path);

    // Create 20 ticks with known buy/sell pattern
    // Ticks 0-1: buy 100, sell 0 → VPIN = 1.0
    // Ticks 2-3: buy 50, sell 50 → VPIN = 0.0
    // Ticks 4-5: buy 75, sell 25 → VPIN = 0.5
    // And so on...
    let mut writer = TvcWriter::new(path, 1u32, 10, 9).unwrap();
    let start_ts = 1_000_000_000u64;

    let patterns: [(u8, i64); 20] = [
        (0, 100),
        (0, 100), // bucket 0: buy=200, sell=0, VPIN=1.0
        (0, 50),
        (1, 50), // bucket 1: buy=250, sell=50, VPIN=(200)/300=0.667
        (0, 75),
        (1, 25), // bucket 2: buy=325, sell=75, VPIN=(250)/400=0.625
        (0, 80),
        (1, 20), // bucket 3: buy=405, sell=95, VPIN=(310)/500=0.62
        (1, 100),
        (1, 100), // bucket 4: buy=405, sell=295, VPIN=(110)/700=0.157
        (0, 50),
        (0, 50),
        (1, 50),
        (1, 50),
        (0, 80),
        (0, 80),
        (1, 20),
        (1, 20),
        (0, 100),
        (0, 100),
    ];

    for (i, (side, size)) in patterns.iter().enumerate() {
        let tick = TradeTick::new(
            start_ts + (i as u64) * 1000,
            100000i64 + (i as i64) * 10,
            *size,
            *side,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let buffer = RingBuffer::open(path, 1u64).unwrap();
    let tick_buffer = TickBuffer::from_ring_buffer(&buffer, 10).unwrap();

    assert_eq!(tick_buffer.num_ticks(), 20);
    assert_eq!(tick_buffer.num_buckets(), 10);

    // Verify first bucket VPIN = 1.0 (all buys)
    let vpin0 = tick_buffer.bucket_vpin(0).unwrap();
    assert!(
        (vpin0 - 1.0).abs() < 1e-9,
        "bucket 0 VPIN should be 1.0, got {}",
        vpin0
    );

    clean_file(path);
}

// =============================================================================
// TickBuffer from RingBuffer Tests
// =============================================================================

#[test]
fn test_tick_buffer_from_ring_buffer() {
    let path = Path::new("/tmp/test_tb_from_rb.tvc");
    clean_file(path);

    let mut writer = TvcWriter::new(path, 1u32, 10, 9).unwrap();
    let start_ts = 1_000_000_000u64;

    for i in 0..100u64 {
        let tick = TradeTick::new(
            start_ts + i * 1000,
            100000i64 + (i as i64) * 10,
            1000i64,
            (i % 2) as u8,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let rb = RingBuffer::open(path, 1u64).unwrap();
    assert_eq!(rb.num_ticks(), 100);

    let tb = TickBuffer::from_ring_buffer(&rb, 10).unwrap();
    assert_eq!(tb.num_ticks(), 100);
    assert_eq!(tb.instrument_id(), 1u64);
    assert_eq!(tb.num_buckets(), 10);

    clean_file(path);
}

// =============================================================================
// Bucket VPIN Tests
// =============================================================================

#[test]
fn test_bucket_vpin() {
    let path = Path::new("/tmp/test_bucket_vpin.tvc");
    clean_file(path);

    let mut writer = TvcWriter::new(path, 1u32, 50, 9).unwrap();
    let start_ts = 1_000_000_000u64;

    // 100 ticks, 10 buckets = 10 ticks per bucket
    for i in 0..100u64 {
        let side = if i < 50 { 0 } else { 1 }; // First 50 buy, next 50 sell
        let tick = TradeTick::new(
            start_ts + i * 1000,
            100000i64 + (i as i64) * 10,
            100i64,
            side,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let buffer = RingBuffer::open(path, 1u64).unwrap();
    let tick_buffer = TickBuffer::from_ring_buffer(&buffer, 10).unwrap();

    assert_eq!(tick_buffer.num_ticks(), 100);
    assert_eq!(tick_buffer.bucket_size(), 10);

    // First 5 buckets (0-4) should have high VPIN (all buys)
    for i in 0..5 {
        let vpin = tick_buffer.bucket_vpin(i).unwrap();
        assert!(
            vpin > 0.9,
            "bucket {} VPIN should be > 0.9 (all buys), got {}",
            i,
            vpin
        );
    }

    // Last 5 buckets (5-9) should have VPIN approaching 0 as sells accumulate
    // Bucket 9: cumulative buy=5000, sell=5000 → VPIN=0
    let vpin9 = tick_buffer.bucket_vpin(9).unwrap();
    assert!(
        vpin9 < 0.1,
        "bucket 9 VPIN should be < 0.1 (mixed), got {}",
        vpin9
    );

    clean_file(path);
}

// =============================================================================
// Tick Iteration Tests
// =============================================================================

#[test]
fn test_tick_buffer_iter() {
    let path = Path::new("/tmp/test_tb_iter.tvc");
    clean_file(path);

    let mut writer = TvcWriter::new(path, 1u32, 10, 9).unwrap();
    let start_ts = 1_000_000_000u64;

    for i in 0..50u64 {
        let tick = TradeTick::new(
            start_ts + i * 1000,
            100000i64 + (i as i64) * 10,
            1000i64,
            (i % 2) as u8,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let rb = RingBuffer::open(path, 1u64).unwrap();
    let tb = TickBuffer::from_ring_buffer(&rb, 5).unwrap();

    let ticks: Vec<_> = tb.iter().collect();
    assert_eq!(ticks.len(), 50);

    // Verify first tick
    let first = ticks[0];
    assert_eq!(first.timestamp_ns, start_ts);
    assert_eq!(first.side, 0); // First tick is buy

    // Verify last tick
    let last = ticks[49];
    assert_eq!(last.timestamp_ns, start_ts + 49 * 1000);
    assert_eq!(last.side, 1); // 49 is odd, sell

    clean_file(path);
}

#[test]
fn test_vpin_at_timestamp() {
    let path = Path::new("/tmp/test_vpin_at_ts.tvc");
    clean_file(path);

    let mut writer = TvcWriter::new(path, 1u32, 10, 9).unwrap();
    let start_ts = 1_000_000_000u64;

    for i in 0..100u64 {
        let tick = TradeTick::new(
            start_ts + i * 1000,
            100000i64 + (i as i64) * 10,
            100i64,
            (i % 2) as u8,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let buffer = RingBuffer::open(path, 1u64).unwrap();
    let tick_buffer = TickBuffer::from_ring_buffer(&buffer, 10).unwrap();

    // Test VPIN at a known timestamp
    let ts = start_ts + 50 * 1000; // Tick 50
    let vpin = tick_buffer.vpin_at_timestamp(ts);
    assert!(vpin.is_some(), "Should find VPIN for timestamp {}", ts);

    clean_file(path);
}
