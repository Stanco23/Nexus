use std::path::Path;
use crate::buffer::RingBuffer;
use crate::instrument::InstrumentId;

fn main() {
    let path = Path::new("/home/shadowarch/Nexus/data/BTCUSDT_2025-01-02.tvc");
    let instrument = InstrumentId::new("BTCUSDT", "BINANCE");
    
    let rb = RingBuffer::open(path, instrument).expect("failed to open");
    
    println!("num_ticks={}, num_anchors={}, anchor_interval={}", 
             rb.num_ticks(), rb.num_anchors(), rb.anchor_interval());
    
    // Use iter() which uses RingIter with our fix
    println!("\nIterating with RingIter...");
    let mut count = 0u64;
    let mut errors = Vec::new();
    
    for tick in rb.iter() {
        count += 1;
        
        // Check at key anchor points
        if tick.sequence == 1024 || tick.sequence == 2048 {
            println!("Reached anchor at tick {}, price={}", tick.sequence, tick.price_int);
        }
        
        // Verify tick is reasonable
        if tick.price_int <= 0 || tick.price_int > 200_000_000_000i64 {
            errors.push(format!("tick {}: bad price {}", tick.sequence, tick.price_int));
        }
        if count >= 3000 {
            println!("Stopped at tick {} (limit)", count);
            break;
        }
    }
    
    println!("\nTotal ticks iterated: {}", count);
    if errors.is_empty() {
        println!("SUCCESS: No errors in first {} ticks", count);
    } else {
        println!("ERRORS: {}", errors.len());
        for e in errors.iter().take(10) {
            println!("  {}", e);
        }
    }
}
