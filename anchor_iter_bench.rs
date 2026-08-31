#!/usr/bin/env cargo +nightly -Zscript
---
[dependencies]
tvc = { path = "/home/shadowarch/Nexus/libs/tvc" }
nexus = { path = "/home/shadowarch/Nexus/libs/nexus" }
nexus_strategy = { path = "/home/shadowarch/Nexus/libs/nexus_strategy" }
---
use std::time::Instant;
use std::sync::Arc;

fn main() {
    let n_ticks = 500_000;
    println!("Generating {} synthetic ticks...", n_ticks);

    let path = std::path::PathBuf::from(format!(
        "/tmp/anchor_iter_bench_{}.tvc",
        std::process::id()
    ));
    let instrument_id = nexus::InstrumentId::new("BTCUSDT", "BINANCE");

    {
        use tvc::TvcWriter;
        use tvc::TradeTick;
        let mut writer = TvcWriter::new(&path, 1u32, 10, 9).unwrap();
        let base_price = 50_000i64 * 1_000_000_000;
        let start_ts = 1_700_000_000_000_000_000u64;
        let tick_interval = 1_000_000_000u64;
        let mut price = base_price;
        let mut seq = 0u32;
        for i in 0..n_ticks {
            let noise = ((i as i64 % 100) - 50) * 100_000_000;
            price += noise;
            let tick = TradeTick::new(
                start_ts + (i as u64) * tick_interval,
                price,
                1_000_000_000i64,
                (i % 2) as u8,
                1,
                seq,
            );
            writer.write_tick(&tick).unwrap();
            seq += 1;
        }
        writer.finalize().unwrap();
    }

    println!("File size: {:.2} MB", path.metadata().unwrap().len() as f64 / 1_048_576.0);

    // Load RingBufferSet
    let buffer_set = Arc::new(
        nexus::buffer::RingBufferSet::from_files([(path.clone(), instrument_id)])
            .expect("Failed to load buffer set")
    );

    let num_anchors = buffer_set.num_anchors();
    let total_ticks = buffer_set.total_ticks();
    println!("Loaded: {} ticks, {} anchors", total_ticks, num_anchors);

    // Pre-load into TickBufferSet too
    let tick_buffer_set = nexus::buffer::TickBufferSet::from_files([(path.clone(), instrument_id.clone())])
        .expect("Failed to load tick buffer set");

    println!("\n=== Benchmark ===\n");

    // ---- Method 1: anchor_iter (new) ----
    println!("anchor_iter (new):");
    let mut tick_count = 0u64;
    let start = Instant::now();
    for (_buffer, _byte_offset, _local_tick_index, _anchor_slot, _instrument_id, mut ring_iter) in buffer_set.anchor_iter() {
        while ring_iter.next().is_some() {
            tick_count += 1;
        }
    }
    let anchor_iter_time = start.elapsed();
    println!("  Ticks: {} in {:.2} ms", tick_count, anchor_iter_time.as_secs_f64() * 1000.0);
    println!("  Throughput: {:.0} ticks/sec", tick_count as f64 / anchor_iter_time.as_secs_f64());

    // ---- Method 2: iter_state_from_global_tick (old) ----
    println!("\niter_state_from_global_tick (old):");
    let mut tick_count2 = 0u64;
    let start = Instant::now();
    let total = buffer_set.total_ticks();
    for global_tick in 0..total {
        let Some((buffer, offset, _tick_idx, _anchor_slot)) =
            buffer_set.iter_state_from_global_tick(global_tick)
        else {
            continue;
        };
        if buffer.decode_anchor_at(offset).is_ok() {
            tick_count2 += 1;
        }
    }
    let old_time = start.elapsed();
    println!("  Ticks: {} in {:.2} ms", tick_count2, old_time.as_secs_f64() * 1000.0);
    println!("  Throughput: {:.0} ticks/sec", tick_count2 as f64 / old_time.as_secs_f64());

    // ---- Method 3: merge_cursor (TickBufferSet) ----
    println!("\nmerge_cursor (TickBufferSet):");
    let mut tick_count3 = 0u64;
    let start = Instant::now();
    let mut cursor = tick_buffer_set.merge_cursor();
    while cursor.next().is_some() {
        tick_count3 += 1;
    }
    let merge_time = start.elapsed();
    println!("  Ticks: {} in {:.2} ms", tick_count3, merge_time.as_secs_f64() * 1000.0);
    println!("  Throughput: {:.0} ticks/sec", tick_count3 as f64 / merge_time.as_secs_f64());

    // ---- Summary ----
    println!("\n=== Summary ===");
    let speedup_anchor = old_time.as_secs_f64() / anchor_iter_time.as_secs_f64();
    let speedup_merge = old_time.as_secs_f64() / merge_time.as_secs_f64();
    println!("anchor_iter vs old:  {:.1}x faster", speedup_anchor);
    println!("merge_cursor vs old: {:.1}x faster", speedup_merge);

    let _ = std::fs::remove_file(&path);
}