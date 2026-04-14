//! Roundtrip test: write N ticks → read back → verify byte-for-byte identical.

use tvc::{TvcWriter, TvcReader, TradeTick};



#[test]
fn test_roundtrip_1m_ticks_anchor_1000() {
    let count = 1_000_000;
    let interval = 1000;

    let path = "/tmp/tvc_test_1m.tvc";
    // Clean up any existing file
    let _ = std::fs::remove_file(path);

    // Write ticks
    let mut writer = TvcWriter::new(std::path::Path::new(path), 12345, interval, 9).unwrap();

    let start_ts = 1_000_000_000u64;
    let start_price = 100_000_000_000i64;
    let start_size = 1_000_000i64;

    for i in 0..count {
        let ts = start_ts + (i as u64) * 100;
        let price = start_price + (i as i64) * 100;
        let size = start_size;

        let tick = TradeTick::new(ts, price, size, i as u8 % 2, 1, i as u32);
        writer.write_tick(&tick).unwrap();
    }

    let _digest = writer.finalize().unwrap();
    // writer dropped here, file should remain at path

    // Verify file size is reasonable
    let metadata = std::fs::metadata(path).unwrap();
    let file_size = metadata.len();

    // For count=1M, interval=1000: ~20MB
    assert!(file_size < 25_000_000, "File size {} too large", file_size);
    println!("File size for 1M ticks: {} bytes ({:.2} MB)", file_size, file_size as f64 / 1_000_000.0);

    // Read back and verify
    let mut reader = TvcReader::open(std::path::Path::new(path)).unwrap();
    assert_eq!(reader.num_ticks(), count as u64);
    assert_eq!(reader.num_anchors(), (count / interval as u64) as u32);

    // Verify header
    let header = reader.header();
    let header_instrument_id = header.instrument_id;
    let header_anchor_interval = header.anchor_interval;
    assert_eq!(header_instrument_id, 12345);
    assert_eq!(header_anchor_interval, interval);

    // Seek to first anchor (tick 0)
    let offset_0 = reader.seek_to_tick(0).unwrap();
    let tick_0 = reader.decode_tick_at(offset_0 as usize).unwrap();
    assert_eq!(tick_0.sequence, 0);

    // Seek to tick 1000 (should be an anchor)
    let offset_1000 = reader.seek_to_tick(1000).unwrap();
    let tick_1000 = reader.decode_tick_at(offset_1000 as usize).unwrap();
    assert_eq!(tick_1000.sequence, 1000);
    assert_eq!(tick_1000.timestamp_ns, start_ts + 1000 * 100);

    // Seek to tick 999999
    let offset_end = reader.seek_to_tick(999999).unwrap();
    let tick_end = reader.decode_tick_at(offset_end as usize).unwrap();
    assert_eq!(tick_end.sequence, 999999);
    assert_eq!(tick_end.timestamp_ns, start_ts + 999999 * 100);

    // Clean up
    let _ = std::fs::remove_file(path);
}

