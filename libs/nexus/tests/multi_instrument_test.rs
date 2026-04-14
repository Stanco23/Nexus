//! Integration tests for TickBufferSet and multi-instrument data handling.

use nexus::buffer::{InstrumentId, MergeCursor, TickBufferSet};
use std::fs;
use std::path::Path;
use tvc::{TradeTick, TvcWriter};

fn clean_file(path: &Path) {
    let _ = fs::remove_file(path);
}

// =============================================================================
// TickBufferSet Tests
// =============================================================================

#[test]
fn test_tick_buffer_set_from_files() {
    let path1 = Path::new("/tmp/test_multi_inst1.tvc");
    let path2 = Path::new("/tmp/test_multi_inst2.tvc");
    clean_file(path1);
    clean_file(path2);

    // Create two TVC files with non-overlapping timestamps
    let mut writer1 = TvcWriter::new(path1, 1u32, 10, 9).unwrap();
    let mut writer2 = TvcWriter::new(path2, 2u32, 10, 9).unwrap();

    let start_ts1 = 1_000_000_000u64;
    let start_ts2 = 1_000_500_000u64; // Instrument 2 starts 500ms later

    // Instrument 1: 10 ticks at 100ms intervals
    for i in 0..10u64 {
        let tick = TradeTick::new(
            start_ts1 + i * 100_000_000,
            100000i64 + (i as i64) * 10,
            1000i64,
            0,
            1,
            i as u32,
        );
        writer1.write_tick(&tick).unwrap();
    }
    writer1.finalize().unwrap();

    // Instrument 2: 10 ticks at 100ms intervals
    for i in 0..10u64 {
        let tick = TradeTick::new(
            start_ts2 + i * 100_000_000,
            200000i64 + (i as i64) * 20,
            2000i64,
            1,
            1,
            i as u32,
        );
        writer2.write_tick(&tick).unwrap();
    }
    writer2.finalize().unwrap();

    // Create TickBufferSet from files
    let buffer_set =
        TickBufferSet::from_files([(path1.to_path_buf(), 1u64), (path2.to_path_buf(), 2u64)])
            .unwrap();

    assert_eq!(buffer_set.num_instruments(), 2);
    assert!(buffer_set.get(1u64).is_some());
    assert!(buffer_set.get(2u64).is_some());
    assert!(buffer_set.get(3u64).is_none()); // Non-existent

    clean_file(path1);
    clean_file(path2);
}

#[test]
fn test_merge_cursor_time_ordering() {
    let path1 = Path::new("/tmp/test_merge1.tvc");
    let path2 = Path::new("/tmp/test_merge2.tvc");
    clean_file(path1);
    clean_file(path2);

    // Create two TVC files with interleaved timestamps
    // Instrument 1: timestamps at 1000, 3000, 5000, 7000, 9000
    // Instrument 2: timestamps at 2000, 4000, 6000, 8000, 10000
    // Expected merged order: 1000, 2000, 3000, 4000, 5000, 6000, 7000, 8000, 9000, 10000

    let mut writer1 = TvcWriter::new(path1, 1u32, 10, 9).unwrap();
    let mut writer2 = TvcWriter::new(path2, 2u32, 10, 9).unwrap();

    let start_ts = 1_000_000_000u64;

    for i in 0..5u64 {
        let tick1 = TradeTick::new(
            start_ts + (i * 2 + 1) * 1_000_000_000, // 1, 3, 5, 7, 9 billion ns
            100000i64 + (i as i64) * 100,
            1000i64,
            0,
            1,
            i as u32,
        );
        writer1.write_tick(&tick1).unwrap();

        let tick2 = TradeTick::new(
            start_ts + (i * 2 + 2) * 1_000_000_000, // 2, 4, 6, 8, 10 billion ns
            200000i64 + (i as i64) * 200,
            2000i64,
            1,
            1,
            i as u32,
        );
        writer2.write_tick(&tick2).unwrap();
    }
    writer1.finalize().unwrap();
    writer2.finalize().unwrap();

    let buffer_set =
        TickBufferSet::from_files([(path1.to_path_buf(), 1u64), (path2.to_path_buf(), 2u64)])
            .unwrap();

    let mut cursor = buffer_set.merge_cursor();

    // Verify time-ordered delivery
    let mut expected_ts = start_ts + 1_000_000_000;
    let mut count = 0;

    while let Some(event) = cursor.next() {
        assert_eq!(
            event.tick.timestamp_ns, expected_ts,
            "Event {} should have timestamp {}, got {}",
            count, expected_ts, event.tick.timestamp_ns
        );
        expected_ts += 1_000_000_000;
        count += 1;
    }

    assert_eq!(count, 10, "Should have 10 events total");

    clean_file(path1);
    clean_file(path2);
}

