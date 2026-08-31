// Debug test to print anchor_index entries

use std::path::Path;
use nexus::buffer::RingBuffer;
use nexus::instrument::InstrumentId;

#[test]
fn debug_anchor_index() {
    let path = Path::new("/home/shadowarch/Nexus/data/BTCUSDT_2025-01-02.tvc");
    let instrument = InstrumentId::new("BTCUSDT", "BINANCE");
    
    let rb = RingBuffer::open(path, instrument).expect("failed to open");
    
    println!("num_anchors: {}", rb.num_anchors());
    println!("anchor_interval: {}", rb.anchor_interval());
    
    // Access anchor_index directly
    let anchors = rb.anchor_index();
    println!("anchor_index.len(): {}", anchors.len());
    
    for (i, entry) in anchors.iter().take(10).enumerate() {
        let tick_index = entry.tick_index;
        let byte_offset = entry.byte_offset;
        println!(
            "anchor_index[{}]: tick_index={}, byte_offset={}",
            i, tick_index, byte_offset
        );
    }
    
    // Now check seek_to_tick
    let result = rb.seek_to_tick(1024).expect("seek");
    println!("\nseek_to_tick(1024): offset={}, anchor_tick={}", result.0, result.1);
    
    let result2 = rb.seek_to_tick(2048).expect("seek");
    println!("seek_to_tick(2048): offset={}, anchor_tick={}", result2.0, result2.1);
}
