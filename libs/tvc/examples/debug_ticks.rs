// Debug test for roundtrip
use tvc::{TvcWriter, TvcReader, TradeTick};
use std::fs;

fn main() {
    let path = "/tmp/tvc_debug_test.tvc";
    let _ = fs::remove_file(path);

    let count = 20;
    let interval = 2;

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

    println!("num_ticks: {}", reader.num_ticks());
    println!("num_anchors: {}", reader.num_anchors());

    println!("\nAnchor index:");
    for (i, entry) in reader.anchor_index().iter().enumerate() {
        println!("  {}: tick_index={}, byte_offset={}", i, {entry.tick_index}, {entry.byte_offset});
    }

    println!("\nDecoding ticks:");
    for i in 0..count.min(10) {
        let offset = reader.seek_to_tick(i as u64).unwrap();
        let tick = reader.decode_tick_at(offset as usize).unwrap();
        println!("  tick {}: sequence={}, offset={}", i, tick.sequence, offset);
    }

    let _ = fs::remove_file(path);
}