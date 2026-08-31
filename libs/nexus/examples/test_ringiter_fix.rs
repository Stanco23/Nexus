use std::path::PathBuf;
use crate::buffer::ring_buffer::RingBuffer;
use crate::instrument::InstrumentId;

fn main() {
    let path = PathBuf::from("/home/shadowarch/Nexus/data/BTCUSDT_2025-01-02.tvc");
    let instrument = InstrumentId::new("BTCUSDT", "BINANCE");
    
    let rb = RingBuffer::open(&path, instrument).expect("failed to open");
    
    println!("File: {}, num_ticks={}, num_anchors={}, anchor_interval={}",
             path.display(), rb.num_ticks(), rb.num_anchors(), rb.anchor_interval());
    
    // Use actual RingIter via iter()
    println!("\nTesting RingIter via iter()...");
    let mut count = 0u64;
    let mut errors = Vec::new();
    
    for tick in rb.iter() {
        count += 1;
        
        if count <= 10 || count % 10000 == 0 {
            println!("tick {}: ts={}, price={}, side={}", 
                     tick.sequence, tick.timestamp_ns, tick.price_int, tick.side);
        }
        
        // Check for anomalies
        if tick.price_int <= 0 || tick.price_int > 200_000_000_000i64 {
            errors.push((count, tick.price_int, "price out of range"));
        }
        if tick.timestamp_ns < 1_700_000_000_000_000_000u64 || tick.timestamp_ns > 2_000_000_000_000_000_000u64 {
            errors.push((count, tick.timestamp_ns as i64, "timestamp out of range"));
        }
        
        if count >= 2000 {
            println!("Stopping at tick {} for verification", count);
            break;
        }
    }
    
    println!("\nTotal ticks iterated: {}", count);
    if !errors.is_empty() {
        println!("Errors found: {:?}", errors);
    } else {
        println!("No errors detected in first {} ticks", count);
    }
}
