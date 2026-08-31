use std::path::PathBuf;
use crate::buffer::ring_buffer::RingBuffer;
use crate::instrument::InstrumentId;

fn main() {
    let path = PathBuf::from("/home/shadowarch/Nexus/data/BTCUSDT_2025-01-02.tvc");
    let instrument = InstrumentId::new("BTCUSDT", "BINANCE");
    
    let rb = RingBuffer::open(&path, instrument).expect("failed to open");
    println!("num_ticks={}, anchor_interval={}, num_anchors={}", 
             rb.num_ticks(), rb.anchor_interval(), rb.num_anchors());
    println!("first_anchor_offset={}", rb.first_anchor_offset());
    
    let mut iter = rb.iter();
    let mut count = 0;
    let mut last_seq: u32 = 0;
    while let Some(tick) = iter.next() {
        if count < 5 || tick.sequence != last_seq + 1 {
            println!("iter[{}]: seq={}, ts={}", count, tick.sequence, tick.timestamp_ns);
        }
        last_seq = tick.sequence;
        count += 1;
        if count >= 2000 { break; }
    }
    println!("iterated {} ticks, last_seq={}", count, last_seq);
}
