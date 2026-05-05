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
    let (buffer, offset, tick_idx, anchor_slot) = buffer_set.iter_state_from_global_tick(300).expect("no state");
    
    // Decode anchors at key positions
    let anchor_328 = buffer.decode_anchor_at(7016).unwrap();
    let anchor_330 = buffer.decode_anchor_at(7058).unwrap();
    let anchor_332 = buffer.decode_anchor_at(7100).unwrap();
    
    println!("Anchors:");
    println!("  328 @ 7016: ts={}, price={:.2}", anchor_328.timestamp_ns, anchor_328.price_int as f64 / 1e9_f64);
    println!("  330 @ 7058: ts={}, price={:.2}", anchor_330.timestamp_ns, anchor_330.price_int as f64 / 1e9_f64);
    println!("  332 @ 7100: ts={}, price={:.2}", anchor_332.timestamp_ns, anchor_332.price_int as f64 / 1e9_f64);
    
    // Decode deltas at key positions
    println!("\nDeltas:");
    match buffer.decode_delta_at(7046, &anchor_328) {
        Ok((t, c)) => println!("  329 @ 7046: ts={}, price={:.2}, consumed={}", t.timestamp_ns, t.price_int as f64 / 1e9_f64, c),
        Err(e) => println!("  329 @ 7046: ERR {:?}", e),
    }
    match buffer.decode_delta_at(7088, &anchor_330) {
        Ok((t, c)) => println!("  331 @ 7088: ts={}, price={:.2}, consumed={}", t.timestamp_ns, t.price_int as f64 / 1e9_f64, c),
        Err(e) => println!("  331 @ 7088: ERR {:?}", e),
    }
    match buffer.decode_delta_at(7130, &anchor_332) {
        Ok((t, c)) => println!("  333 @ 7130: ts={}, price={:.2}, consumed={}", t.timestamp_ns, t.price_int as f64 / 1e9_f64, c),
        Err(e) => println!("  333 @ 7130: ERR {:?}", e),
    }
    
    // Now manually trace RingIter iterations 28-35
    println!("\n=== Manual RingIter trace (iterations 28-35) ===");
    
    let ai = buffer.anchor_index();
    let ai_ptr = ai.as_ptr();
    
    let mut current_offset = offset;
    let mut current_tick = tick_idx;
    let mut last_tick = buffer.decode_anchor_at(offset).unwrap();
    let mut anchor_slot_state = anchor_slot;
    let mut started = false;
    
    for iter in 0..35 {
        // Simulate RingIter behavior
        if !started {
            started = true;
            current_offset += 30;
            current_tick += 1;
            anchor_slot_state = 1;
            println!("Iter {}: offset={}, tick={}, slot={} [FIRST anchor]", 
                iter, current_offset, current_tick, anchor_slot_state);
            continue;
        }
        
        let tick_index_at_slot = unsafe { (*ai_ptr.add(anchor_slot_state)).tick_index };
        
        if current_tick == tick_index_at_slot as u64 {
            // Anchor decode
            match buffer.decode_anchor_at(current_offset) {
                Ok(tick) => {
                    let side_ok = tick.side <= 1;
                    let ts_ok = tick.timestamp_ns >= last_tick.timestamp_ns;
                    println!("Iter {}: offset={}, tick={}, slot={} [ANCHOR ts={} price={:.2}]", 
                        iter, current_offset, current_tick, anchor_slot_state, 
                        tick.timestamp_ns, tick.price_int as f64 / 1e9_f64);
                    if side_ok && ts_ok {
                        last_tick = tick;
                        current_offset += 30;
                        current_tick += 1;
                        anchor_slot_state += 1;
                    } else {
                        println!("  -> Invalid anchor, would fall through to delta");
                    }
                }
                Err(e) => {
                    println!("Iter {}: offset={}, tick={}, slot={} [ERR: {:?}]", 
                        iter, current_offset, current_tick, anchor_slot_state, e);
                    break;
                }
            }
        } else {
            // Delta decode
            match buffer.decode_delta_at(current_offset, &last_tick) {
                Ok((tick, consumed)) => {
                    println!("Iter {}: offset={}, tick={}, slot={} [DELTA ts={} price={:.2} consumed={}]", 
                        iter, current_offset, current_tick, anchor_slot_state,
                        tick.timestamp_ns, tick.price_int as f64 / 1e9_f64, consumed);
                    last_tick = tick;
                    current_offset += consumed;
                    current_tick += 1;
                }
                Err(e) => {
                    println!("Iter {}: offset={}, tick={}, slot={} [DELTA_ERR: {:?}]", 
                        iter, current_offset, current_tick, anchor_slot_state, e);
                    break;
                }
            }
        }
        
        if iter >= 34 { break; }
    }
}