#[test]
fn test_merge_cursor_peek() {
    let path1 = Path::new("/tmp/test_peek1.tvc");
    let path2 = Path::new("/tmp/test_peek2.tvc");
    clean_file(path1);
    clean_file(path2);

    let mut writer1 = TvcWriter::new(path1, 1u32, 10, 9).unwrap();
    let mut writer2 = TvcWriter::new(path2, 2u32, 10, 9).unwrap();

    let start_ts = 1_000_000_000u64;

    for i in 0..3u64 {
        let tick1 = TradeTick::new(
            start_ts + i * 2_000_000_000,
            100000i64,
            1000i64,
            0,
            1,
            i as u32,
        );
        writer1.write_tick(&tick1).unwrap();

        let tick2 = TradeTick::new(
            start_ts + (i * 2 + 1) * 1_000_000_000,
            200000i64,
            2000i64,
            1,
            1,
            i as u32,
        );
        writer2.write_tick(&tick2).unwrap();
    }
    writer1.finalize().unwrap();
    writer2.finalize().unwrap();

    let buffer_set =
        TickBufferSet::from_files([(path1.to_path_buf(), 1u64), (path2.to_path_buf(), 2u64)])
            .unwrap();

    let mut cursor = buffer_set.merge_cursor();

    // First peek should return the earliest event
    let first = cursor.peek();
    assert!(first.is_some());
    assert_eq!(first.unwrap().instrument_id, 1u64);
    assert_eq!(first.unwrap().tick.timestamp_ns, start_ts);

    // Peek again without advancing should return same event
    let second = cursor.peek();
    assert!(second.is_some());
    assert_eq!(second.unwrap().tick.timestamp_ns, start_ts);

    // Now advance
    let advanced = cursor.next();
    assert!(advanced.is_some());
    assert_eq!(advanced.unwrap().tick.timestamp_ns, start_ts);

    // Next peek should show the second event
    let third = cursor.peek();
    assert!(third.is_some());
    assert_eq!(third.unwrap().instrument_id, 2u64);
    assert_eq!(third.unwrap().tick.timestamp_ns, start_ts + 1_000_000_000);

    clean_file(path1);
    clean_file(path2);
}

#[test]
fn test_merge_cursor_single_instrument() {
    let path = Path::new("/tmp/test_single.tvc");
    clean_file(path);

    let mut writer = TvcWriter::new(path, 1u32, 10, 9).unwrap();
    let start_ts = 1_000_000_000u64;

    for i in 0..5u64 {
        let tick = TradeTick::new(
            start_ts + i * 1_000_000_000,
            100000i64 + (i as i64) * 100,
            1000i64,
            (i % 2) as u8,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let buffer_set = TickBufferSet::from_files([(path.to_path_buf(), 1u64)]).unwrap();

    let mut cursor = buffer_set.merge_cursor();
    let mut count = 0;

    while let Some(event) = cursor.next() {
        assert_eq!(event.instrument_id, 1u64);
        count += 1;
    }

    assert_eq!(count, 5);

    clean_file(path);
}

#[test]
fn test_merge_cursor_has_next() {
    let path = Path::new("/tmp/test_has_next.tvc");
    clean_file(path);

    let mut writer = TvcWriter::new(path, 1u32, 5, 9).unwrap();
    let start_ts = 1_000_000_000u64;

    for i in 0..3u64 {
        let tick = TradeTick::new(
            start_ts + i * 1_000_000_000,
            100000i64,
            1000i64,
            0,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let buffer_set = TickBufferSet::from_files([(path.to_path_buf(), 1u64)]).unwrap();

    let mut cursor = buffer_set.merge_cursor();

    assert!(cursor.has_next());
    cursor.next().unwrap();
    assert!(cursor.has_next());
    cursor.next().unwrap();
    assert!(cursor.has_next());
    cursor.next().unwrap();
    assert!(!cursor.has_next());

    clean_file(path);
}

#[test]
fn test_tick_buffer_set_num_instruments() {
    let path1 = Path::new("/tmp/test_num1.tvc");
    let path2 = Path::new("/tmp/test_num2.tvc");
    let path3 = Path::new("/tmp/test_num3.tvc");
    clean_file(path1);
    clean_file(path2);
    clean_file(path3);

    let mut create_file = |p: &Path, id: u64| {
        let mut w = TvcWriter::new(p, id as u32, 10, 9).unwrap();
        let ts = 1_000_000_000u64 + id * 1_000_000_000;
        for i in 0..5u64 {
            let tick = TradeTick::new(
                ts + i * 1_000_000_000,
                100000i64 * (id as i64),
                1000i64,
                0,
                1,
                i as u32,
            );
            w.write_tick(&tick).unwrap();
        }
        w.finalize().unwrap();
    };

    create_file(path1, 1);
    create_file(path2, 2);
    create_file(path3, 3);

    let buffer_set = TickBufferSet::from_files([
        (path1.to_path_buf(), 1u64),
        (path2.to_path_buf(), 2u64),
        (path3.to_path_buf(), 3u64),
    ])
    .unwrap();

    assert_eq!(buffer_set.num_instruments(), 3);

    let ids = buffer_set.instrument_ids();
    assert_eq!(ids.len(), 3);

    clean_file(path1);
    clean_file(path2);
    clean_file(path3);
}
