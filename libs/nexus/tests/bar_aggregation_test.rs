//! Integration tests for Bar Aggregation using actual TVC files.

use nexus::buffer::{BarBuffer, BarPeriod, RingBuffer, TickBuffer};
use std::fs;
use std::path::Path;
use tvc::{TradeTick, TvcWriter};

fn clean_file(path: &Path) {
    let _ = fs::remove_file(path);
}

// =============================================================================
// BarBuffer Tests
// =============================================================================

#[test]
fn test_bar_buffer_from_tick_buffer() {
    let path = Path::new("/tmp/test_bar_buffer.tvc");
    clean_file(path);

    // Create TVC with 100 ticks over 1 second interval
    let mut writer = TvcWriter::new(path, 1u32, 100, 9).unwrap();
    let start_ts = 1_000_000_000u64;

    for i in 0..100u64 {
        let tick = TradeTick::new(
            start_ts + i * 10_000_000, // 10ms intervals
            100000i64 + (i as i64),
            1000i64 + (i as i64 / 10),
            (i % 2) as u8,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let rb = RingBuffer::open(path, 1u64).unwrap();
    let tb = TickBuffer::from_ring_buffer(&rb, 10).unwrap();

    // Build 1-second bars
    let bar_buffer = BarBuffer::from_tick_buffer(&tb, BarPeriod::OneSecond);

    // With 100 ticks at 10ms intervals = 1000ms total
    // Should get ~1 bar (actually depends on aggregation)
    assert!(bar_buffer.num_bars() > 0);
    assert_eq!(bar_buffer.period(), BarPeriod::OneSecond);
    assert_eq!(bar_buffer.instrument_id(), 1u64);

    clean_file(path);
}

#[test]
fn test_bar_aggregation_1m_intervals() {
    let path = Path::new("/tmp/test_bar_1m.tvc");
    clean_file(path);

    // Create TVC with ticks at 1-minute intervals
    let mut writer = TvcWriter::new(path, 1u32, 10, 9).unwrap();
    let start_ts = 1_000_000_000u64;

    // 60 ticks at 1s intervals = 60 seconds = 1 minute of data
    for i in 0..60u64 {
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

    let rb = RingBuffer::open(path, 1u64).unwrap();
    let tb = TickBuffer::from_ring_buffer(&rb, 10).unwrap();

    // Build 1-second bars
    let bar_buffer = BarBuffer::from_tick_buffer(&tb, BarPeriod::OneSecond);

    // 60 ticks at 1s intervals should produce ~60 bars
    assert_eq!(bar_buffer.num_bars(), 60);

    // Verify first bar
    let first = bar_buffer.get(0).unwrap();
    assert_eq!(first.timestamp_ns, start_ts);
    assert_eq!(first.open, 100000);

    // Verify last bar
    let last = bar_buffer.get(59).unwrap();
    assert_eq!(last.timestamp_ns, start_ts + 59_000_000_000);
    assert_eq!(last.close, 100000 + 59 * 100);

    clean_file(path);
}

// =============================================================================
// Bar Aggregation with Known Tick Pattern Tests
// =============================================================================

#[test]
fn test_bar_high_low_tracking() {
    let path = Path::new("/tmp/test_bar_hl.tvc");
    clean_file(path);

    let mut writer = TvcWriter::new(path, 1u32, 10, 9).unwrap();
    let start_ts = 1_000_000_000u64;

    // Ticks in first second: 100, 105, 95, 110
    let prices = [100000, 105000, 95000, 110000];
    for (i, price) in prices.iter().enumerate() {
        let tick = TradeTick::new(
            start_ts + (i as u64) * 250_000_000, // 250ms apart
            *price,
            1000i64,
            0,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let rb = RingBuffer::open(path, 1u64).unwrap();
    let tb = TickBuffer::from_ring_buffer(&rb, 10).unwrap();
    let bar_buffer = BarBuffer::from_tick_buffer(&tb, BarPeriod::OneSecond);

    let bar = bar_buffer.get(0).unwrap();
    assert_eq!(bar.open, 100000);
    assert_eq!(bar.high, 110000); // max of 100, 105, 95, 110
    assert_eq!(bar.low, 95000); // min of 100, 105, 95, 110
    assert_eq!(bar.close, 110000); // last price

    clean_file(path);
}

#[test]
fn test_bar_buy_sell_volume() {
    let path = Path::new("/tmp/test_bar_vol.tvc");
    clean_file(path);

    let mut writer = TvcWriter::new(path, 1u32, 10, 9).unwrap();
    let start_ts = 1_000_000_000u64;

    // 5 buys of 100, then 5 sells of 100
    for i in 0..10u64 {
        let side = if i < 5 { 0 } else { 1 };
        let tick = TradeTick::new(
            start_ts + (i as u64) * 100_000_000, // 100ms apart
            100000i64,
            100i64,
            side,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let rb = RingBuffer::open(path, 1u64).unwrap();
    let tb = TickBuffer::from_ring_buffer(&rb, 10).unwrap();
    let bar_buffer = BarBuffer::from_tick_buffer(&tb, BarPeriod::OneSecond);

    // All 10 ticks in 1 second = 1 bar
    assert_eq!(bar_buffer.num_bars(), 1);
    let bar = bar_buffer.get(0).unwrap();
    assert_eq!(bar.volume, 1000); // 10 * 100
    assert_eq!(bar.buy_volume, 500); // 5 buys * 100
    assert_eq!(bar.sell_volume, 500); // 5 sells * 100

    clean_file(path);
}

#[test]
fn test_bar_iteration() {
    let path = Path::new("/tmp/test_bar_iter.tvc");
    clean_file(path);

    let mut writer = TvcWriter::new(path, 1u32, 10, 9).unwrap();
    let start_ts = 1_000_000_000u64;

    for i in 0..20u64 {
        let tick = TradeTick::new(
            start_ts + (i as u64) * 1_000_000_000,
            100000i64 + (i as i64) * 100,
            1000i64,
            0,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let rb = RingBuffer::open(path, 1u64).unwrap();
    let tb = TickBuffer::from_ring_buffer(&rb, 10).unwrap();
    let bar_buffer = BarBuffer::from_tick_buffer(&tb, BarPeriod::OneSecond);

    let bars: Vec<_> = bar_buffer.iter().collect();
    assert_eq!(bars.len(), 20);

    // Verify bar iteration preserves order
    for (i, bar) in bars.iter().enumerate() {
        assert_eq!(bar.timestamp_ns, start_ts + (i as u64) * 1_000_000_000);
    }

    clean_file(path);
}

// =============================================================================
// Bar Aggregation Accuracy Test
// =============================================================================

#[test]
fn test_bar_output_matches_manual() {
    let path = Path::new("/tmp/test_bar_manual.tvc");
    clean_file(path);

    let mut writer = TvcWriter::new(path, 1u32, 100, 9).unwrap();
    let start_ts = 1_000_000_000u64;

    // 100 ticks at 10ms intervals = 1 second of data
    for i in 0..100u64 {
        let tick = TradeTick::new(
            start_ts + i * 10_000_000,
            100000i64 + (i as i64) * 10,
            1000i64,
            0,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();

    let rb = RingBuffer::open(path, 1u64).unwrap();
    let tb = TickBuffer::from_ring_buffer(&rb, 10).unwrap();

    // Manually compute expected bar values
    // For 1-second bars: all 100 ticks fall in 1 bar
    // open = first price = 100000
    // high = max price = 100000 + 99*10 = 100990
    // low = min price = 100000
    // close = last price = 100000 + 99*10 = 100990
    // volume = 100 * 1000 = 100000

    let bar_buffer = BarBuffer::from_tick_buffer(&tb, BarPeriod::OneSecond);

    assert_eq!(bar_buffer.num_bars(), 1);
    let bar = bar_buffer.get(0).unwrap();
    assert_eq!(bar.open, 100000);
    assert_eq!(bar.high, 100990);
    assert_eq!(bar.low, 100000);
    assert_eq!(bar.close, 100990);
    assert_eq!(bar.volume, 100000);
    assert_eq!(bar.tick_count, 100);

    clean_file(path);
}

// =============================================================================
// Multi-period Tests
// =============================================================================

#[test]
fn test_multiple_bar_periods() {
    let path = Path::new("/tmp/test_multi_period.tvc");
    clean_file(path);

    let mut writer = TvcWriter::new(path, 1u32, 10, 9).unwrap();
    let start_ts = 1_000_000_000u64;

    // 120 ticks at 1s intervals = 2 minutes of data
    for i in 0..120u64 {
        let tick = TradeTick::new(
            start_ts + i * 1_000_000_000,
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
    let tb = TickBuffer::from_ring_buffer(&rb, 10).unwrap();

    // 1-minute bars: 120 ticks / 60 ticks per minute = 2 bars
    let bar_1m = BarBuffer::from_tick_buffer(&tb, BarPeriod::OneMinute);
    assert_eq!(
        bar_1m.num_bars(),
        3,
        "Expected 3 bars (2 full + 1 partial): bars 0, 60, 120"
    );

    // 5-minute bars: 120 ticks all fall in the same 5-min window (0-5 min)
    // We get 1 partial bar at the end
    let bar_5m = BarBuffer::from_tick_buffer(&tb, BarPeriod::FiveMinutes);
    assert_eq!(bar_5m.num_bars(), 1, "All 120 ticks fall in one 5-min bar");

    clean_file(path);
}
