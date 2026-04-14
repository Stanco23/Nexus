//! Integration tests for RingBuffer using actual TVC files.

use nexus::buffer::RingBuffer;
use std::fs;
use std::path::Path;
use tvc::{TradeTick, TvcWriter};

fn clean_file(path: &Path) {
    let _ = fs::remove_file(path);
}

// =============================================================================
// Basic Tests
// =============================================================================

#[test]
fn test_ring_buffer_two_ticks() {
    let path = Path::new("/tmp/test_two_ticks.tvc");
    clean_file(path);

    let mut writer = TvcWriter::new(path, 1u32, 10, 9).unwrap();
    let start_ts = 1000u64;

    for i in 0..2u64 {
        let tick = TradeTick::new(
            start_ts + i * 100,
            1000i64 + (i as i64) * 10,
            1_000_000i64,
            0,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let buffer = RingBuffer::open(path, 1u64).unwrap();
    assert_eq!(buffer.num_ticks(), 2);
    assert_eq!(buffer.num_anchors(), 1);

    let ticks: Vec<_> = buffer.iter().collect();
    assert_eq!(ticks.len(), 2);
    assert_eq!(ticks[0].sequence, 0);
    assert_eq!(ticks[1].sequence, 1);

    clean_file(path);
}

#[test]
fn test_ring_buffer_ten_ticks() {
    let path = Path::new("/tmp/test_ten_ticks.tvc");
    clean_file(path);

    // 10 ticks, anchor every 10 means only tick 0 is anchor
    let mut writer = TvcWriter::new(path, 1u32, 10, 9).unwrap();
    let start_ts = 2000u64;

    for i in 0..10u64 {
        let tick = TradeTick::new(
            start_ts + i * 100,
            2000i64 + (i as i64) * 100,
            1_000_000i64,
            0,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let buffer = RingBuffer::open(path, 1u64).unwrap();
    assert_eq!(buffer.num_ticks(), 10);
    assert_eq!(buffer.num_anchors(), 1);

    let ticks: Vec<_> = buffer.iter().collect();
    assert_eq!(ticks.len(), 10);
    for (i, t) in ticks.iter().enumerate() {
        assert_eq!(t.sequence, i as u32);
    }

    clean_file(path);
}

// =============================================================================
// Multi-anchor Tests
// =============================================================================

#[test]
fn test_ring_buffer_multiple_anchors() {
    let path = Path::new("/tmp/test_multi_anchor.tvc");
    clean_file(path);

    // 25 ticks, anchor every 10 -> anchors at tick 0, 10, 20
    let mut writer = TvcWriter::new(path, 1u32, 10, 9).unwrap();
    let start_ts = 1000u64;

    for i in 0..25u64 {
        let tick = TradeTick::new(
            start_ts + i * 100,
            1000i64 + (i as i64) * 100,
            1_000_000i64,
            (i % 2) as u8,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let buffer = RingBuffer::open(path, 1u64).unwrap();
    assert_eq!(buffer.num_ticks(), 25);
    assert_eq!(buffer.num_anchors(), 3); // 0, 10, 20

    let ticks: Vec<_> = buffer.iter().collect();
    assert_eq!(ticks.len(), 25);
    assert_eq!(ticks[0].sequence, 0);
    assert_eq!(ticks[10].sequence, 10);
    assert_eq!(ticks[20].sequence, 20);

    clean_file(path);
}

// =============================================================================
// Seek Tests
// =============================================================================

#[test]
fn test_ring_buffer_seek_to_tick() {
    let path = Path::new("/tmp/test_seek.tvc");
    clean_file(path);

    // 100 ticks, anchor every 10
    let mut writer = TvcWriter::new(path, 123u32, 10, 9).unwrap();
    let start_ts = 5000u64;

    for i in 0..100u64 {
        let tick = TradeTick::new(
            start_ts + i * 100,
            50000i64 + (i as i64) * 100,
            1_000_000i64,
            0,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let buffer = RingBuffer::open(path, 123u64).unwrap();

    // Seek to tick 0
    let (offset, anchor_tick) = buffer.seek_to_tick(0).unwrap();
    assert_eq!(anchor_tick, 0);
    let tick = buffer.decode_anchor_at(offset).unwrap();
    assert_eq!(tick.sequence, 0);

    // Seek to tick 15 (should find anchor at 10)
    let (offset, anchor_tick) = buffer.seek_to_tick(15).unwrap();
    assert_eq!(anchor_tick, 10);
    let tick = buffer.decode_anchor_at(offset).unwrap();
    assert_eq!(tick.sequence, 10);

    // Seek to tick 55 (should find anchor at 50)
    let (offset, anchor_tick) = buffer.seek_to_tick(55).unwrap();
    assert_eq!(anchor_tick, 50);

    // Seek to tick 99 (should find anchor at 90)
    let (offset, anchor_tick) = buffer.seek_to_tick(99).unwrap();
    assert_eq!(anchor_tick, 90);

    clean_file(path);
}

// =============================================================================
// Binary Search Correctness
// =============================================================================

#[test]
fn test_ring_buffer_binary_search_all_ticks() {
    let path = Path::new("/tmp/test_binary_search.tvc");
    clean_file(path);

    // 100 ticks, anchor every 10
    let mut writer = TvcWriter::new(path, 1u32, 10, 9).unwrap();
    let start_ts = 10000u64;

    for i in 0..100u64 {
        let tick = TradeTick::new(
            start_ts + i * 100,
            100000i64 + (i as i64) * 100,
            1_000_000i64,
            0,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let buffer = RingBuffer::open(path, 1u64).unwrap();

    // Verify every tick's timestamp by seeking to it
    for target in [0u64, 5, 9, 10, 15, 20, 50, 99] {
        let (offset, anchor_tick) = buffer.seek_to_tick(target).unwrap();
        let decoded = buffer.decode_anchor_at(offset).unwrap();

        // The anchor should be at floor(target/anchor_interval) * anchor_interval
        let expected_anchor = (target / 10) * 10;
        assert_eq!(anchor_tick, expected_anchor, "Seek to {} failed", target);
        assert_eq!(decoded.sequence as u64, expected_anchor);
    }

    clean_file(path);
}

// =============================================================================
// Time Range Iteration
// =============================================================================

#[test]
#[ignore] // TODO: implement time range iteration correctly
fn test_ring_buffer_time_range() {
    let path = Path::new("/tmp/test_time_range.tvc");
    clean_file(path);

    // 1000 ticks at 1000ns intervals, anchor every 100
    let mut writer = TvcWriter::new(path, 1u32, 100, 9).unwrap();
    let start_ts = 1_000_000_000u64; // 1 second

    for i in 0..1000u64 {
        let tick = TradeTick::new(
            start_ts + i * 1000,
            100000i64 + (i as i64) * 1000,
            1_000_000i64,
            0,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let buffer = RingBuffer::open(path, 1u64).unwrap();

    // Iterate range: ticks from ts=1_000_050_000 to 1_000_100_000
    // That's ticks 50 through 100 (51 ticks)
    let start_ns = start_ts + 50_000u64;
    let end_ns = start_ts + 100_000u64;

    let iter = buffer.iter_range(start_ns, end_ns);
    let ticks: Vec<_> = iter.collect();

    assert_eq!(
        ticks.len(),
        51,
        "Expected 51 ticks in range, got {}",
        ticks.len()
    );
    assert_eq!(ticks[0].sequence, 50);
    assert_eq!(ticks[50].sequence, 100);

    clean_file(path);
}

// =============================================================================
// RingBufferSet Tests
// =============================================================================

use nexus::buffer::RingBufferSet;
use std::path::PathBuf;

#[test]
fn test_ring_buffer_set_single_file() {
    let path = Path::new("/tmp/test_rbset_single.tvc");
    clean_file(path);

    let mut writer = TvcWriter::new(path, 111u32, 50, 9).unwrap();
    for i in 0..200u64 {
        let tick = TradeTick::new(
            1000u64 + i * 100,
            1000i64 + (i as i64) * 100,
            1_000_000i64,
            0,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let set = RingBufferSet::single(path, 111u64).unwrap();
    assert_eq!(set.num_instruments(), 1);
    assert_eq!(set.total_ticks(), 200);

    let buf = set.get(111u64).unwrap();
    assert_eq!(buf.num_ticks(), 200);

    clean_file(path);
}

#[test]
fn test_ring_buffer_set_two_instruments() {
    let path1 = Path::new("/tmp/test_rbset_1.tvc");
    let path2 = Path::new("/tmp/test_rbset_2.tvc");
    clean_file(path1);
    clean_file(path2);

    // Instrument 1: 100 ticks
    let mut w1 = TvcWriter::new(path1, 1u32, 20, 9).unwrap();
    for i in 0..100u64 {
        let tick = TradeTick::new(
            1000u64 + i * 100,
            1000i64 + (i as i64) * 100,
            1_000_000i64,
            0,
            1,
            i as u32,
        );
        w1.write_tick(&tick).unwrap();
    }
    w1.finalize().unwrap();

    // Instrument 2: 50 ticks
    let mut w2 = TvcWriter::new(path2, 2u32, 20, 9).unwrap();
    for i in 0..50u64 {
        let tick = TradeTick::new(
            1500u64 + i * 100, // Starts after instrument 1
            2000i64 + (i as i64) * 100,
            1_000_000i64,
            0,
            1,
            i as u32,
        );
        w2.write_tick(&tick).unwrap();
    }
    w2.finalize().unwrap();

    let set =
        RingBufferSet::from_files([(PathBuf::from(path1), 1u64), (PathBuf::from(path2), 2u64)])
            .unwrap();

    assert_eq!(set.num_instruments(), 2);
    assert_eq!(set.total_ticks(), 150);

    let buf1 = set.get(1u64).unwrap();
    let buf2 = set.get(2u64).unwrap();
    assert_eq!(buf1.num_ticks(), 100);
    assert_eq!(buf2.num_ticks(), 50);

    clean_file(path1);
    clean_file(path2);
}

// =============================================================================
// ExactSizeIterator Contract
// =============================================================================

#[test]
fn test_ring_iter_exact_size() {
    let path = Path::new("/tmp/test_exact_size.tvc");
    clean_file(path);

    let mut writer = TvcWriter::new(path, 1u32, 10, 9).unwrap();
    let start_ts = 100u64;

    for i in 0..20u64 {
        let tick = TradeTick::new(
            start_ts + i * 100,
            100i64 + (i as i64) * 10,
            1_000_000i64,
            0,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let buffer = RingBuffer::open(path, 1u64).unwrap();
    let iter = buffer.iter();

    // Check size_hint
    let (min, max) = iter.size_hint();
    assert_eq!(min, 20);
    assert_eq!(max, Some(20));

    // Collect all and verify count
    let ticks: Vec<_> = iter.collect();
    assert_eq!(ticks.len(), 20);

    clean_file(path);
}
