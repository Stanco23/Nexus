//! Debug: trace first ticks in buffer[1] (Jan 2)
use nexus::buffer::buffer_set::RingBufferSet;
use nexus::instrument::InstrumentId;
use std::path::PathBuf;

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
    
    // Find global tick range for buffer[1] (Jan 2, 1440 ticks at 0-1439)
    // buffer[0] = 300 ticks (Jan 1), buffer[1] = 1440 ticks (Jan 2)
    // So global ticks 300-1739 map to buffer[1]
    println!("Buffer 1 (Jan 2) global tick range: [300, 1739]");
    
    // Show first 10 ticks of buffer[1]
    println!("\nFirst 10 ticks of buffer[1]:");
    for global_tick in 300..310 {
        if let Some((anchor, buffer)) = buffer_set.seek_to_global_tick(global_tick) {
            let tick = buffer.decode_anchor_at(anchor.byte_offset as usize);
            if let Ok(tick) = tick {
                let ts = tick.timestamp_ns;
                let ts_sec = ts / 1_000_000_000;
                let utc_dt = chrono::DateTime::from_timestamp(ts_sec as i64, 0).unwrap();
                let est_dt = utc_dt - chrono::Duration::hours(5);
                println!("  seek({}): buf={}, local={}, ts={}, UTC={}, EST={}", 
                    global_tick, anchor.buffer_idx, anchor.local_tick_index, ts,
                    utc_dt.format("%H:%M:%S"), est_dt.format("%H:%M:%S"));
            }
        }
    }
    
    // Find target date range
    let target_date = chrono::NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
    let target_start = date_est_to_utc_start(target_date);
    let target_end = target_start + 24 * 60 * 60 * 1_000_000_000;
    println!("\nTarget (Jan 2 EST) UTC range: [{}, {})", target_start, target_end);
    
    // Count ticks in range using seek
    let num_ticks = buffer_set.total_ticks();
    let mut count = 0u64;
    for gt in 0..num_ticks {
        if let Some((anchor, buffer)) = buffer_set.seek_to_global_tick(gt) {
            if let Ok(tick) = buffer.decode_anchor_at(anchor.byte_offset as usize) {
                if tick.timestamp_ns >= target_start && tick.timestamp_ns < target_end {
                    count += 1;
                    if count <= 5 {
                        let ts = tick.timestamp_ns;
                        let ts_sec = ts / 1_000_000_000;
                        let utc_dt = chrono::DateTime::from_timestamp(ts_sec as i64, 0).unwrap();
                        let est_dt = utc_dt - chrono::Duration::hours(5);
                        println!("  in_range[{}]: global_tick={}, buf={}, ts={}, EST={}", 
                            count, gt, anchor.buffer_idx, ts, est_dt.format("%H:%M:%S"));
                    }
                }
            }
        }
    }
    println!("\nTotal ticks in Jan 2 EST range: {}", count);
}

fn date_est_to_utc_start(date: chrono::NaiveDate) -> u64 {
    let prev_day = date - chrono::Duration::days(1);
    let est_start = chrono::NaiveDateTime::new(
        prev_day,
        chrono::NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
    );
    est_start.and_utc().timestamp_nanos_opt().unwrap() as u64
}