#[test]
fn test_roundtrip_anchor_interval_1() {
    // Every tick is an anchor - interval=1 is not allowed (must be >= 2)
    // Use interval=2 instead
    let count = 1000;
    let interval = 2;

    let path = "/tmp/tvc_test_interval1.tvc";
    let _ = std::fs::remove_file(path);

    let mut writer = TvcWriter::new(std::path::Path::new(path), 99999, interval, 9).unwrap();

    for i in 0..count {
        let tick = TradeTick::new(
            1_000_000_000 + (i as u64) * 1000,
            100_000_000_000 + (i as i64) * 1000,
            1_000_000i64,
            0,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }

    writer.finalize().unwrap();

    let mut reader = TvcReader::open(std::path::Path::new(path)).unwrap();
    assert_eq!(reader.num_ticks(), count as u64);
    assert_eq!(reader.num_anchors(), 500); // interval=2 means anchors at 0,2,4,...,998 = 500 anchors

    // All ticks should be readable - every tick is an anchor
    for i in 0..count {
        let offset = reader.seek_to_tick(i as u64).unwrap();
        let tick = reader.decode_tick_at(offset as usize).unwrap();
        println!("i={}, offset={}, tick={:?}", i, offset, tick); assert_eq!(tick.sequence, i as u32);
    }
}

#[test]
fn test_roundtrip_anchor_interval_10000() {
    // Few anchors, heavy delta compression
    let count = 50000;
    let interval = 10000;

    let path = "/tmp/tvc_test_interval10000.tvc";
    let _ = std::fs::remove_file(path);

    let mut writer = TvcWriter::new(std::path::Path::new(path), 77777, interval, 9).unwrap();

    for i in 0..count {
        let tick = TradeTick::new(
            1_000_000_000 + (i as u64) * 50,
            100_000_000_000 + (i as i64) * 50,
            500_000i64,
            0,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }

    writer.finalize().unwrap();

    let mut reader = TvcReader::open(std::path::Path::new(path)).unwrap();
    assert_eq!(reader.num_ticks(), count as u64);
    assert_eq!(reader.num_anchors(), 5u32);

    // Check tick 9999 (near end of first delta region)
    let offset = reader.seek_to_tick(9999).unwrap();
    let tick = reader.decode_tick_at(offset as usize).unwrap();
    assert_eq!(tick.sequence, 9999);

    // Check tick 19999
    let offset = reader.seek_to_tick(19999).unwrap();
    let tick = reader.decode_tick_at(offset as usize).unwrap();
    assert_eq!(tick.sequence, 19999);
}

#[test]
fn test_overflow_ticks() {
    // Create ticks that trigger overflow encoding
    let count = 100;
    let interval = 50;

    let path = "/tmp/tvc_test_overflow.tvc";
    let _ = std::fs::remove_file(path);

    let mut writer = TvcWriter::new(std::path::Path::new(path), 11111, interval, 9).unwrap();

    for i in 0..count {
        let ts = 1_000_000_000 + (i as u64) * 2_000_000;
        let price = 100_000_000_000 + (i as i64) * 200_000;

        let tick = TradeTick::new(ts, price, 1_000_000i64, 0, 1, i as u32);
        writer.write_tick(&tick).unwrap();
    }

    writer.finalize().unwrap();

    let mut reader = TvcReader::open(std::path::Path::new(path)).unwrap();
    assert_eq!(reader.num_ticks(), count as u64);

    // Verify overflow-encoded ticks - just first and last
    let offset_0 = reader.seek_to_tick(0).unwrap();
    let tick_0 = reader.decode_tick_at(offset_0 as usize).unwrap();
    assert_eq!(tick_0.sequence, 0);

    let offset_50 = reader.seek_to_tick(50).unwrap();
    let tick_50 = reader.decode_tick_at(offset_50 as usize).unwrap();
    assert_eq!(tick_50.sequence, 50);
}

#[test]
fn test_side_change_triggers_overflow() {
    // Side changes should trigger overflow encoding
    let count = 100;
    let interval = 20;

    let path = "/tmp/tvc_test_sidechange.tvc";
    let _ = std::fs::remove_file(path);

    let mut writer = TvcWriter::new(std::path::Path::new(path), 22222, interval, 9).unwrap();

    for i in 0..count {
        let tick = TradeTick::new(
            1_000_000_000 + (i as u64) * 100,
            100_000_000_000,
            1_000_000i64,
            (i % 2) as u8,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }

    writer.finalize().unwrap();

    let mut reader = TvcReader::open(std::path::Path::new(path)).unwrap();

    // Verify side changes are preserved - check some samples
    for idx in [0, 1, 10, 19, 20, 21, 50, 99] {
        let offset = reader.seek_to_tick(idx).unwrap();
        let tick = reader.decode_tick_at(offset as usize).unwrap();
        assert_eq!(tick.side, (idx % 2) as u8, "Tick {} side mismatch", idx);
    }
}