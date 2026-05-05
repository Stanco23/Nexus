use nexus::buffer::buffer_set::RingBufferSet;
use nexus::instrument::InstrumentId;
use std::path::PathBuf;
use chrono::{NaiveDate, Duration, TimeZone, Utc, Timelike};

fn main() {
    let data_dir = PathBuf::from("/home/shadowarch/Nexus/data");
    let instrument_id = InstrumentId::new("BTCUSDT", "BINANCE");
    
    let mut files: Vec<_> = std::fs::read_dir(&data_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let path = e.path();
            let stem = path.file_stem().unwrap().to_string_lossy();
            stem.starts_with("BTCUSDT") && path.extension().map_or(false, |e| e == "tvc")
        })
        .map(|e| (e.path(), instrument_id.clone()))
        .collect();
    
    let mut file_times: Vec<_> = files.iter()
        .map(|(p, id)| {
            let rb = nexus::buffer::ring_buffer::RingBuffer::open(p, id.clone()).unwrap();
            (p.clone(), id.clone(), rb.start_time_ns())
        })
        .collect();
    file_times.sort_by_key(|(_, _, t)| *t);
    files = file_times.into_iter().map(|(p, id, _)| (p, id)).collect();
    
    let buffer_set = RingBufferSet::from_files(files).expect("Failed to load");
    let buffers = buffer_set.buffers();
    let anchors = buffer_set.merged_anchors();
    
    // === Buffer summary ===
    println!("\n=== Buffer summary ===");
    for (i, (id, buf)) in buffers.iter().enumerate() {
        println!("  buffer[{}]: instrument={}, num_ticks={}, num_anchors={}, anchor_interval={}",
            i, id.symbol, buf.num_ticks(), buf.anchor_index().len(), buf.anchor_interval());
    }
    println!("  total_ticks={}", buffer_set.total_ticks());
    println!("  merged_anchors.len={}", anchors.len());
    
    // The key question: what is the global_tick_index of merged_anchors[569]?
    // If merged_anchors is sorted by global_tick_index, then anchor[i].global_tick_index should == i (approximately)
    
    // Let's check if merged_anchors[i].global_tick_index == i for small i
    println!("\nChecking if merged_anchors[i].global_tick_index == i for i=0..10:");
    for i in 0..10 {
        if let Some(a) = anchors.get(i) {
            println!("  anchor[{}]: global_tick={}, expected={}", i, a.global_tick_index, i);
        }
    }
    
    // Now check anchor[300]
    println!("\nanchor[300]: global_tick={} (expected ~300)", anchors.get(300).map(|a| a.global_tick_index).unwrap_or(0));
    println!("anchor[298]: global_tick={} (expected ~298)", anchors.get(298).map(|a| a.global_tick_index).unwrap_or(0));
    println!("anchor[299]: global_tick={} (expected ~299)", anchors.get(299).map(|a| a.global_tick_index).unwrap_or(0));
    
    // What global tick corresponds to EST 09:35?
    // EST 09:35 = UTC 14:35 = 14:35:00 UTC on Jan 2, 2025
    // Jan 2 2025 00:00 UTC = 1735768800
    // 14:35 UTC = 14*3600 + 35*60 = 50700 + 2100 = 52800 sec
    // 14:35 UTC = 1735768800 + 52800 = 1735821600 ns
    // But that's 14:35, not 09:35. So 09:35 EST = 14:35 UTC. Wait that doesn't make sense either.
    
    // Let me recalculate:
    // UTC = EST + 5
    // So EST 09:35 = UTC 14:35
    // 09:35 EST on Jan 2 = 14:35 UTC on Jan 2
    // Jan 2 2025 00:00 UTC = 1735768800
    // 14:35 UTC = 14*3600 + 35*60 = 50700 + 2100 = 52800 sec
    // So 14:35 UTC = 1735768800 + 52800 = 1735821600
    // In ns: 1735821600 * 1e9 = 1735821600000000000
    
    let target_935_est_ns = 1735821600000000000u64;
    println!("\n09:35 EST should be at UTC ns = {}", target_935_est_ns);
    
    // Find which global tick has EST=09:35
    println!("\nSearching for EST=09:35 in the data...");
    let num_ticks = buffer_set.total_ticks();
    let mut found_935 = false;
    for gt in 0..num_ticks {
        if let Some((anchor, buffer)) = buffer_set.seek_to_global_tick(gt) {
            if let Ok(tick) = buffer.decode_anchor_at(anchor.byte_offset as usize) {
                let ts_sec = (tick.timestamp_ns / 1_000_000_000) as i64;
                let utc_dt = Utc.timestamp_opt(ts_sec, 0).unwrap();
                let est_offset = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
                let est_dt = est_offset.from_utc_datetime(&utc_dt.naive_utc());
                if est_dt.hour() == 9 && est_dt.minute() == 35 {
                    println!("FOUND EST 09:35: global_tick={}, buf={}, EST={:02}:{:02}:{:02}, ts={}", 
                        gt, anchor.buffer_idx, est_dt.hour(), est_dt.minute(), est_dt.second(), tick.timestamp_ns);
                    found_935 = true;
                    break;
                }
            }
        }
    }
    if !found_935 {
        println!("EST 09:35 NOT FOUND in data!");
    }
    
    // Also search for EST 09:30
    println!("\nSearching for EST=09:30...");
    let mut found_930 = false;
    for gt in 0..num_ticks {
        if let Some((anchor, buffer)) = buffer_set.seek_to_global_tick(gt) {
            if let Ok(tick) = buffer.decode_anchor_at(anchor.byte_offset as usize) {
                let ts_sec = (tick.timestamp_ns / 1_000_000_000) as i64;
                let utc_dt = Utc.timestamp_opt(ts_sec, 0).unwrap();
                let est_offset = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
                let est_dt = est_offset.from_utc_datetime(&utc_dt.naive_utc());
                if est_dt.hour() == 9 && est_dt.minute() == 30 {
                    println!("FOUND EST 09:30: global_tick={}, buf={}, EST={:02}:{:02}:{:02}, ts={}", 
                        gt, anchor.buffer_idx, est_dt.hour(), est_dt.minute(), est_dt.second(), tick.timestamp_ns);
                    found_930 = true;
                }
            }
        }
    }
    if !found_930 {
        println!("EST 09:30 NOT FOUND!");
    }
}

fn date_est_to_utc_start(date: NaiveDate) -> u64 {
    let prev_day = date - Duration::days(1);
    let est_start = chrono::NaiveDateTime::new(
        prev_day,
        chrono::NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
    );
    est_start.and_utc().timestamp_nanos_opt().unwrap() as u64
}
