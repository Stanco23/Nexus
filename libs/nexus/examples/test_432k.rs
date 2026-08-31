use std::path::PathBuf;
use crate::buffer::ring_buffer::RingBuffer;
use crate::instrument::InstrumentId;

fn main() {
    let path = PathBuf::from("/home/shadowarch/Nexus/data/BTCUSDT_2025-01-02.tvc");
    let instrument = InstrumentId::new("BTCUSDT", "BINANCE");
    
    let rb = RingBuffer::open(&path, instrument).expect("failed to open");
    println!("num_ticks={}, anchor_interval={}, num_anchors={}", 
             rb.num_ticks(), rb.anchor_interval(), rb.num_anchors());
    
    let mut iter = rb.iter();
    let mut count = 0;
    let mut last_seq: u32 = 0;
    let mut last_ts = 0u64;
    let mut gap_count = 0;
    
    while let Some(tick) = iter.next() {
        if count > 0 && tick.sequence != last_seq + 1 {
            gap_count += 1;
        }
        last_seq = tick.sequence;
        last_ts = tick.timestamp_ns;
        count += 1;
        if count >= 500000 { break; }
    }
    
    println!("iterated {} ticks, gap_count={}, last_seq={}, last_ts={}", 
             count, gap_count, last_seq, last_ts);
    println!("expected num_ticks={}", rb.num_ticks());
}
